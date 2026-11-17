use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::components::MaterialHandle;
use crate::texture_cache::{TextureCache, TextureHandle};
pub use naive_sim::assets::{AssetError as MaterialError, MaterialFile, MaterialProperties};

/// GPU-side material uniform data.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub _pad: [f32; 2],
    pub emission: [f32; 4],
}

impl MaterialUniform {
    pub fn from_properties(props: &MaterialProperties) -> Self {
        Self {
            base_color: [
                props.base_color[0],
                props.base_color[1],
                props.base_color[2],
                1.0,
            ],
            roughness: props.roughness,
            metallic: props.metallic,
            _pad: [0.0; 2],
            emission: [props.emission[0], props.emission[1], props.emission[2], 0.0],
        }
    }
}

/// A loaded GPU material.
pub struct GpuMaterial {
    pub uniform: MaterialUniform,
    /// Albedo texture loaded from material's `albedo_map` field.
    pub albedo_texture: Option<TextureHandle>,
    /// Normal map loaded from material's `normal_map` field.
    pub normal_texture: Option<TextureHandle>,
}

/// Cache of loaded materials.
pub struct MaterialCache {
    materials: Vec<GpuMaterial>,
    path_to_handle: HashMap<PathBuf, MaterialHandle>,
    default_handle: Option<MaterialHandle>,
}

impl MaterialCache {
    pub fn new() -> Self {
        Self {
            materials: Vec::new(),
            path_to_handle: HashMap::new(),
            default_handle: None,
        }
    }

    pub fn get_or_load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        project_root: &Path,
        material_path: &str,
        mut texture_cache: Option<&mut TextureCache>,
        texture_layout: Option<&wgpu::BindGroupLayout>,
    ) -> Result<MaterialHandle, MaterialError> {
        let key = PathBuf::from(material_path);
        if let Some(&handle) = self.path_to_handle.get(&key) {
            return Ok(handle);
        }

        let mat_file = naive_sim::assets::load_material(project_root, material_path)?;

        let uniform = MaterialUniform::from_properties(&mat_file.properties);
        tracing::warn!(
            "Material '{}' base_color=[{:.3},{:.3},{:.3},{:.3}] roughness={:.2}",
            material_path,
            uniform.base_color[0],
            uniform.base_color[1],
            uniform.base_color[2],
            uniform.base_color[3],
            uniform.roughness
        );

        // The shared loader already validates referenced maps even when this
        // caller does not supply GPU texture resources.
        let albedo_texture = if let (Some(path), Some(cache), Some(layout)) = (
            &mat_file.properties.albedo_map,
            texture_cache.as_deref_mut(),
            texture_layout,
        ) {
            Some(
                cache
                    .get_or_load(device, queue, layout, project_root, path)
                    .map_err(MaterialError)?,
            )
        } else {
            None
        };
        let normal_texture = if let (Some(path), Some(cache), Some(layout)) = (
            &mat_file.properties.normal_map,
            texture_cache,
            texture_layout,
        ) {
            Some(
                cache
                    .get_or_load(device, queue, layout, project_root, path)
                    .map_err(MaterialError)?,
            )
        } else {
            None
        };

        let gpu_material = GpuMaterial {
            uniform,
            albedo_texture,
            normal_texture,
        };

        let handle = MaterialHandle(self.materials.len());
        self.materials.push(gpu_material);
        self.path_to_handle.insert(key, handle);
        tracing::info!("Loaded material: {}", material_path);
        Ok(handle)
    }

    pub fn get(&self, handle: MaterialHandle) -> &GpuMaterial {
        &self.materials[handle.0]
    }

    pub fn get_mut(&mut self, handle: MaterialHandle) -> &mut GpuMaterial {
        &mut self.materials[handle.0]
    }

    /// Get the path/name for a material handle (reverse lookup for serialization).
    pub fn name_for_handle(&self, handle: MaterialHandle) -> Option<String> {
        for (path, &h) in &self.path_to_handle {
            if h.0 == handle.0 {
                return Some(path.to_string_lossy().to_string());
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn ensure_default(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        project_root: &Path,
    ) -> MaterialHandle {
        if let Some(handle) = self.default_handle {
            return handle;
        }
        let handle = self
            .get_or_load(
                device,
                queue,
                project_root,
                "procedural:default",
                None,
                None,
            )
            .expect("the explicit engine default has no filesystem dependencies");
        self.default_handle = Some(handle);
        handle
    }
}

impl Default for MaterialCache {
    fn default() -> Self {
        Self::new()
    }
}
