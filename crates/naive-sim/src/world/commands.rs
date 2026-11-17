//! Deferred entity commands shared by native and headless simulation.

use crate::scene::EntityDef;

/// Deferred pool operation.
pub enum PoolOp {
    Release(String),
}

/// Deferred entity commands from Lua scripts, processed each frame.
#[derive(Default)]
pub struct EntityCommandQueue {
    pub spawns: Vec<SpawnCommand>,
    pub destroys: Vec<String>,
    pub scale_updates: Vec<(String, [f32; 3])>,
    pub visibility_updates: Vec<(String, bool)>,
    pub projectile_spawns: Vec<ProjectileSpawnCommand>,
    pub projectile_counter: u64,
    pub dynamic_spawns: Vec<DynamicSpawnCommand>,
    pub dynamic_counter: u64,
    pub pool_ops: Vec<PoolOp>,
    pub pending_scene_load: Option<String>,
    pub texture_swaps: Vec<TextureSwapCommand>,
    pub mesh_creates: Vec<MeshCreateCommand>,
}

/// Deferred texture swap: change an entity's albedo or normal map at runtime.
pub struct TextureSwapCommand {
    pub entity_id: String,
    pub slot: String,
    pub texture_path: String,
}

/// Deferred mesh creation from Lua vertex/index data. Both runners apply
/// these before the same frame's spawns, so a script can create a mesh and
/// spawn it in one call.
pub struct MeshCreateCommand {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshCreateCommand {
    /// The validated CPU mesh the renderer and physics share.
    pub fn cpu_mesh(&self) -> Result<crate::assets::CpuMesh, crate::assets::AssetError> {
        crate::assets::runtime_mesh(&self.positions, &self.normals, &self.uvs, &self.indices)
    }
}

pub struct SpawnCommand {
    pub id: String,
    pub mesh: String,
    pub material: String,
    pub position: [f32; 3],
    pub scale: [f32; 3],
    pub collider: Option<SpawnColliderSpec>,
    pub rigid_body: Option<String>,
    pub script: Option<String>,
}

impl SpawnCommand {
    /// The same authored representation feeds native and headless spawning.
    pub fn entity_def(&self) -> EntityDef {
        EntityDef {
            id: self.id.clone(),
            tags: Vec::new(),
            extends: None,
            components: crate::scene::ComponentMap {
                transform: Some(crate::scene::TransformDef {
                    position: self.position,
                    rotation: [0.0; 3],
                    scale: self.scale,
                }),
                mesh_renderer: Some(crate::scene::MeshRendererDef {
                    mesh: self.mesh.clone(),
                    material: self.material.clone(),
                    cast_shadows: true,
                    receive_shadows: true,
                }),
                collider: self
                    .collider
                    .as_ref()
                    .map(|collider| crate::scene::ColliderDef {
                        shape: collider.shape.clone(),
                        half_extents: collider.half_extents,
                        radius: collider.radius,
                        half_height: None,
                        is_trigger: collider.is_trigger,
                        restitution: collider.restitution,
                        friction: collider.friction,
                    }),
                rigid_body: self
                    .rigid_body
                    .as_ref()
                    .map(|body_type| crate::scene::RigidBodyDef {
                        body_type: body_type.clone(),
                        mass: 1.0,
                        ccd: false,
                    }),
                script: self.script.as_ref().map(|source| crate::scene::ScriptDef {
                    source: source.clone(),
                }),
                ..Default::default()
            },
        }
    }
}

/// Collider specification for runtime-spawned entities.
pub struct SpawnColliderSpec {
    pub shape: String,
    pub half_extents: Option<[f32; 3]>,
    pub radius: Option<f32>,
    pub is_trigger: bool,
    pub restitution: f32,
    pub friction: f32,
}

pub struct ProjectileSpawnCommand {
    pub id: String,
    pub mesh: String,
    pub material: String,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub speed: f32,
    pub damage: f32,
    pub lifetime: f32,
    pub gravity: bool,
    pub owner_id: String,
    pub scale: [f32; 3],
}

pub struct DynamicSpawnCommand {
    pub id: String,
    pub mesh: String,
    pub material: String,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub scale: [f32; 3],
    pub radius: f32,
    pub mass: f32,
    pub restitution: f32,
    pub friction: f32,
    pub lifetime: f32,
}

impl EntityCommandQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.spawns.clear();
        self.destroys.clear();
        self.scale_updates.clear();
        self.visibility_updates.clear();
        self.projectile_spawns.clear();
        self.dynamic_spawns.clear();
        self.pool_ops.clear();
        self.pending_scene_load = None;
        self.texture_swaps.clear();
        self.mesh_creates.clear();
    }
}
