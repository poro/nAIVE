//! View-frustum culling: leave out of the camera's draws what it cannot see.
//!
//! Each mesh has a bounding sphere in its own space. Per frame the camera's
//! view-projection matrix gives six planes, and an entity is drawn when its
//! sphere, moved and scaled by the entity's world matrix, touches the volume
//! between them. Shadow passes do not cull: an entity outside the view can
//! still cast a shadow into it.

use glam::{Mat4, Vec3, Vec4};

/// A sphere holding every vertex of a mesh, in the mesh's own space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingSphere {
    pub center: Vec3,
    pub radius: f32,
}

impl BoundingSphere {
    /// The sphere centred on the positions' bounding box that reaches the
    /// farthest of them. No positions give a point at the origin.
    pub fn from_positions<I>(positions: I) -> Self
    where
        I: IntoIterator<Item = [f32; 3]>,
        I::IntoIter: Clone,
    {
        let positions = positions.into_iter();
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in positions.clone() {
            let p = Vec3::from_array(p);
            min = min.min(p);
            max = max.max(p);
        }
        if min.x > max.x {
            return Self {
                center: Vec3::ZERO,
                radius: 0.0,
            };
        }
        let center = (min + max) * 0.5;
        let radius = positions
            .map(|p| Vec3::from_array(p).distance(center))
            .fold(0.0_f32, f32::max);
        Self { center, radius }
    }

    /// This sphere under `model`: the centre moves with it, and the radius
    /// grows by its largest axis scale, so the result still holds the mesh.
    pub fn transformed(&self, model: &Mat4) -> (Vec3, f32) {
        let scale = model
            .x_axis
            .truncate()
            .length()
            .max(model.y_axis.truncate().length())
            .max(model.z_axis.truncate().length());
        (model.transform_point3(self.center), self.radius * scale)
    }
}

/// The volume a camera sees, as six planes whose normals point inward.
#[derive(Debug, Clone)]
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    /// The frustum of a view-projection matrix with WebGPU clip space, where
    /// depth runs from 0 at the near plane to 1 at the far plane.
    pub fn from_view_projection(view_projection: &Mat4) -> Self {
        let (r0, r1, r2, r3) = (
            view_projection.row(0),
            view_projection.row(1),
            view_projection.row(2),
            view_projection.row(3),
        );
        let planes = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2].map(|plane| {
            let length = plane.truncate().length();
            if length > 0.0 {
                plane / length
            } else {
                plane
            }
        });
        Self { planes }
    }

    /// Whether any part of the sphere is inside.
    pub fn intersects_sphere(&self, center: Vec3, radius: f32) -> bool {
        self.planes
            .iter()
            .all(|plane| plane.truncate().dot(center) + plane.w >= -radius)
    }

    /// Whether a mesh with these bounds, drawn with this model matrix, can be
    /// seen.
    pub fn sees(&self, bounds: &BoundingSphere, model: &Mat4) -> bool {
        let (center, radius) = bounds.transformed(model);
        self.intersects_sphere(center, radius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera at the origin looking down -Z: 90 degree field of view,
    /// square, near 0.1, far 100.
    fn camera() -> Frustum {
        let view = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        let projection = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            0.1,
            100.0,
        );
        Frustum::from_view_projection(&(projection * view))
    }

    #[test]
    fn spheres_are_tested_against_all_six_planes() {
        let frustum = camera();
        assert!(frustum.intersects_sphere(Vec3::new(0.0, 0.0, -10.0), 1.0));
        // Behind the camera, beyond the far plane, and off each side.
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, 10.0), 1.0));
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, -102.0), 1.0));
        assert!(!frustum.intersects_sphere(Vec3::new(-30.0, 0.0, -10.0), 1.0));
        assert!(!frustum.intersects_sphere(Vec3::new(30.0, 0.0, -10.0), 1.0));
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 30.0, -10.0), 1.0));
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, -30.0, -10.0), 1.0));
        // Centre outside, but the sphere reaches in: still drawn.
        assert!(frustum.intersects_sphere(Vec3::new(-11.5, 0.0, -10.0), 2.0));
        assert!(frustum.intersects_sphere(Vec3::new(0.0, 0.0, 0.5), 1.0));
    }

    #[test]
    fn bounds_follow_the_model_matrix_and_its_largest_scale() {
        let cube = BoundingSphere::from_positions([[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]);
        assert_eq!(cube.center, Vec3::ZERO);
        assert!((cube.radius - 3.0_f32.sqrt()).abs() < 1e-6);
        let model = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 4.0, 1.0),
            glam::Quat::from_rotation_y(0.7),
            Vec3::new(5.0, 0.0, -20.0),
        );
        let (center, radius) = cube.transformed(&model);
        assert!(center.distance(Vec3::new(5.0, 0.0, -20.0)) < 1e-5);
        assert!((radius - 4.0 * 3.0_f32.sqrt()).abs() < 1e-4);

        let frustum = camera();
        assert!(frustum.sees(&cube, &model));
        // A tall, thin object just off screen still reaches in because its
        // sphere is scaled by its largest axis.
        let off_side = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 1.0, 8.0),
            glam::Quat::IDENTITY,
            Vec3::new(-14.0, 0.0, -10.0),
        );
        assert!(frustum.sees(&cube, &off_side));
        let far_away = Mat4::from_translation(Vec3::new(0.0, 0.0, 50.0));
        assert!(!frustum.sees(&cube, &far_away));
        assert_eq!(
            BoundingSphere::from_positions(std::iter::empty::<[f32; 3]>()).radius,
            0.0
        );
    }
}
