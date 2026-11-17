use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wgpu::util::DeviceExt;

use crate::components::MeshHandle;

pub use naive_sim::assets::{AssetError as MeshError, SkinData};

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

impl From<naive_sim::assets::CpuVertex> for Vertex3D {
    fn from(v: naive_sim::assets::CpuVertex) -> Self {
        Self {
            position: v.position,
            normal: v.normal,
            tex_coords: v.tex_coords,
            color: v.color,
            joint_indices: v.joint_indices,
            joint_weights: v.joint_weights,
        }
    }
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
    /// Sphere around the vertices, for frustum culling.
    pub bounds: crate::culling::BoundingSphere,
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
    // CPU import normalizes every supported glTF pixel format before upload.
    debug_assert_eq!(image_data.format, gltf::image::Format::R8G8B8A8);
    let (width, height) = (image_data.width, image_data.height);

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
        &image_data.pixels,
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

pub use crate::physics::PhysicsTrimesh;

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

        let cpu = naive_sim::assets::load_mesh(project_root, mesh_path)?;
        let vertices: Vec<Vertex3D> = cpu.vertices.iter().copied().map(Vertex3D::from).collect();
        let mut gpu_mesh = build_procedural_gpu_mesh(device, &vertices, &cpu.indices, mesh_path);
        gpu_mesh.skin_data = cpu.skin;
        gpu_mesh.physics_vertices = Some(cpu.vertices.iter().map(|v| v.position).collect());
        gpu_mesh.physics_indices = Some(
            cpu.indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|v| [v[0], v[1], v[2]])
                .collect(),
        );
        if let (Some(index), Some(resources)) = (cpu.base_color_image, texture_resources) {
            gpu_mesh.texture_bind_group = Some(create_texture_bind_group(
                device,
                queue,
                &cpu.images[index],
                &resources.bind_group_layout,
            ));
        }

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
        let scaled_verts: Vec<rapier3d::math::Vector> = verts
            .iter()
            .map(|v| rapier3d::math::Vector::new(v[0] * scale.x, v[1] * scale.y, v[2] * scale.z))
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
        physics_vertices: Some(vertices.iter().map(|v| v.position).collect()),
        physics_indices: Some(
            indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|v| [v[0], v[1], v[2]])
                .collect(),
        ),
        bounds: crate::culling::BoundingSphere::from_positions(vertices.iter().map(|v| v.position)),
    }
}
