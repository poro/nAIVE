use glam::{Quat, Vec3};

use crate::camera::CameraState;
use crate::input::{InputState, Key};

/// Free fly camera for editor mode.
/// WASD + always-on mouse look, scroll to adjust speed,
/// Space/Q for up/down, Shift for fast mode.
pub struct EditorCamera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
    pub sensitivity: f32,
    pub fov_degrees: f32,
    pub near: f32,
    pub far: f32,
}

impl EditorCamera {
    pub fn new(position: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            position,
            yaw,
            pitch,
            speed: 5.0,
            sensitivity: 0.003,
            fov_degrees: 75.0,
            near: 0.1,
            far: 500.0,
        }
    }

    pub fn default_position() -> Self {
        Self::new(Vec3::new(0.0, 5.0, 10.0), 0.0, -0.3)
    }

    /// Update camera from input. Mouse look is always active in editor mode.
    pub fn update(&mut self, input: &InputState, dt: f32) {
        // Mouse look: always active (no button needed — works on Mac trackpads)
        let delta = input.mouse_delta();
        if delta.x.abs() > 0.01 || delta.y.abs() > 0.01 {
            self.yaw -= delta.x * self.sensitivity;
            self.pitch = (self.pitch - delta.y * self.sensitivity).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.01,
                std::f32::consts::FRAC_PI_2 - 0.01,
            );
        }

        // Speed adjustment via scroll wheel
        let scroll = input.scroll_delta().y;
        if scroll != 0.0 {
            self.speed = (self.speed * (1.0 + scroll * 0.1)).clamp(0.5, 200.0);
        }

        // Movement
        let speed = if input.key_held(Key::ShiftLeft) || input.key_held(Key::ShiftRight) {
            self.speed * 3.0
        } else {
            self.speed
        };

        let forward = self.forward();
        let right = self.right();
        let up = Vec3::Y;

        let mut movement = Vec3::ZERO;

        if input.key_held(Key::W) {
            movement += forward;
        }
        if input.key_held(Key::S) {
            movement -= forward;
        }
        if input.key_held(Key::A) {
            movement -= right;
        }
        if input.key_held(Key::D) {
            movement += right;
        }
        if input.key_held(Key::Space) {
            movement += up;
        }
        if input.key_held(Key::Q) {
            movement -= up;
        }

        if movement.length_squared() > 0.001 {
            self.position += movement.normalize() * speed * dt;
        }
    }

    /// Apply the editor camera to the CameraState GPU uniform.
    pub fn apply_to_camera_state(
        &self,
        camera_state: &mut CameraState,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) {
        let view = glam::camera::rh::view::look_to_mat4(self.position, self.forward(), Vec3::Y);
        let projection = glam::camera::rh::proj::directx::perspective(
            self.fov_degrees.to_radians(),
            width as f32 / height.max(1) as f32,
            self.near,
            self.far,
        );
        let view_projection = projection * view;
        let inv_view_projection = view_projection.inverse();

        camera_state.uniform = crate::camera::CameraUniform {
            view: view.to_cols_array_2d(),
            projection: projection.to_cols_array_2d(),
            view_projection: view_projection.to_cols_array_2d(),
            position: self.position.to_array(),
            near_plane: self.near,
            far_plane: self.far,
            _pad1: 0.0,
            viewport_size: [width as f32, height as f32],
            _pad2: [0.0; 4],
            inv_view_projection: inv_view_projection.to_cols_array_2d(),
        };

        queue.write_buffer(
            &camera_state.buffer,
            0,
            bytemuck::cast_slice(&[camera_state.uniform]),
        );
    }

    fn forward(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        Vec3::new(-sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch).normalize()
    }

    fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize()
    }

    /// Get the rotation quaternion for this camera.
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }
}
