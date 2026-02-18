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
    pub color: [f32; 4],
}

impl Vertex3D {
    const ATTRIBS: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4];

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

        // Support procedural mesh specifiers: "procedural:sphere", "procedural:cube"
        let gpu_mesh = if let Some(shape) = mesh_path.strip_prefix("procedural:") {
            match shape {
                "sphere" => {
                    tracing::info!("Generating procedural sphere");
                    create_procedural_sphere(device, 0.5, 32, 32)
                }
                "cube" => {
                    tracing::info!("Generating procedural cube");
                    create_procedural_cube(device)
                }
                _ => {
                    tracing::warn!("Unknown procedural shape '{}', using cube", shape);
                    create_procedural_cube(device)
                }
            }
        } else {
            load_gltf(device, project_root, mesh_path)?
        };

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
/// Merges all nodes/meshes/primitives into a single draw call,
/// applying each node's world transform to positions and normals.
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

    let mut all_vertices: Vec<Vertex3D> = Vec::new();
    let mut all_indices: Vec<u32> = Vec::new();
    let mut total_primitives = 0u32;

    // Walk every node in every scene, applying world transforms
    for scene in document.scenes() {
        for node in scene.nodes() {
            collect_node_meshes(
                &node,
                glam::Mat4::IDENTITY,
                &buffers,
                &mut all_vertices,
                &mut all_indices,
                &mut total_primitives,
            );
        }
    }

    if all_vertices.is_empty() {
        return Err(MeshError::NoMeshes);
    }

    tracing::info!(
        "glTF '{}': merged {} primitives, {} verts, {} indices",
        mesh_path,
        total_primitives,
        all_vertices.len(),
        all_indices.len()
    );

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Mesh VB: {}", mesh_path)),
        contents: bytemuck::cast_slice(&all_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("Mesh IB: {}", mesh_path)),
        contents: bytemuck::cast_slice(&all_indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    Ok(GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: all_indices.len() as u32,
    })
}

/// Recursively walk a glTF node tree, collecting mesh primitives with
/// accumulated world transforms.
fn collect_node_meshes(
    node: &gltf::Node,
    parent_transform: glam::Mat4,
    buffers: &[gltf::buffer::Data],
    vertices: &mut Vec<Vertex3D>,
    indices: &mut Vec<u32>,
    prim_count: &mut u32,
) {
    let local = glam::Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent_transform * local;
    // Normal matrix: inverse-transpose of upper-left 3x3
    let normal_mat = glam::Mat3::from_mat4(world).inverse().transpose();

    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buf| Some(&buffers[buf.index()]));

            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(p) => p.collect(),
                None => continue,
            };

            let tex_coords: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            let colors: Vec<[f32; 4]> = reader
                .read_colors(0)
                .map(|c| c.into_rgba_f32().collect())
                .unwrap_or_else(|| vec![[1.0, 1.0, 1.0, 1.0]; positions.len()]);

            // Collect indices first so we can use them for normal generation
            let prim_indices: Vec<u32> = if let Some(read_indices) = reader.read_indices() {
                read_indices.into_u32().collect()
            } else {
                (0..positions.len() as u32).collect()
            };

            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|n| n.collect())
                .unwrap_or_else(|| {
                    if !prim_indices.is_empty() {
                        generate_smooth_normals(&positions, &prim_indices)
                    } else {
                        generate_flat_normals(&positions)
                    }
                });

            let base_vertex = vertices.len() as u32;

            for (i, pos) in positions.iter().enumerate() {
                let p = world.transform_point3(glam::Vec3::from(*pos));
                let n = normal_mat
                    * glam::Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                vertices.push(Vertex3D {
                    position: p.to_array(),
                    normal: n.normalize_or_zero().to_array(),
                    tex_coords: tex_coords.get(i).copied().unwrap_or([0.0, 0.0]),
                    color: colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]),
                });
            }

            for idx in &prim_indices {
                indices.push(base_vertex + idx);
            }

            *prim_count += 1;
        }
    }

    // Recurse into children
    for child in node.children() {
        collect_node_meshes(&child, world, buffers, vertices, indices, prim_count);
    }
}

/// Generate smooth normals by accumulating face normals at each vertex using the index buffer.
fn generate_smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![glam::Vec3::ZERO; positions.len()];

    // Accumulate face normals at each vertex
    for tri in indices.chunks_exact(3) {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;
        if i0 < positions.len() && i1 < positions.len() && i2 < positions.len() {
            let v0 = glam::Vec3::from(positions[i0]);
            let v1 = glam::Vec3::from(positions[i1]);
            let v2 = glam::Vec3::from(positions[i2]);
            let face_normal = (v1 - v0).cross(v2 - v0);
            // Weight by face area (unnormalized cross product magnitude)
            normals[i0] += face_normal;
            normals[i1] += face_normal;
            normals[i2] += face_normal;
        }
    }

    // Normalize
    normals
        .iter()
        .map(|n| {
            let normalized = n.normalize_or_zero();
            if normalized == glam::Vec3::ZERO {
                [0.0, 1.0, 0.0]
            } else {
                normalized.to_array()
            }
        })
        .collect()
}

/// Generate flat normals for non-indexed geometry (every 3 consecutive vertices form a triangle).
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

/// Create a procedural UV sphere.
fn create_procedural_sphere(device: &wgpu::Device, radius: f32, rings: u32, sectors: u32) -> GpuMesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for ring in 0..=rings {
        let theta = std::f32::consts::PI * ring as f32 / rings as f32;
        let sin_theta = theta.sin();
        let cos_theta = theta.cos();

        for sector in 0..=sectors {
            let phi = 2.0 * std::f32::consts::PI * sector as f32 / sectors as f32;
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();

            let nx = sin_theta * cos_phi;
            let ny = cos_theta;
            let nz = sin_theta * sin_phi;

            vertices.push(Vertex3D {
                position: [radius * nx, radius * ny, radius * nz],
                normal: [nx, ny, nz],
                tex_coords: [sector as f32 / sectors as f32, ring as f32 / rings as f32],
                color: [1.0, 1.0, 1.0, 1.0],
            });
        }
    }

    for ring in 0..rings {
        for sector in 0..sectors {
            let curr_row = ring * (sectors + 1);
            let next_row = (ring + 1) * (sectors + 1);

            // CCW winding when viewed from outside the sphere
            indices.push(curr_row + sector);
            indices.push(next_row + sector + 1);
            indices.push(next_row + sector);

            indices.push(curr_row + sector);
            indices.push(curr_row + sector + 1);
            indices.push(next_row + sector + 1);
        }
    }

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Sphere VB"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Procedural Sphere IB"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
    }
}

/// Create a procedural unit cube as fallback.
fn create_procedural_cube(device: &wgpu::Device) -> GpuMesh {
    #[rustfmt::skip]
    let vertices: Vec<Vertex3D> = vec![
        // Front face (z = 0.5)
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        // Back face (z = -0.5)
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        // Top face (y = 0.5)
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        // Bottom face (y = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        // Right face (x = 0.5)
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        // Left face (x = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0] },
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
