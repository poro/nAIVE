//! Validated CPU assets used by the renderer and headless tests.
mod material;
mod mesh;
mod splat;
pub use material::*;
pub use mesh::*;
use naive_core::components::{MaterialHandle, MeshHandle, SplatHandle};
pub use splat::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct AssetError(pub String);
impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AssetError {}
impl From<std::io::Error> for AssetError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

pub struct AssetCatalog {
    root: PathBuf,
    meshes: Vec<CpuMesh>,
    mesh_paths: HashMap<String, MeshHandle>,
    materials: Vec<MaterialFile>,
    material_paths: HashMap<String, MaterialHandle>,
    splats: Vec<SplatData>,
    splat_paths: HashMap<String, SplatHandle>,
}
impl AssetCatalog {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.into(),
            meshes: vec![],
            mesh_paths: HashMap::new(),
            materials: vec![],
            material_paths: HashMap::new(),
            splats: vec![],
            splat_paths: HashMap::new(),
        }
    }
    pub fn mesh(&mut self, path: &str) -> Result<MeshHandle, AssetError> {
        if let Some(h) = self.mesh_paths.get(path) {
            return Ok(*h);
        }
        let data = load_mesh(&self.root, path)?;
        let handle = MeshHandle(self.meshes.len());
        self.meshes.push(data);
        self.mesh_paths.insert(path.into(), handle);
        Ok(handle)
    }
    pub fn mesh_data(&self, h: MeshHandle) -> Option<&CpuMesh> {
        self.meshes.get(h.0)
    }
    pub fn material(&mut self, path: &str) -> Result<MaterialHandle, AssetError> {
        if let Some(h) = self.material_paths.get(path) {
            return Ok(*h);
        }
        let data = load_material(&self.root, path)?;
        let handle = MaterialHandle(self.materials.len());
        self.materials.push(data);
        self.material_paths.insert(path.into(), handle);
        Ok(handle)
    }
    /// Validate and retain the exact CPU splat data used by native uploads.
    pub fn validate_splat(&mut self, path: &str) -> Result<SplatHandle, AssetError> {
        if let Some(&handle) = self.splat_paths.get(path) {
            return Ok(handle);
        }
        let data = load_splat(&self.root, path)?;
        let handle = SplatHandle(self.splats.len());
        self.splats.push(data);
        self.splat_paths.insert(path.into(), handle);
        Ok(handle)
    }
    pub fn splat_data(&self, handle: SplatHandle) -> Option<&SplatData> {
        self.splats.get(handle.0)
    }
}
