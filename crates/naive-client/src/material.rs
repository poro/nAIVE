use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use crate::components::MaterialHandle;
use crate::texture_cache::{TextureCache, TextureHandle};

#[derive(Debug)]
pub enum MaterialError {
    IoError(std::io::Error),
    ParseError(serde_yaml::Error),
}

impl std::fmt::Display for MaterialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "Material IO error: {}", e),
            Self::ParseError(e) => write!(f, "Material parse error: {}", e),
        }
    }
}

/// Material YAML deserialization type.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MaterialFile {
    #[serde(default)]
    pub shader: String,
    #[serde(default)]
    pub properties: MaterialProperties,
    #[serde(default = "default_opaque")]
    pub blend_mode: String,
    #[serde(default = "default_back")]
    pub cull_mode: String,
}

fn default_opaque() -> String {
    "opaque".to_string()
}
fn default_back() -> String {
    "back".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MaterialProperties {
    #[serde(default = "default_base_color")]
    pub base_color: [f32; 3],
    #[serde(default = "default_roughness")]
    pub roughness: f32,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default)]
    pub emission: [f32; 3],
    // Texture paths are recorded but not loaded until Phase 3
    #[serde(default)]
    pub albedo_map: Option<String>,
    #[serde(default)]
    pub normal_map: Option<String>,
}

impl Default for MaterialProperties {
    fn default() -> Self {
        Self {
            base_color: default_base_color(),
            roughness: default_roughness(),
            metallic: 0.0,
            emission: [0.0; 3],
            albedo_map: None,
            normal_map: None,
        }
    }
}

fn default_base_color() -> [f32; 3] {
    [0.8, 0.8, 0.8]
}
fn default_roughness() -> f32 {
    0.5
}

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
            base_color: [props.base_color[0], props.base_color[1], props.base_color[2], 1.0],
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

        let full_path = project_root.join(material_path);

        let mat_file = if full_path.exists() {
            let contents = std::fs::read_to_string(&full_path).map_err(MaterialError::IoError)?;
            serde_yaml::from_str::<MaterialFile>(&contents).map_err(MaterialError::ParseError)?
        } else {
            tracing::warn!(
                "Material file not found: {:?}, using defaults",
                full_path
            );
            MaterialFile {
                shader: String::new(),
                properties: MaterialProperties::default(),
                blend_mode: default_opaque(),
                cull_mode: default_back(),
            }
        };

        let uniform = MaterialUniform::from_properties(&mat_file.properties);
        tracing::warn!(
            "Material '{}' base_color=[{:.3},{:.3},{:.3},{:.3}] roughness={:.2}",
            material_path,
            uniform.base_color[0], uniform.base_color[1], uniform.base_color[2], uniform.base_color[3],
            uniform.roughness
        );

        // Load textures if referenced
        let albedo_texture = if let (Some(albedo_path), Some(tex_cache), Some(tex_layout)) =
            (&mat_file.properties.albedo_map, texture_cache.as_deref_mut(), texture_layout)
        {
            match tex_cache.get_or_load(device, queue, tex_layout, project_root, albedo_path) {
                Ok(handle) => {
                    tracing::info!("Material '{}' loaded albedo_map: {}", material_path, albedo_path);
                    Some(handle)
                }
                Err(e) => {
                    tracing::warn!("Material '{}' failed to load albedo_map '{}': {}", material_path, albedo_path, e);
                    None
                }
            }
        } else {
            None
        };

        let normal_texture = if let (Some(normal_path), Some(tex_cache), Some(tex_layout)) =
            (&mat_file.properties.normal_map, texture_cache, texture_layout)
        {
            match tex_cache.get_or_load(device, queue, tex_layout, project_root, normal_path) {
                Ok(handle) => {
                    tracing::info!("Material '{}' loaded normal_map: {}", material_path, normal_path);
                    Some(handle)
                }
                Err(e) => {
                    tracing::warn!("Material '{}' failed to load normal_map '{}': {}", material_path, normal_path, e);
                    None
                }
            }
        } else {
            None
        };

        let gpu_material = GpuMaterial { uniform, albedo_texture, normal_texture };

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
    pub fn ensure_default(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, project_root: &Path) -> MaterialHandle {
        if let Some(handle) = self.default_handle {
            return handle;
        }
        let handle = self
            .get_or_load(device, queue, project_root, "assets/materials/default.yaml", None, None)
            .unwrap_or_else(|_| {
                // Create a hardcoded default
                let uniform = MaterialUniform::from_properties(&MaterialProperties::default());
                let h = MaterialHandle(self.materials.len());
                self.materials.push(GpuMaterial { uniform, albedo_texture: None, normal_texture: None });
                h
            });
        self.default_handle = Some(handle);
        handle
    }
}

impl Default for MaterialCache {
    fn default() -> Self {
        Self::new()
    }
}
