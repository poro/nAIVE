use glam::{Mat4, Quat, Vec3};

/// Transform component. Present on every entity.
#[derive(Debug, Clone)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    pub world_matrix: Mat4,
    pub parent: Option<hecs::Entity>,
    pub dirty: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            world_matrix: Mat4::IDENTITY,
            parent: None,
            dirty: true,
        }
    }
}

/// Identifies this entity as a mesh to render.
#[derive(Debug, Clone)]
pub struct MeshRenderer {
    pub mesh_handle: MeshHandle,
    pub material_handle: MaterialHandle,
}

/// Newtype handle into the mesh cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshHandle(pub usize);

/// Newtype handle into the material cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterialHandle(pub usize);

/// Newtype handle into the splat cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SplatHandle(pub usize);

/// Identifies this entity as a Gaussian splat cloud to render.
#[derive(Debug, Clone)]
pub struct GaussianSplat {
    pub splat_handle: SplatHandle,
}

/// Camera component.
#[derive(Debug, Clone)]
pub struct Camera {
    pub fov_degrees: f32,
    pub near: f32,
    pub far: f32,
    pub role: CameraRole,
    pub aspect_ratio: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CameraRole {
    Main,
    Other(String),
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov_degrees: 75.0,
            near: 0.1,
            far: 100.0,
            role: CameraRole::Main,
            aspect_ratio: 16.0 / 9.0,
        }
    }
}

/// Point light component.
#[derive(Debug, Clone)]
pub struct PointLight {
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
}

/// Tag component storing the entity's YAML id string.
#[derive(Debug, Clone)]
pub struct EntityId(pub String);

/// Tag component for searchable tags.
#[derive(Debug, Clone)]
pub struct Tags(pub Vec<String>);
