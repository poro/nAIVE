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
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            &white_pixel,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
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
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        Self { bind_group_layout, default_bind_group }
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
        gltf::image::Format::R8G8B8A8 => {
            (image_data.pixels.clone(), image_data.width, image_data.height)
        }
        gltf::image::Format::R8G8B8 => {
            let rgba: Vec<u8> = image_data.pixels.chunks(3)
                .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .collect();
            (rgba, image_data.width, image_data.height)
        }
        _ => {
            // Fallback: try to use as RGBA
            (image_data.pixels.clone(), image_data.width, image_data.height)
        }
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GLB Albedo Texture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture, mip_level: 0,
            origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
        },
        &rgba_pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
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
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    })
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
                _ => {
                    tracing::warn!("Unknown procedural shape '{}', using cube", shape);
                    create_procedural_cube(device)
                }
            }
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

    /// Check if a mesh has skin data.
    pub fn has_skin(&self, handle: MeshHandle) -> bool {
        self.meshes.get(handle.0).map(|m| m.skin_data.is_some()).unwrap_or(false)
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
    let full_path = project_root.join(mesh_path);

    // If the file doesn't exist, generate a procedural cube
    if !full_path.exists() {
        tracing::warn!(
            "Mesh file not found: {:?}, using procedural cube",
            full_path
        );
        return Ok(create_procedural_cube(device));
    }

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
                if let Some(info) = prim.material().pbr_metallic_roughness().base_color_texture() {
                    texture_image_index = Some(info.texture().source().index());
                    break 'outer;
                }
            }
        }
        if let Some(idx) = texture_image_index {
            if idx < images.len() {
                tracing::info!("GLB '{}': loading albedo texture ({}x{})", mesh_path, images[idx].width, images[idx].height);
                Some(create_texture_bind_group(device, queue, &images[idx], &tex_res.bind_group_layout))
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

    Ok(GpuMesh {
        vertex_buffer,
        index_buffer,
        index_count: all_indices.len() as u32,
        texture_bind_group,
        skin_data,
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
                                        iter.map(|v| glam::Vec3::from(v)).collect(),
                                    )
                                }
                                gltf::animation::util::ReadOutputs::Scales(iter) => {
                                    naive_core::animation::ChannelValues::Vec3(
                                        iter.map(|v| glam::Vec3::from(v)).collect(),
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

            // Read joint indices and weights for skinned meshes
            let joints: Vec<[u32; 4]> = reader
                .read_joints(0)
                .map(|j| j.into_u16().map(|v| [v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32]).collect())
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
                    let local_n = glam::Mat3::from_mat4(local)
                        .inverse()
                        .transpose()
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
        texture_bind_group: None,
        skin_data: None,
    }
}

/// Create a procedural unit cube as fallback.
fn create_procedural_cube(device: &wgpu::Device) -> GpuMesh {
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

    #[rustfmt::skip]
    let indices: Vec<u32> = vec![
        0,  2,  1,  0,  3,  2,   // front
        4,  6,  5,  4,  7,  6,   // back
        8,  10, 9,  8,  11, 10,  // top
        12, 14, 13, 12, 15, 14,  // bottom
        16, 18, 17, 16, 19, 18,  // right
        20, 22, 21, 20, 23, 22,  // left
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
        texture_bind_group: None,
        skin_data: None,
    }
}
