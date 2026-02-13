use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::components::{Camera, Transform};

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    pub view: [[f32; 4]; 4],           // offset 0
    pub projection: [[f32; 4]; 4],      // offset 64
    pub view_projection: [[f32; 4]; 4], // offset 128
    pub position: [f32; 3],             // offset 192
    pub near_plane: f32,                // offset 204
    pub far_plane: f32,                 // offset 208
    pub _pad1: f32,                     // offset 212 (align viewport_size to WGSL vec2 alignment 8)
    pub viewport_size: [f32; 2],        // offset 216
    pub _pad2: [f32; 4],               // offset 224 → 240 (align mat4 to 16)
    pub inv_view_projection: [[f32; 4]; 4], // offset 240, 64 bytes → total 304
}

impl Default for CameraUniform {
    fn default() -> Self {
        Self {
            view: Mat4::IDENTITY.to_cols_array_2d(),
            projection: Mat4::IDENTITY.to_cols_array_2d(),
            view_projection: Mat4::IDENTITY.to_cols_array_2d(),
            position: [0.0; 3],
            near_plane: 0.1,
            far_plane: 100.0,
            _pad1: 0.0,
            viewport_size: [1280.0, 720.0],
            _pad2: [0.0; 4],
            inv_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
        }
    }
}

/// Manages the camera uniform buffer and bind group.
pub struct CameraState {
    pub uniform: CameraUniform,
    pub buffer: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

impl CameraState {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Camera Bind Group Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform = CameraUniform::default();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Uniform Buffer"),
            contents: bytemuck::cast_slice(&[uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        CameraState {
            uniform,
            buffer,
            bind_group,
            bind_group_layout,
        }
    }

    /// Update the camera uniform from the main camera entity.
    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        camera: &Camera,
        transform: &Transform,
        viewport_width: u32,
        viewport_height: u32,
    ) {
        let view = Mat4::look_to_rh(
            transform.position,
            transform.rotation * Vec3::NEG_Z,
            Vec3::Y,
        );
        let projection = Mat4::perspective_rh(
            camera.fov_degrees.to_radians(),
            viewport_width as f32 / viewport_height.max(1) as f32,
            camera.near,
            camera.far,
        );
        let view_projection = projection * view;

        let inv_view_projection = view_projection.inverse();

        self.uniform = CameraUniform {
            view: view.to_cols_array_2d(),
            projection: projection.to_cols_array_2d(),
            view_projection: view_projection.to_cols_array_2d(),
            position: transform.position.to_array(),
            near_plane: camera.near,
            far_plane: camera.far,
            _pad1: 0.0,
            viewport_size: [viewport_width as f32, viewport_height as f32],
            _pad2: [0.0; 4],
            inv_view_projection: inv_view_projection.to_cols_array_2d(),
        };

        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&[self.uniform]));
    }

    /// Get the current view matrix.
    pub fn view_matrix(&self) -> Mat4 {
        Mat4::from_cols_array_2d(&self.uniform.view)
    }
}
