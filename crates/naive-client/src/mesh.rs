use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wgpu::util::DeviceExt;

use crate::components::MeshHandle;

#[derive(Debug)]
pub enum MeshError {
    IoError(String),
    GltfError(gltf::Error),
    StlError(String),
    NoMeshes,
    NoPrimitives,
    NoPositions,
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "Mesh IO error: {}", msg),
            Self::GltfError(e) => write!(f, "glTF error: {}", e),
            Self::StlError(msg) => write!(f, "STL error: {}", msg),
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

/// 3D vertex for mesh rendering (with optional skeletal animation data).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex3D {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tex_coords: [f32; 2],
    pub color: [f32; 4],
    /// Joint indices for skeletal animation (up to 4 influences).
    /// For non-skinned meshes, set to [0, 0, 0, 0].
    pub joint_indices: [u32; 4],
    /// Joint weights for skeletal animation (up to 4 influences).
    /// For non-skinned meshes, set to [1.0, 0.0, 0.0, 0.0].
    pub joint_weights: [f32; 4],
}

impl Vertex3D {
    const ATTRIBS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x3,  // position
        1 => Float32x3,  // normal
        2 => Float32x2,  // tex_coords
        3 => Float32x4,  // color
        4 => Uint32x4,   // joint_indices
        5 => Float32x4   // joint_weights
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex3D>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Skinning data extracted from a glTF file.
pub struct SkinData {
    pub skeleton: naive_core::animation::Skeleton,
    pub clips: Vec<naive_core::animation::AnimationClip>,
}

/// A loaded GPU mesh.
pub struct GpuMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    /// Texture bind group for GLB albedo texture (None = no texture).
    pub texture_bind_group: Option<wgpu::BindGroup>,
    /// Skinning data if the mesh has a skeleton and animations.
    pub skin_data: Option<SkinData>,
    /// CPU-side vertex positions for physics trimesh colliders.
    pub physics_vertices: Option<Vec<[f32; 3]>>,
    /// CPU-side triangle indices for physics trimesh colliders.
    pub physics_indices: Option<Vec<[u32; 3]>>,
}

/// Shared texture resources: bind group layout and 1x1 white fallback.
pub struct TextureResources {
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub default_bind_group: wgpu::BindGroup,
}

impl TextureResources {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Texture Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Create 1x1 white fallback texture
        let white_pixel: [u8; 4] = [255, 255, 255, 255];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("White 1x1 Fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &white_pixel,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let default_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Default White Texture BG"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            bind_group_layout,
            default_bind_group,
        }
    }
}

fn create_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image_data: &gltf::image::Data,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    // Ensure RGBA8 format
    let (rgba_pixels, width, height) = match image_data.format {
        gltf::image::Format::R8G8B8A8 => (
            image_data.pixels.clone(),
            image_data.width,
            image_data.height,
        ),
        gltf::image::Format::R8G8B8 => {
            let rgba: Vec<u8> = image_data
                .pixels
                .chunks(3)
                .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .collect();
            (rgba, image_data.width, image_data.height)
        }
        _ => {
            // Fallback: try to use as RGBA
            (
                image_data.pixels.clone(),
                image_data.width,
                image_data.height,
            )
        }
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GLB Albedo Texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba_pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("GLB Albedo Texture BG"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}

/// Cache of loaded meshes, keyed by file path.
pub struct MeshCache {
    meshes: Vec<GpuMesh>,
    path_to_handle: HashMap<PathBuf, MeshHandle>,
}

/// Vertices and triangle indices of a mesh, as the physics engine takes them.
pub type PhysicsTrimesh = (Vec<rapier3d::na::Point3<f32>>, Vec<[u32; 3]>);

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
        queue: &wgpu::Queue,
        project_root: &Path,
        mesh_path: &str,
        texture_resources: Option<&TextureResources>,
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
                "plane" => {
                    tracing::info!("Generating procedural plane");
                    create_procedural_plane(device, 1.0, 1.0, 1)
                }
                "cylinder" => {
                    tracing::info!("Generating procedural cylinder");
                    create_procedural_cylinder(device, 0.5, 1.0, 32)
                }
                "cone" => {
                    tracing::info!("Generating procedural cone");
                    create_procedural_cone(device, 0.5, 1.0, 32)
                }
                "torus" => {
                    tracing::info!("Generating procedural torus");
                    create_procedural_torus(device, 0.3, 0.1, 32, 16)
                }
                _ => {
                    tracing::warn!("Unknown procedural shape '{}', using cube", shape);
                    create_procedural_cube(device)
                }
            }
        } else if mesh_path.to_lowercase().ends_with(".stl") {
            load_stl(device, project_root, mesh_path)?
        } else {
            load_gltf(device, queue, project_root, mesh_path, texture_resources)?
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

    /// Insert a runtime mesh built from raw vertex data (from Lua mesh.create).
    pub fn insert_runtime_mesh(
        &mut self,
        device: &wgpu::Device,
        name: &str,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        uvs: &[[f32; 2]],
        indices: &[u32],
    ) -> MeshHandle {
        let key = PathBuf::from(format!("runtime:{}", name));
        if let Some(&handle) = self.path_to_handle.get(&key) {
            return handle;
        }

        let j = [0u32, 0, 0, 0];
        let w = [1.0f32, 0.0, 0.0, 0.0];
        let vertices: Vec<Vertex3D> = positions
            .iter()
            .enumerate()
            .map(|(i, pos)| Vertex3D {
                position: *pos,
                normal: normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                tex_coords: uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: j,
                joint_weights: w,
            })
            .collect();

        let gpu_mesh =
            build_procedural_gpu_mesh(device, &vertices, indices, &format!("Runtime: {}", name));
        let handle = MeshHandle(self.meshes.len());
        self.meshes.push(gpu_mesh);
        self.path_to_handle.insert(key, handle);
        tracing::info!(
            "Created runtime mesh '{}': {} vertices, {} indices",
            name,
            positions.len(),
            indices.len()
        );
        handle
    }

    /// Check if a mesh has skin data.
    pub fn has_skin(&self, handle: MeshHandle) -> bool {
        self.meshes
            .get(handle.0)
            .map(|m| m.skin_data.is_some())
            .unwrap_or(false)
    }

    /// Take skin data from all meshes that have it (ownership transfer).
    /// Returns (mesh_index, SkinData) pairs.
    pub fn take_skin_data(&mut self) -> Vec<(usize, SkinData)> {
        let mut result = Vec::new();
        for (i, mesh) in self.meshes.iter_mut().enumerate() {
            if let Some(skin_data) = mesh.skin_data.take() {
                result.push((i, skin_data));
            }
        }
        result
    }

    /// Get physics trimesh data for a mesh handle (vertices scaled by transform scale).
    pub fn get_physics_trimesh(
        &self,
        handle: MeshHandle,
        scale: glam::Vec3,
    ) -> Option<PhysicsTrimesh> {
        let mesh = self.meshes.get(handle.0)?;
        let verts = mesh.physics_vertices.as_ref()?;
        let idxs = mesh.physics_indices.as_ref()?;
        let scaled_verts: Vec<rapier3d::na::Point3<f32>> = verts
            .iter()
            .map(|v| rapier3d::na::Point3::new(v[0] * scale.x, v[1] * scale.y, v[2] * scale.z))
            .collect();
        Some((scaled_verts, idxs.clone()))
    }

    /// Get the path/name for a mesh handle (reverse lookup for serialization).
    pub fn name_for_handle(&self, handle: MeshHandle) -> Option<String> {
        for (path, &h) in &self.path_to_handle {
            if h.0 == handle.0 {
                return Some(path.to_string_lossy().to_string());
            }
        }
        None
    }
}

impl Default for MeshCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Load an STL file (ASCII or binary) and create GPU buffers.
/// STL files contain triangles with per-face normals but no UVs or textures.
fn load_stl(
    device: &wgpu::Device,
    project_root: &Path,
    mesh_path: &str,
) -> Result<GpuMesh, MeshError> {
    let full_path =
        crate::project_path::resolve(project_root, mesh_path).map_err(MeshError::IoError)?;

    if !full_path.exists() {
        tracing::warn!("STL file not found: {:?}, using procedural cube", full_path);
        return Ok(create_procedural_cube(device));
    }

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .open(&full_path)
        .map_err(|e| MeshError::StlError(format!("Failed to open {:?}: {}", full_path, e)))?;

    let stl = stl_io::read_stl(&mut file)
        .map_err(|e| MeshError::StlError(format!("Failed to parse {:?}: {}", full_path, e)))?;

    if stl.faces.is_empty() {
        return Err(MeshError::StlError("STL file contains no triangles".into()));
    }

    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];

    // Build per-vertex data from indexed STL
    let mut vertices: Vec<Vertex3D> = Vec::with_capacity(stl.vertices.len());
    // Compute per-vertex normals by accumulating face normals
    let mut vertex_normals = vec![[0.0f32; 3]; stl.vertices.len()];

    for face in &stl.faces {
        let n = [face.normal[0], face.normal[1], face.normal[2]];
        for &vi in &face.vertices {
            let idx = vi;
            vertex_normals[idx][0] += n[0];
            vertex_normals[idx][1] += n[1];
            vertex_normals[idx][2] += n[2];
        }
    }

    // Normalize accumulated normals
    for normal in &mut vertex_normals {
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if len > 1e-8 {
            normal[0] /= len;
            normal[1] /= len;
            normal[2] /= len;
        } else {
            *normal = [0.0, 1.0, 0.0];
        }
    }

    for (i, v) in stl.vertices.iter().enumerate() {
        vertices.push(Vertex3D {
            position: [v[0], v[1], v[2]],
            normal: vertex_normals[i],
            tex_coords: [0.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }

    let indices: Vec<u32> = stl
        .faces
        .iter()
        .flat_map(|face| face.vertices.iter().map(|&vi| vi as u32))
        .collect();

    tracing::info!(
        "STL '{}': {} triangles, {} vertices, {} indices",
        mesh_path,
        stl.faces.len(),
        vertices.len(),
        indices.len()
    );

    let physics_vertices: Vec<[f32; 3]> = vertices.iter().map(|v| v.position).collect();
    let physics_indices: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect();

    let mut gpu_mesh =
        build_procedural_gpu_mesh(device, &vertices, &indices, &format!("STL: {}", mesh_path));
    gpu_mesh.physics_vertices = Some(physics_vertices);
    gpu_mesh.physics_indices = Some(physics_indices);

    Ok(gpu_mesh)
}

/// Load a glTF file and create GPU buffers.
/// Merges all nodes/meshes/primitives into a single draw call,
/// applying each node's world transform to positions and normals.
fn load_gltf(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project_root: &Path,
    mesh_path: &str,
    texture_resources: Option<&TextureResources>,
) -> Result<GpuMesh, MeshError> {
    let full_path =
        crate::project_path::resolve(project_root, mesh_path).map_err(MeshError::IoError)?;

    // If the file doesn't exist, generate a procedural cube
    if !full_path.exists() {
        tracing::warn!(
            "Mesh file not found: {:?}, using procedural cube",
            full_path
        );
        return Ok(create_procedural_cube(device));
    }

    crate::project_path::validate_gltf_resources(project_root, &full_path)
        .map_err(MeshError::IoError)?;
    let (document, buffers, images) = gltf::import(&full_path)?;

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

    // Extract GLB texture: find the first base_color texture
    let texture_bind_group = if let Some(tex_res) = texture_resources {
        let mut texture_image_index: Option<usize> = None;
        'outer: for mesh in document.meshes() {
            for prim in mesh.primitives() {
                if let Some(info) = prim
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_texture()
                {
                    texture_image_index = Some(info.texture().source().index());
                    break 'outer;
                }
            }
        }
        if let Some(idx) = texture_image_index {
            if idx < images.len() {
                tracing::info!(
                    "GLB '{}': loading albedo texture ({}x{})",
                    mesh_path,
                    images[idx].width,
                    images[idx].height
                );
                Some(create_texture_bind_group(
                    device,
                    queue,
                    &images[idx],
                    &tex_res.bind_group_layout,
                ))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Extract skin and animation data
    let skin_data = extract_skin_data(&document, &buffers);

    // Keep CPU-side data for physics trimesh colliders
    let physics_vertices: Vec<[f32; 3]> = all_vertices.iter().map(|v| v.position).collect();
    let physics_indices: Vec<[u32; 3]> = all_indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect();

    Ok(GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: all_indices.len() as u32,
        texture_bind_group,
        skin_data,
        physics_vertices: Some(physics_vertices),
        physics_indices: Some(physics_indices),
    })
}

/// Extract skeleton and animation clip data from a glTF document.
fn extract_skin_data(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Option<SkinData> {
    let skin = document.skins().next()?;

    // Build joint list
    let joint_nodes: Vec<gltf::Node> = skin.joints().collect();

    // Read inverse bind matrices
    let ibms: Vec<glam::Mat4> = skin
        .reader(|buf| Some(&buffers[buf.index()]))
        .read_inverse_bind_matrices()
        .map(|iter| iter.map(|m| glam::Mat4::from_cols_array_2d(&m)).collect())
        .unwrap_or_else(|| vec![glam::Mat4::IDENTITY; joint_nodes.len()]);

    // Map node index -> joint index
    let node_to_joint: std::collections::HashMap<usize, usize> = joint_nodes
        .iter()
        .enumerate()
        .map(|(ji, node)| (node.index(), ji))
        .collect();

    // Build parent map by walking node children: if joint A has child joint B, then B's parent is A
    let mut parent_map: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for node in &joint_nodes {
        for child in node.children() {
            if node_to_joint.contains_key(&child.index()) {
                parent_map.insert(child.index(), node.index());
            }
        }
    }

    // Build joints
    let joints: Vec<naive_core::animation::Joint> = joint_nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let parent = parent_map
                .get(&node.index())
                .and_then(|parent_node_idx| node_to_joint.get(parent_node_idx).copied());

            let (t, r, s) = node.transform().decomposed();
            let local_transform = naive_core::animation::JointTransform {
                translation: glam::Vec3::from(t),
                rotation: glam::Quat::from_array(r),
                scale: glam::Vec3::from(s),
            };

            naive_core::animation::Joint {
                name: node.name().unwrap_or("joint").to_string(),
                parent,
                inverse_bind_matrix: ibms.get(i).copied().unwrap_or(glam::Mat4::IDENTITY),
                local_transform,
            }
        })
        .collect();

    let skeleton = naive_core::animation::Skeleton::new(joints);

    // Extract animation clips
    let clips: Vec<naive_core::animation::AnimationClip> = document
        .animations()
        .enumerate()
        .map(|(anim_idx, anim)| {
            let channels: Vec<naive_core::animation::AnimationChannel> = anim
                .channels()
                .filter_map(|channel| {
                    let target = channel.target();
                    let joint_index = node_to_joint.get(&target.node().index())?;

                    let property = match target.property() {
                        gltf::animation::Property::Translation => {
                            naive_core::animation::ChannelProperty::Translation
                        }
                        gltf::animation::Property::Rotation => {
                            naive_core::animation::ChannelProperty::Rotation
                        }
                        gltf::animation::Property::Scale => {
                            naive_core::animation::ChannelProperty::Scale
                        }
                        _ => return None,
                    };

                    let sampler = channel.sampler();
                    let interpolation = match sampler.interpolation() {
                        gltf::animation::Interpolation::Step => {
                            naive_core::animation::Interpolation::Step
                        }
                        gltf::animation::Interpolation::Linear => {
                            naive_core::animation::Interpolation::Linear
                        }
                        gltf::animation::Interpolation::CubicSpline => {
                            naive_core::animation::Interpolation::CubicSpline
                        }
                    };

                    let reader = channel.reader(|buf| Some(&buffers[buf.index()]));
                    let timestamps: Vec<f32> = reader.read_inputs()?.collect();

                    let values = match property {
                        naive_core::animation::ChannelProperty::Translation
                        | naive_core::animation::ChannelProperty::Scale => {
                            let outputs = reader.read_outputs()?;
                            match outputs {
                                gltf::animation::util::ReadOutputs::Translations(iter) => {
                                    naive_core::animation::ChannelValues::Vec3(
                                        iter.map(glam::Vec3::from).collect(),
                                    )
                                }
                                gltf::animation::util::ReadOutputs::Scales(iter) => {
                                    naive_core::animation::ChannelValues::Vec3(
                                        iter.map(glam::Vec3::from).collect(),
                                    )
                                }
                                _ => return None,
                            }
                        }
                        naive_core::animation::ChannelProperty::Rotation => {
                            let outputs = reader.read_outputs()?;
                            match outputs {
                                gltf::animation::util::ReadOutputs::Rotations(iter) => {
                                    naive_core::animation::ChannelValues::Quat(
                                        iter.into_f32()
                                            .map(|v| glam::Quat::from_array(v).normalize())
                                            .collect(),
                                    )
                                }
                                _ => return None,
                            }
                        }
                    };

                    Some(naive_core::animation::AnimationChannel {
                        joint_index: *joint_index,
                        property,
                        interpolation,
                        timestamps,
                        values,
                    })
                })
                .collect();

            let duration = channels
                .iter()
                .flat_map(|c| c.timestamps.last())
                .copied()
                .fold(0.0f32, f32::max);

            naive_core::animation::AnimationClip {
                name: anim
                    .name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("clip_{}", anim_idx)),
                duration,
                channels,
            }
        })
        .collect();

    if skeleton.joints.is_empty() {
        return None;
    }

    tracing::info!(
        "Loaded skeleton: {} joints, {} animation clips",
        skeleton.joints.len(),
        clips.len()
    );

    Some(SkinData { skeleton, clips })
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

            let normals: Vec<[f32; 3]> =
                reader
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

            // Read joint indices and weights for skinned meshes
            let joints: Vec<[u32; 4]> = reader
                .read_joints(0)
                .map(|j| {
                    j.into_u16()
                        .map(|v| [v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32])
                        .collect()
                })
                .unwrap_or_else(|| vec![[0, 0, 0, 0]; positions.len()]);

            let weights: Vec<[f32; 4]> = reader
                .read_weights(0)
                .map(|w| w.into_f32().collect())
                .unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; positions.len()]);

            // For skinned meshes, don't bake world transforms (skeleton handles that).
            // For non-skinned meshes, bake transforms as before.
            let is_skinned = reader.read_joints(0).is_some();
            // Re-read since we consumed the iterator above
            let _ = is_skinned; // Use the flag from joints vec being non-default
            let has_joints = joints.iter().any(|j| j != &[0u32, 0, 0, 0]);

            for (i, pos) in positions.iter().enumerate() {
                let (p, n) = if has_joints {
                    // Skinned: store local-space positions (skeleton transforms at runtime)
                    let local_p = local.transform_point3(glam::Vec3::from(*pos));
                    let local_n = glam::Mat3::from_mat4(local).inverse().transpose()
                        * glam::Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                    (local_p, local_n)
                } else {
                    // Non-skinned: bake world transform
                    let p = world.transform_point3(glam::Vec3::from(*pos));
                    let n = normal_mat
                        * glam::Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                    (p, n)
                };
                vertices.push(Vertex3D {
                    position: p.to_array(),
                    normal: n.normalize_or_zero().to_array(),
                    tex_coords: tex_coords.get(i).copied().unwrap_or([0.0, 0.0]),
                    color: colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]),
                    joint_indices: joints.get(i).copied().unwrap_or([0, 0, 0, 0]),
                    joint_weights: weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
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
    for tri in indices.as_chunks::<3>().0 {
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
fn create_procedural_sphere(
    device: &wgpu::Device,
    radius: f32,
    rings: u32,
    sectors: u32,
) -> GpuMesh {
    let (vertices, indices) = sphere_geometry(radius, rings, sectors);
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Sphere")
}

fn sphere_geometry(radius: f32, rings: u32, sectors: u32) -> (Vec<Vertex3D>, Vec<u32>) {
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
                joint_indices: [0, 0, 0, 0],
                joint_weights: [1.0, 0.0, 0.0, 0.0],
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

    (vertices, indices)
}

/// Create a procedural unit cube as fallback.
fn create_procedural_cube(device: &wgpu::Device) -> GpuMesh {
    let (vertices, indices) = cube_geometry();
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Cube")
}

fn cube_geometry() -> (Vec<Vertex3D>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    #[rustfmt::skip]
    let vertices: Vec<Vertex3D> = vec![
        // Front face (z = 0.5)
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Back face (z = -0.5)
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Top face (y = 0.5)
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Bottom face (y = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Right face (x = 0.5)
        Vertex3D { position: [ 0.5, -0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5, -0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [ 0.5,  0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Left face (x = -0.5)
        Vertex3D { position: [-0.5, -0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5, -0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5,  0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        Vertex3D { position: [-0.5,  0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
    ];

    // Counter-clockwise seen from outside, so back-face culling keeps the
    // outer faces.
    #[rustfmt::skip]
    let indices: Vec<u32> = vec![
        0,  1,  2,  0,  2,  3,   // front
        4,  5,  6,  4,  6,  7,   // back
        8,  9,  10, 8,  10, 11,  // top
        12, 13, 14, 12, 14, 15,  // bottom
        16, 17, 18, 16, 18, 19,  // right
        20, 21, 22, 20, 22, 23,  // left
    ];

    (vertices, indices)
}

/// Create a procedural XZ plane centered at the origin.
fn create_procedural_plane(
    device: &wgpu::Device,
    width: f32,
    depth: f32,
    subdivisions: u32,
) -> GpuMesh {
    let (vertices, indices) = plane_geometry(width, depth, subdivisions);
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Plane")
}

fn plane_geometry(width: f32, depth: f32, subdivisions: u32) -> (Vec<Vertex3D>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let segs = subdivisions + 1;
    let hw = width / 2.0;
    let hd = depth / 2.0;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for iz in 0..=subdivisions {
        for ix in 0..=subdivisions {
            let u = ix as f32 / subdivisions as f32;
            let v = iz as f32 / subdivisions as f32;
            vertices.push(Vertex3D {
                position: [-hw + u * width, 0.0, -hd + v * depth],
                normal: [0.0, 1.0, 0.0],
                tex_coords: [u, v],
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: j,
                joint_weights: w,
            });
        }
    }

    for iz in 0..subdivisions {
        for ix in 0..subdivisions {
            let tl = iz * segs + ix;
            let tr = tl + 1;
            let bl = (iz + 1) * segs + ix;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, br, tr, tl, bl, br]);
        }
    }

    (vertices, indices)
}

/// Create a procedural Y-axis aligned cylinder with caps.
fn create_procedural_cylinder(
    device: &wgpu::Device,
    radius: f32,
    height: f32,
    segments: u32,
) -> GpuMesh {
    let (vertices, indices) = cylinder_geometry(radius, height, segments);
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Cylinder")
}

fn cylinder_geometry(radius: f32, height: f32, segments: u32) -> (Vec<Vertex3D>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let half_h = height / 2.0;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    // Side vertices: two rings
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let u = i as f32 / segments as f32;

        // Bottom ring
        vertices.push(Vertex3D {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [cos_a, 0.0, sin_a],
            tex_coords: [u, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
        // Top ring
        vertices.push(Vertex3D {
            position: [radius * cos_a, half_h, radius * sin_a],
            normal: [cos_a, 0.0, sin_a],
            tex_coords: [u, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }

    // Side indices
    for i in 0..segments {
        let b0 = i * 2;
        let t0 = b0 + 1;
        let b1 = b0 + 2;
        let t1 = b0 + 3;
        indices.extend_from_slice(&[b0, t0, t1, b0, t1, b1]);
    }

    // Top cap
    let top_center = vertices.len() as u32;
    vertices.push(Vertex3D {
        position: [0.0, half_h, 0.0],
        normal: [0.0, 1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(Vertex3D {
            position: [radius * cos_a, half_h, radius * sin_a],
            normal: [0.0, 1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[top_center, top_center + 2 + i, top_center + 1 + i]);
    }

    // Bottom cap
    let bot_center = vertices.len() as u32;
    vertices.push(Vertex3D {
        position: [0.0, -half_h, 0.0],
        normal: [0.0, -1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(Vertex3D {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [0.0, -1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 - 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    (vertices, indices)
}

/// Create a procedural Y-axis cone with a base cap.
fn create_procedural_cone(
    device: &wgpu::Device,
    radius: f32,
    height: f32,
    segments: u32,
) -> GpuMesh {
    let (vertices, indices) = cone_geometry(radius, height, segments);
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Cone")
}

fn cone_geometry(radius: f32, height: f32, segments: u32) -> (Vec<Vertex3D>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let half_h = height / 2.0;
    let slope = radius / height;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    // Tip vertex
    let tip_idx = 0u32;
    vertices.push(Vertex3D {
        position: [0.0, half_h, 0.0],
        normal: [0.0, 1.0, 0.0],
        tex_coords: [0.5, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });

    // Base ring vertices for sides
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let ny = slope;
        let len = (1.0 + ny * ny).sqrt();
        vertices.push(Vertex3D {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [cos_a / len, ny / len, sin_a / len],
            tex_coords: [i as f32 / segments as f32, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }

    // Side triangles
    for i in 0..segments {
        indices.extend_from_slice(&[tip_idx, 2 + i, 1 + i]);
    }

    // Base cap
    let bot_center = vertices.len() as u32;
    vertices.push(Vertex3D {
        position: [0.0, -half_h, 0.0],
        normal: [0.0, -1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(Vertex3D {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [0.0, -1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 - 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    (vertices, indices)
}

/// Create a procedural torus (donut) centered at the origin in the XZ plane.
fn create_procedural_torus(
    device: &wgpu::Device,
    major_radius: f32,
    minor_radius: f32,
    major_segments: u32,
    minor_segments: u32,
) -> GpuMesh {
    let (vertices, indices) =
        torus_geometry(major_radius, minor_radius, major_segments, minor_segments);
    build_procedural_gpu_mesh(device, &vertices, &indices, "Procedural Torus")
}

fn torus_geometry(
    major_radius: f32,
    minor_radius: f32,
    major_segments: u32,
    minor_segments: u32,
) -> (Vec<Vertex3D>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let pi2 = 2.0 * std::f32::consts::PI;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for i in 0..=major_segments {
        let theta = pi2 * i as f32 / major_segments as f32;
        let (sin_t, cos_t) = (theta.sin(), theta.cos());

        for k in 0..=minor_segments {
            let phi = pi2 * k as f32 / minor_segments as f32;
            let (sin_p, cos_p) = (phi.sin(), phi.cos());

            let x = (major_radius + minor_radius * cos_p) * cos_t;
            let y = minor_radius * sin_p;
            let z = (major_radius + minor_radius * cos_p) * sin_t;

            vertices.push(Vertex3D {
                position: [x, y, z],
                normal: [cos_p * cos_t, sin_p, cos_p * sin_t],
                tex_coords: [
                    i as f32 / major_segments as f32,
                    k as f32 / minor_segments as f32,
                ],
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: j,
                joint_weights: w,
            });
        }
    }

    let ring_size = minor_segments + 1;
    for i in 0..major_segments {
        for k in 0..minor_segments {
            let a = i * ring_size + k;
            let b = a + 1;
            let c = (i + 1) * ring_size + k;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    (vertices, indices)
}

/// Helper to build a GpuMesh from vertex and index data.
fn build_procedural_gpu_mesh(
    device: &wgpu::Device,
    vertices: &[Vertex3D],
    indices: &[u32],
    label: &str,
) -> GpuMesh {
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("{} VB", label)),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&format!("{} IB", label)),
        contents: bytemuck::cast_slice(indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
        texture_bind_group: None,
        skin_data: None,
        physics_vertices: None,
        physics_indices: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    /// The mesh passes cull back faces with `FrontFace::Ccw`, so every
    /// triangle must wind counter-clockwise seen from the side its normals
    /// point to. One wound the other way is culled from outside and drawn
    /// from inside, where its normals face away from the light.
    fn assert_wound_outward(shape: &str, (vertices, indices): (Vec<Vertex3D>, Vec<u32>)) {
        for tri in indices.as_chunks::<3>().0 {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| &vertices[i as usize]);
            let (pa, pb, pc) = (
                Vec3::from(a.position),
                Vec3::from(b.position),
                Vec3::from(c.position),
            );
            let face_normal = (pb - pa).cross(pc - pa);
            // Triangles that collapse at a pole have no winding.
            if face_normal.length() < 1e-6 {
                continue;
            }
            let vertex_normal = Vec3::from(a.normal) + Vec3::from(b.normal) + Vec3::from(c.normal);
            assert!(
                face_normal.dot(vertex_normal) > 0.0,
                "procedural {shape}: triangle {tri:?} winds clockwise seen from outside"
            );
        }
    }

    #[test]
    fn procedural_meshes_wind_counter_clockwise_seen_from_outside() {
        assert_wound_outward("cube", cube_geometry());
        assert_wound_outward("sphere", sphere_geometry(0.5, 32, 32));
        assert_wound_outward("plane", plane_geometry(1.0, 1.0, 4));
        assert_wound_outward("cylinder", cylinder_geometry(0.5, 1.0, 32));
        assert_wound_outward("cone", cone_geometry(0.5, 1.0, 32));
        assert_wound_outward("torus", torus_geometry(0.3, 0.1, 32, 16));
    }
}
