//! Native GPU-cache adapter for the shared entity asset preparation contract.

use std::path::Path;

use crate::components::{MaterialHandle, MeshHandle, SplatHandle};
use crate::material::MaterialCache;
use crate::mesh::{MeshCache, TextureResources};
use crate::physics::PhysicsTrimesh;
use crate::splat::SplatCache;
use crate::texture_cache::TextureCache;

use super::SpawnAssetResolver;

pub(super) struct NativeAssetResolver<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub project_root: &'a Path,
    pub meshes: &'a mut MeshCache,
    pub materials: &'a mut MaterialCache,
    pub splats: Option<&'a mut SplatCache>,
    pub textures: Option<&'a mut TextureCache>,
    pub texture_resources: Option<&'a TextureResources>,
}

impl SpawnAssetResolver for NativeAssetResolver<'_> {
    fn mesh(&mut self, path: &str) -> Result<MeshHandle, String> {
        self.meshes
            .get_or_load(
                self.device,
                self.queue,
                self.project_root,
                path,
                self.texture_resources,
            )
            .map_err(|error| error.to_string())
    }

    fn material(&mut self, path: &str) -> Result<MaterialHandle, String> {
        self.materials
            .get_or_load(
                self.device,
                self.queue,
                self.project_root,
                path,
                self.textures.as_deref_mut(),
                self.texture_resources
                    .map(|resources| &resources.bind_group_layout),
            )
            .map_err(|error| error.to_string())
    }

    fn splat(&mut self, path: &str) -> Result<SplatHandle, String> {
        self.splats
            .as_deref_mut()
            .ok_or("splat cache is unavailable for this spawn")?
            .get_or_load(self.device, self.project_root, path)
            .map_err(|error| error.to_string())
    }

    fn trimesh(&mut self, mesh: MeshHandle, scale: glam::Vec3) -> Result<PhysicsTrimesh, String> {
        self.meshes
            .get_physics_trimesh(mesh, scale)
            .ok_or_else(|| "resolved mesh has no CPU triangle geometry".into())
    }
}
