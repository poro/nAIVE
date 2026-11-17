use crate::components::{Animator, SkeletonHandle};
use crate::mesh::MeshCache;
use glam::Mat4;
use naive_core::animation::{AnimationClip, JointTransform, Skeleton, MAX_JOINTS};

/// Stores skeletons and their animation clips, indexed by SkeletonHandle.
pub struct SkeletonStore {
    skeletons: Vec<SkeletonEntry>,
}

struct SkeletonEntry {
    skeleton: Skeleton,
    clips: Vec<AnimationClip>,
    /// Maps clip name -> index for quick lookup.
    name_to_clip: std::collections::HashMap<String, usize>,
}

impl SkeletonStore {
    pub fn new() -> Self {
        Self {
            skeletons: Vec::new(),
        }
    }

    /// Register a skeleton and its clips. Returns the handle.
    pub fn add(&mut self, skeleton: Skeleton, clips: Vec<AnimationClip>) -> SkeletonHandle {
        let handle = SkeletonHandle(self.skeletons.len());
        let name_to_clip = clips
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        self.skeletons.push(SkeletonEntry {
            skeleton,
            clips,
            name_to_clip,
        });
        handle
    }

    pub fn get_skeleton(&self, handle: SkeletonHandle) -> Option<&Skeleton> {
        self.skeletons.get(handle.0).map(|e| &e.skeleton)
    }

    pub fn get_clips(&self, handle: SkeletonHandle) -> Option<&[AnimationClip]> {
        self.skeletons.get(handle.0).map(|e| e.clips.as_slice())
    }

    pub fn find_clip(&self, handle: SkeletonHandle, name: &str) -> Option<usize> {
        self.skeletons
            .get(handle.0)
            .and_then(|e| e.name_to_clip.get(name).copied())
    }

    pub fn clip_count(&self, handle: SkeletonHandle) -> usize {
        self.skeletons
            .get(handle.0)
            .map(|e| e.clips.len())
            .unwrap_or(0)
    }
}

impl Default for SkeletonStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-entity bone matrix palette for GPU upload.
/// Stored as column-major mat4x4 arrays for wgpu uniform buffer.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BoneMatrixPalette {
    /// Joint count + padding.
    pub joint_count: u32,
    pub has_skin: u32,
    pub _pad: [u32; 2],
    /// Bone matrices: MAX_JOINTS mat4x4 values.
    pub matrices: [[[f32; 4]; 4]; MAX_JOINTS],
}

impl Default for BoneMatrixPalette {
    fn default() -> Self {
        let mut palette = Self {
            joint_count: 0,
            has_skin: 0,
            _pad: [0; 2],
            matrices: [[[0.0; 4]; 4]; MAX_JOINTS],
        };
        // Initialize all matrices to identity
        for m in &mut palette.matrices {
            *m = Mat4::IDENTITY.to_cols_array_2d();
        }
        palette
    }
}

/// Animation system: ticks animation controllers, computes bone matrices.
pub struct AnimationSystem {
    pub skeleton_store: SkeletonStore,
    /// Cached meshes may be reused after their skin data has been consumed.
    mesh_skeletons: std::collections::HashMap<usize, SkeletonHandle>,
    /// Per-entity scratch space for joint transforms during sampling.
    scratch_transforms: Vec<JointTransform>,
}

impl AnimationSystem {
    pub fn new() -> Self {
        Self {
            skeleton_store: SkeletonStore::new(),
            mesh_skeletons: std::collections::HashMap::new(),
            scratch_transforms: Vec::new(),
        }
    }

    /// Register skeletons from newly loaded meshes in the cache.
    pub fn register_from_mesh_cache(
        &mut self,
        mesh_cache: &mut MeshCache,
    ) -> Vec<(usize, SkeletonHandle)> {
        let mut registered = Vec::new();
        for (mesh_idx, skin_data) in mesh_cache.take_skin_data() {
            let handle = self.skeleton_store.add(skin_data.skeleton, skin_data.clips);
            self.mesh_skeletons.insert(mesh_idx, handle);
            registered.push((mesh_idx, handle));
            tracing::info!(
                "Registered skeleton handle {:?} for mesh {}",
                handle,
                mesh_idx
            );
        }
        registered
    }

    /// Attach animation to new entities even when their mesh was loaded earlier.
    /// Authored playback settings are applied once, leaving live controllers alone.
    pub fn attach_animators(&self, scene_world: &mut crate::world::SceneWorld) {
        let pending: Vec<_> = scene_world
            .world
            .query::<(hecs::Entity, &crate::components::MeshRenderer)>()
            .iter()
            .filter_map(|(entity, mesh)| {
                self.mesh_skeletons
                    .get(&mesh.mesh_handle.0)
                    .map(|&skeleton| (entity, skeleton))
            })
            .collect();
        for (entity, skeleton_handle) in pending {
            if scene_world.world.get::<&Animator>(entity).is_ok() {
                continue;
            }
            let controller = {
                let authored = scene_world
                    .world
                    .get::<&crate::world::SceneEntityDefinition>(entity)
                    .ok();
                animation_controller(
                    authored
                        .as_ref()
                        .and_then(|definition| definition.0.components.animator.as_ref()),
                )
            };
            let _ = scene_world.world.insert_one(
                entity,
                Animator {
                    skeleton_handle,
                    controller,
                },
            );
            let _ = scene_world.world.insert_one(entity, skeleton_handle);
        }
    }

    /// Tick a single entity's animation and compute its bone matrix palette.
    pub fn tick_entity(&mut self, animator: &mut Animator, dt: f32) -> BoneMatrixPalette {
        let mut palette = BoneMatrixPalette::default();

        let skeleton = match self.skeleton_store.get_skeleton(animator.skeleton_handle) {
            Some(s) => s,
            None => return palette,
        };

        let clips = match self.skeleton_store.get_clips(animator.skeleton_handle) {
            Some(clips) => clips,
            None => return palette,
        };
        let Some(clip_index) =
            naive_sim::animation::advance_controller(&mut animator.controller, clips, dt)
        else {
            return palette;
        };
        let clip = &clips[clip_index];
        let time = animator.controller.current_time;

        // Prepare scratch transforms from skeleton rest pose
        let joint_count = skeleton.joints.len().min(MAX_JOINTS);
        self.scratch_transforms.clear();
        self.scratch_transforms.extend(
            skeleton
                .joints
                .iter()
                .take(joint_count)
                .map(|j| j.local_transform),
        );

        // Sample animation into scratch transforms
        clip.sample(time, &mut self.scratch_transforms);

        // Compute final skin matrices
        let skin_matrices = skeleton.compute_skin_matrices(&self.scratch_transforms);

        palette.joint_count = joint_count as u32;
        palette.has_skin = 1;
        for (i, mat) in skin_matrices.iter().take(joint_count).enumerate() {
            palette.matrices[i] = mat.to_cols_array_2d();
        }

        palette
    }
}

fn animation_controller(
    definition: Option<&naive_core::scene::AnimatorDef>,
) -> naive_core::animation::AnimationController {
    naive_sim::animation::controller_from_definition(definition)
}

impl Default for AnimationSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{MaterialHandle, MeshHandle, MeshRenderer};
    use crate::scene::{AnimatorDef, ComponentMap, EntityDef};
    use crate::world::{SceneEntityDefinition, SceneWorld};

    #[test]
    fn cached_mesh_spawns_keep_authored_animation_and_live_progress() {
        let mut system = AnimationSystem::new();
        let skeleton = system
            .skeleton_store
            .add(Skeleton::new(Vec::new()), Vec::new());
        system.mesh_skeletons.insert(3, skeleton);
        let mut world = SceneWorld::new();
        let authored = EntityDef {
            id: "first".into(),
            tags: Vec::new(),
            extends: None,
            components: ComponentMap {
                animator: Some(AnimatorDef {
                    state: "dance".into(),
                    speed: 2.5,
                    looping: false,
                }),
                ..Default::default()
            },
        };
        let first = world.world.spawn((
            MeshRenderer {
                mesh_handle: MeshHandle(3),
                material_handle: MaterialHandle(0),
            },
            SceneEntityDefinition(authored.clone()),
        ));
        system.attach_animators(&mut world);
        {
            let mut animator = world.world.get::<&mut Animator>(first).unwrap();
            assert_eq!(animator.controller.current_state.clip_name(), "dance");
            assert_eq!(animator.controller.speed, 2.5);
            assert!(!animator.controller.looping);
            animator.controller.current_time = 7.0;
        }
        // No newly registered skin data: a second instance still gets a controller.
        let second = world.world.spawn((
            MeshRenderer {
                mesh_handle: MeshHandle(3),
                material_handle: MaterialHandle(0),
            },
            SceneEntityDefinition(authored),
        ));
        system.attach_animators(&mut world);
        assert_eq!(
            world
                .world
                .get::<&Animator>(first)
                .unwrap()
                .controller
                .current_time,
            7.0
        );
        let animator = world.world.get::<&Animator>(second).unwrap();
        assert_eq!(animator.controller.current_state.clip_name(), "dance");
        assert_eq!(animator.controller.speed, 2.5);
        assert!(!animator.controller.looping);
        assert_eq!(animator.skeleton_handle, skeleton);
    }
}
