use glam::Mat4;
use naive_core::animation::{AnimationClip, JointTransform, Skeleton, MAX_JOINTS};
use crate::components::{Animator, SkeletonHandle};
use crate::mesh::MeshCache;

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
    /// Per-entity scratch space for joint transforms during sampling.
    scratch_transforms: Vec<JointTransform>,
}

impl AnimationSystem {
    pub fn new() -> Self {
        Self {
            skeleton_store: SkeletonStore::new(),
            scratch_transforms: Vec::new(),
        }
    }

    /// Register skeletons from newly loaded meshes in the cache.
    pub fn register_from_mesh_cache(&mut self, mesh_cache: &mut MeshCache) -> Vec<(usize, SkeletonHandle)> {
        let mut registered = Vec::new();
        for (mesh_idx, skin_data) in mesh_cache.take_skin_data() {
            let handle = self.skeleton_store.add(skin_data.skeleton, skin_data.clips);
            registered.push((mesh_idx, handle));
            tracing::info!("Registered skeleton handle {:?} for mesh {}", handle, mesh_idx);
        }
        registered
    }

    /// Tick a single entity's animation and compute its bone matrix palette.
    pub fn tick_entity(
        &mut self,
        animator: &mut Animator,
        dt: f32,
    ) -> BoneMatrixPalette {
        let mut palette = BoneMatrixPalette::default();

        let skeleton = match self.skeleton_store.get_skeleton(animator.skeleton_handle) {
            Some(s) => s,
            None => return palette,
        };

        // Resolve active clip if needed
        if animator.controller.active_clip_index.is_none() {
            let clip_name = animator.controller.current_state.clip_name();
            animator.controller.active_clip_index =
                self.skeleton_store.find_clip(animator.skeleton_handle, clip_name)
                    .or_else(|| {
                        // Fallback: try first clip
                        if self.skeleton_store.clip_count(animator.skeleton_handle) > 0 {
                            Some(0)
                        } else {
                            None
                        }
                    });
        }

        let clip_index = match animator.controller.active_clip_index {
            Some(idx) => idx,
            None => return palette,
        };

        let clips = match self.skeleton_store.get_clips(animator.skeleton_handle) {
            Some(c) => c,
            None => return palette,
        };

        let clip = match clips.get(clip_index) {
            Some(c) => c,
            None => return palette,
        };

        // Advance time
        let time = animator.controller.tick(dt, clip.duration);

        // Prepare scratch transforms from skeleton rest pose
        let joint_count = skeleton.joints.len().min(MAX_JOINTS);
        self.scratch_transforms.clear();
        self.scratch_transforms.extend(
            skeleton.joints.iter().take(joint_count).map(|j| j.local_transform)
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
