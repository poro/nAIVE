use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use crate::components::MaterialHandle;

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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
        _device: &wgpu::Device,
        project_root: &Path,
        material_path: &str,
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
        let gpu_material = GpuMaterial { uniform };

        let handle = MaterialHandle(self.materials.len());
        self.materials.push(gpu_material);
        self.path_to_handle.insert(key, handle);
        tracing::info!("Loaded material: {}", material_path);
        Ok(handle)
    }

    pub fn get(&self, handle: MaterialHandle) -> &GpuMaterial {
        &self.materials[handle.0]
    }

    #[allow(dead_code)]
    pub fn ensure_default(&mut self, device: &wgpu::Device, project_root: &Path) -> MaterialHandle {
        if let Some(handle) = self.default_handle {
            return handle;
        }
        let handle = self
            .get_or_load(device, project_root, "assets/materials/default.yaml")
            .unwrap_or_else(|_| {
                // Create a hardcoded default
                let uniform = MaterialUniform::from_properties(&MaterialProperties::default());
                let h = MaterialHandle(self.materials.len());
                self.materials.push(GpuMaterial { uniform });
                h
            });
        self.default_handle = Some(handle);
        handle
    }
}
