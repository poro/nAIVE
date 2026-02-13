use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wgpu::util::DeviceExt;

use crate::components::MeshHandle;

#[derive(Debug)]
pub enum MeshError {
    IoError(String),
    GltfError(gltf::Error),
    NoMeshes,
    NoPrimitives,
    NoPositions,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "Mesh IO error: {}", msg),
            Self::GltfError(e) => write!(f, "glTF error: {}", e),
            Self::NoMeshes => write!(f, "glTF file contains no meshes"),
            Self::NoPrimitives => write!(f, "glTF mesh has no primitives"),
            Self::NoPositions => write!(f, "glTF primitive has no position data"),
        }
    }
}

impl From<gltf::Error> for MeshError {
    fn from(e: gltf::Error) -> Self {
        Self::GltfError(e)
    }
}

/// 3D vertex for mesh rendering.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex3D {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tex_coords: [f32; 2],
}

impl Vertex3D {
    const ATTRIBS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex3D>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// A loaded GPU mesh.
pub struct GpuMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
}

/// Cache of loaded meshes, keyed by file path.
pub struct MeshCache {
    meshes: Vec<GpuMesh>,
    path_to_handle: HashMap<PathBuf, MeshHandle>,
}

impl MeshCache {
    pub fn new() -> Self {
        Self {
            meshes: Vec::new(),
            path_to_handle: HashMap::new(),
        }
    }

    pub fn get_or_load(
        &mut self,
        device: &wgpu::Device,
        project_root: &Path,
        mesh_path: &str,
    ) -> Result<MeshHandle, MeshError> {
        let key = PathBuf::from(mesh_path);
        if let Some(&handle) = self.path_to_handle.get(&key) {
            return Ok(handle);
        }

        let gpu_mesh = load_gltf(device, project_root, mesh_path)?;
        let handle = MeshHandle(self.meshes.len());
        self.meshes.push(gpu_mesh);
        self.path_to_handle.insert(key, handle);
        tracing::info!("Loaded mesh: {}", mesh_path);
        Ok(handle)
    }

    pub fn get(&self, handle: MeshHandle) -> &GpuMesh {
        &self.meshes[handle.0]
    }
}

/// Load a glTF file and create GPU buffers.
fn load_gltf(
    device: &wgpu::Device,
    project_root: &Path,
    mesh_path: &str,
) -> Result<GpuMesh, MeshError> {
    let full_path = project_root.join(mesh_path);

    // If the file doesn't exist, generate a procedural cube
    if !full_path.exists() {
        tracing::warn!(
            "Mesh file not found: {:?}, using procedural cube",
            full_path
        );
        return Ok(create_procedural_cube(device));
    }

    let (document, buffers, _images) = gltf::import(&full_path)?;

    let mesh = document.meshes().next().ok_or(MeshError::NoMeshes)?;
    let primitive = mesh.primitives().next().ok_or(MeshError::NoPrimitives)?;

    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or(MeshError::NoPositions)?
        .collect();

    let normals: Vec<[f32; 3]> = reader
        .read_normals()
        .map(|n| n.collect())
        .unwrap_or_else(|| generate_flat_normals(&positions));

    let tex_coords: Vec<[f32; 2]> = reader
        .read_tex_coords(0)
        .map(|t| t.into_f32().collect())
        .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

    let vertices: Vec<Vertex3D> = positions
        .iter()
        .enumerate()
        .map(|(i, pos)| Vertex3D {
            position: *pos,
            normal: normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
            tex_coords: tex_coords.get(i).copied().unwrap_or([0.0, 0.0]),
        })
        .collect();

    let indices: Vec<u32> = if let Some(read_indices) = reader.read_indices() {
        read_indices.into_u32().collect()
    } else {
        // Generate sequential indices if none provided
        (0..vertices.len() as u32).collect()
    };

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Mesh VB: {}", mesh_path)),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Mesh IB: {}", mesh_path)),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    Ok(GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
    })
}

/// Generate flat normals from triangle positions (every 3 vertices).
fn generate_flat_normals(positions: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0f32, 1.0, 0.0]; positions.len()];
    for chunk in (0..positions.len()).step_by(3) {
        if chunk + 2 < positions.len() {
            let v0 = glam::Vec3::from(positions[chunk]);
            let v1 = glam::Vec3::from(positions[chunk + 1]);
            let v2 = glam::Vec3::from(positions[chunk + 2]);
            let normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
            let n = normal.to_array();
            normals[chunk] = n;
            normals[chunk + 1] = n;
            normals[chunk + 2] = n;
        }
    }
    normals
}

/// Create a procedural unit cube as fallback.
fn create_procedural_cube(device: &wgpu::Device) -> GpuMesh {
    #[rustfmt::skip]
    let vertices: Vec<Vertex3D> = vec![
        // Front face (z = 0.5)
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 0.0] },
        // Back face (z = -0.5)
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 0.0] },
        // Top face (y = 0.5)
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 0.0] },
        // Bottom face (y = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 0.0] },
        // Right face (x = 0.5)
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 0.0] },
        // Left face (x = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 1.0] },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 0.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 0.0] },
    ];

    #[rustfmt::skip]
    let indices: Vec<u32> = vec![
        0,  1,  2,  0,  2,  3,   // front
        4,  5,  6,  4,  6,  7,   // back
        8,  9,  10, 8,  10, 11,  // top
        12, 13, 14, 12, 14, 15,  // bottom
        16, 17, 18, 16, 18, 19,  // right
        20, 21, 22, 20, 22, 23,  // left
    ];

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Cube VB"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Cube IB"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
    }
}
