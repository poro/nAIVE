//! CPU presentation contracts shared by the native renderer and headless scripts.
use glam::{Mat4, Vec3, Vec4};
use naive_core::components::{Camera, CameraMode, CameraRole, Player, Transform};

use crate::physics::PhysicsWorld;

/// Pixel dimensions, independent of any window or GPU surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

impl Viewport {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraView {
    pub camera: Camera,
    pub transform: Transform,
    pub view: Mat4,
    pub projection: Mat4,
    pub view_projection: Mat4,
    pub inv_view_projection: Mat4,
    pub viewport: Viewport,
}

impl CameraView {
    pub fn new(camera: &Camera, transform: &Transform, viewport: Viewport) -> Self {
        let viewport = Viewport::new(viewport.width, viewport.height);
        let view = glam::camera::rh::view::look_to_mat4(
            transform.position,
            transform.rotation * Vec3::NEG_Z,
            Vec3::Y,
        );
        let projection = glam::camera::rh::proj::directx::perspective(
            camera.fov_degrees.to_radians(),
            viewport.width as f32 / viewport.height as f32,
            camera.near,
            camera.far,
        );
        let view_projection = projection * view;
        Self {
            camera: camera.clone(),
            transform: transform.clone(),
            view,
            projection,
            view_projection,
            inv_view_projection: view_projection.inverse(),
            viewport,
        }
    }

    pub fn world_to_screen(&self, point: Vec3) -> (f32, f32, bool) {
        let clip = self.view_projection * point.extend(1.0);
        if clip.w <= 0.0 || !clip.is_finite() {
            return (0.0, 0.0, false);
        }
        let ndc = clip.truncate() / clip.w;
        let visible = (-1.0..=1.0).contains(&ndc.x)
            && (-1.0..=1.0).contains(&ndc.y)
            && (0.0..=1.0).contains(&ndc.z);
        (
            (ndc.x * 0.5 + 0.5) * self.viewport.width as f32,
            (0.5 - ndc.y * 0.5) * self.viewport.height as f32,
            visible,
        )
    }

    /// Ray starts at the near plane, matching the native picking convention.
    pub fn screen_to_ray(&self, x: f32, y: f32) -> Option<(Vec3, Vec3)> {
        let nx = x / self.viewport.width as f32 * 2.0 - 1.0;
        let ny = 1.0 - y / self.viewport.height as f32 * 2.0;
        let near = self.inv_view_projection * Vec4::new(nx, ny, 0.0, 1.0);
        let far = self.inv_view_projection * Vec4::new(nx, ny, 1.0, 1.0);
        if near.w.abs() < 1e-6 || far.w.abs() < 1e-6 {
            return None;
        }
        let origin = near.truncate() / near.w;
        let direction = (far.truncate() / far.w - origin).normalize_or_zero();
        (origin.is_finite() && direction.is_finite() && direction.length_squared() > 0.0)
            .then_some((origin, direction))
    }
}

/// Select the main camera and apply the same player offset and wall avoidance
/// for both render uploads and script projection queries.
pub fn scene_camera(
    world: &hecs::World,
    physics: Option<&PhysicsWorld>,
    viewport: Viewport,
    shake_offset: Vec3,
) -> Option<CameraView> {
    for (entity, (transform, player, camera)) in world
        .query::<(hecs::Entity, (&Transform, &Player, &Camera))>()
        .iter()
    {
        if camera.role != CameraRole::Main {
            continue;
        }
        let mut result = transform.clone();
        if let Ok(mode) = world.get::<&CameraMode>(entity) {
            if let CameraMode::ThirdPerson {
                distance,
                height_offset,
                ..
            } = *mode
            {
                let target = transform.position + Vec3::Y * height_offset;
                let offset = Vec3::new(
                    player.yaw.sin() * player.pitch.cos() * distance,
                    player.pitch.sin() * distance,
                    player.yaw.cos() * player.pitch.cos() * distance,
                );
                let mut desired = target + offset;
                if let Some(physics) = physics {
                    let direction = offset.normalize_or_zero();
                    if let Some((_, toi, _, _)) =
                        physics.raycast_detailed(target, direction, offset.length(), None)
                    {
                        desired = target + direction * (toi - 0.2).max(0.5);
                    }
                }
                let forward = (target - desired).normalize_or_zero();
                result.position = desired + shake_offset;
                result.rotation = if forward.length_squared() > 0.001 {
                    glam::Quat::from_rotation_arc(Vec3::NEG_Z, forward)
                } else {
                    glam::Quat::IDENTITY
                };
                return Some(CameraView::new(camera, &result, viewport));
            }
        }
        result.position += Vec3::Y * player.height * 0.4 + shake_offset;
        result.rotation =
            glam::Quat::from_rotation_y(player.yaw) * glam::Quat::from_rotation_x(player.pitch);
        return Some(CameraView::new(camera, &result, viewport));
    }
    world
        .query::<(&Transform, &Camera)>()
        .iter()
        .find_map(|(transform, camera)| {
            if camera.role != CameraRole::Main {
                return None;
            }
            let mut transform = transform.clone();
            transform.position += shake_offset;
            Some(CameraView::new(camera, &transform, viewport))
        })
}

/// Deterministic camera shake advanced by simulation time.
#[derive(Debug, Clone)]
pub struct CameraShakeState {
    pub intensity: f32,
    pub duration: f32,
    pub timer: f32,
    pub seed: u32,
}

impl CameraShakeState {
    pub fn advance(&mut self, dt: f32) {
        self.timer = (self.timer - dt).max(0.0);
    }

    pub fn offset(&self) -> glam::Vec3 {
        if self.timer <= 0.0 || self.duration <= 0.0 {
            return glam::Vec3::ZERO;
        }
        let scale = self.intensity * self.timer / self.duration;
        let phase = self.timer * 37.0 + self.seed as f32;
        glam::Vec3::new(
            (phase * 7.31).sin(),
            (phase * 13.17).sin(),
            (phase * 23.41).sin(),
        ) * scale
    }

    pub fn new() -> Self {
        Self {
            intensity: 0.0,
            duration: 0.0,
            timer: 0.0,
            seed: 0,
        }
    }
}

impl Default for CameraShakeState {
    fn default() -> Self {
        Self::new()
    }
}

pub const BITMAP_GLYPH_WIDTH: u32 = 6;
pub const BITMAP_GLYPH_HEIGHT: u32 = 8;

/// Monospaced bitmap font geometry; no atlas texture is needed to measure text.
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub glyph_width: f32,
    pub glyph_height: f32,
}
impl Default for FontMetrics {
    fn default() -> Self {
        Self {
            glyph_width: BITMAP_GLYPH_WIDTH as f32,
            glyph_height: BITMAP_GLYPH_HEIGHT as f32,
        }
    }
}
impl FontMetrics {
    pub fn text_width(self, text: &str, size: f32) -> f32 {
        text.chars().filter(|&ch| ch != '\n').count() as f32 * self.glyph_width * size
            / self.glyph_height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiIntent {
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        color: [f32; 4],
    },
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: [f32; 4],
    },
}

/// Immediate-mode script draw list and persistent, simulation-timed screen flash.
#[derive(Debug, Default)]
pub struct UiFrame {
    pub intents: Vec<UiIntent>,
    pub flash_color: [f32; 4],
    pub flash_remaining: f32,
    pub flash_duration: f32,
}
impl UiFrame {
    pub fn set_flash(&mut self, color: [f32; 4], duration: f32) {
        self.flash_color = color;
        self.flash_duration = duration;
        self.flash_remaining = duration.max(0.0);
    }
    pub fn advance(&mut self, dt: f32) {
        self.flash_remaining = (self.flash_remaining - dt).max(0.0);
    }
    pub fn flash(&self, viewport: Viewport) -> Option<UiIntent> {
        (self.flash_remaining > 0.0).then(|| UiIntent::Rect {
            x: 0.0,
            y: 0.0,
            width: viewport.width as f32,
            height: viewport.height as f32,
            color: [
                self.flash_color[0],
                self.flash_color[1],
                self.flash_color[2],
                self.flash_color[3] * self.flash_remaining / self.flash_duration.max(0.001),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_rays_resize_and_missing_camera() {
        let world = hecs::World::new();
        assert!(scene_camera(&world, None, Viewport::default(), Vec3::ZERO).is_none());
        let camera = Camera {
            fov_degrees: 60.0,
            ..Default::default()
        };
        for viewport in [
            Viewport::default(),
            Viewport::new(800, 800),
            Viewport::new(0, 0),
        ] {
            let view = CameraView::new(&camera, &Transform::default(), viewport);
            let (x, y, visible) = view.world_to_screen(Vec3::new(0.0, 0.0, -5.0));
            assert!(visible);
            assert_eq!(
                (x, y),
                (viewport.width as f32 / 2.0, viewport.height as f32 / 2.0)
            );
            let (origin, direction) = view.screen_to_ray(x, y).unwrap();
            assert!((origin - Vec3::new(0.0, 0.0, -0.1)).length() < 1e-5);
            assert!((direction - Vec3::NEG_Z).length() < 1e-5);
            assert!(!view.world_to_screen(Vec3::Z).2);
            let point = Vec3::new(1.0, 0.5, -5.0);
            let (x, y, visible) = view.world_to_screen(point);
            assert!(visible);
            let (origin, direction) = view.screen_to_ray(x, y).unwrap();
            assert!((point - origin).normalize().dot(direction) > 0.99999);
        }
    }
    #[test]
    fn player_camera_priority_offsets_and_wall_avoidance_are_shared() {
        let mut world = hecs::World::new();
        world.spawn((
            Transform {
                position: Vec3::splat(100.0),
                ..Default::default()
            },
            Camera::default(),
        ));
        let player = world.spawn((Transform::default(), Camera::default(), Player::default()));
        let view = scene_camera(&world, None, Viewport::default(), Vec3::ZERO).unwrap();
        assert!((view.transform.position - Vec3::Y * 0.72).length() < 1e-5);
        world
            .insert_one(
                player,
                CameraMode::ThirdPerson {
                    distance: 4.0,
                    height_offset: 1.5,
                    pitch_min: -1.0,
                    pitch_max: 1.0,
                },
            )
            .unwrap();
        let view = scene_camera(&world, None, Viewport::default(), Vec3::X).unwrap();
        assert_eq!(view.transform.position, Vec3::new(1.0, 1.5, 4.0));
        let wall = world.spawn(());
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        physics.add_static_body(
            wall,
            Vec3::new(0.0, 1.5, 2.0),
            glam::Quat::IDENTITY,
            crate::physics::PhysicsShape::Box {
                half_extents: Vec3::new(2.0, 2.0, 0.2),
            },
            false,
            0.0,
            0.5,
        );
        physics.step_fixed();
        let view = scene_camera(&world, Some(&physics), Viewport::default(), Vec3::ZERO).unwrap();
        assert!((view.transform.position.z - 1.6).abs() < 1e-4);
        assert!((view.transform.position.y - 1.5).abs() < 1e-4);
    }

    #[test]
    fn flash_and_font_measurements_are_cpu_state() {
        assert_eq!(FontMetrics::default().text_width("aé\nb", 16.0), 36.0);
        let mut ui = UiFrame::default();
        ui.set_flash([1.0; 4], 1.0);
        ui.advance(0.25);
        let Some(UiIntent::Rect { color, .. }) = ui.flash(Viewport::default()) else {
            panic!()
        };
        assert_eq!(color[3], 0.75);
        ui.advance(1.0);
        assert!(ui.flash(Viewport::default()).is_none());
    }
}
