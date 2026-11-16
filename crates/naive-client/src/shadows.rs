//! Cascaded shadow maps for the directional light.
//!
//! The camera's view is cut by distance into [`CASCADES`] slices, nearest
//! first and growing with distance. Each slice gets its own orthographic
//! shadow camera, rendered into one quarter of the shadow map, so the area
//! near the viewer gets most of the shadow resolution. Cascade `i` covers
//! view depths up to `splits[i]` and uses tile `(i % 2, i / 2)` of the map.
//!
//! Shadows reach `2 * shadow_extent` from the camera (or its far plane, if
//! nearer), and objects up to `2 * shadow_extent` toward the light from a
//! slice still cast shadows into it.

use glam::{Mat4, Vec3, Vec4Swizzles};

/// Number of cascades. The shadow map holds them as a 2 by 2 grid of tiles.
pub const CASCADES: usize = 4;

/// Per-cascade shadow cameras and the view depth where each one ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cascades {
    pub view_projections: [Mat4; CASCADES],
    pub splits: [f32; CASCADES],
    /// Depth bias for each cascade, in its shadow-map depth units: about two
    /// texels plus a centimetre of world distance, so surfaces do not shadow
    /// themselves while objects resting on a surface still shadow it.
    pub depth_biases: [f32; CASCADES],
}

/// How much of the split distribution is logarithmic rather than even. More
/// logarithmic puts finer cascades near the camera.
const LOG_WEIGHT: f32 = 0.75;

/// Fit cascades to a camera.
///
/// `inv_view_projection`, `near` and `far` describe the camera, whose clip
/// depth runs from 0 at `near` to 1 at `far`. `tile_resolution` is the width
/// of one cascade's tile in texels; shadow cameras move in whole texels so
/// that shadow edges stay still while the camera moves.
pub fn fit_cascades(
    inv_view_projection: Mat4,
    near: f32,
    far: f32,
    light_direction: Vec3,
    shadow_extent: f32,
    tile_resolution: u32,
) -> Cascades {
    let dir = light_direction.try_normalize().unwrap_or(Vec3::NEG_Y);
    let extent = shadow_extent.max(0.01);
    let near = near.max(0.001);
    let far = far.max(near * 1.001);
    let shadow_far = far.min(2.0 * extent).max(near * 1.001);

    let mut splits = [0.0; CASCADES];
    for (i, split) in splits.iter_mut().enumerate() {
        let t = (i + 1) as f32 / CASCADES as f32;
        let even = near + (shadow_far - near) * t;
        let log = near * (shadow_far / near).powf(t);
        *split = LOG_WEIGHT * log + (1.0 - LOG_WEIGHT) * even;
    }
    splits[CASCADES - 1] = shadow_far;

    // The view volume's corners on the near and far planes. View depth is
    // linear along each line from a near corner to its far corner.
    let unproject = |x: f32, y: f32, z: f32| {
        let p = inv_view_projection * glam::Vec4::new(x, y, z, 1.0);
        p.xyz() / p.w
    };
    let edges: Vec<(Vec3, Vec3)> = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .iter()
        .map(|&(x, y)| (unproject(x, y, 0.0), unproject(x, y, 1.0)))
        .collect();
    let at_depth = |depth: f32| {
        let t = (depth - near) / (far - near);
        edges.iter().map(move |&(a, b)| a + (b - a) * t)
    };

    // look_at needs an up vector that is not parallel to the light.
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let light_rotation = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, dir, up);
    let caster_margin = 2.0 * extent;

    let mut view_projections = [Mat4::IDENTITY; CASCADES];
    let mut depth_biases = [0.0; CASCADES];
    for i in 0..CASCADES {
        let start = if i == 0 { near } else { splits[i - 1] };
        let corners: Vec<Vec3> = at_depth(start).chain(at_depth(splits[i])).collect();
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        let radius = corners
            .iter()
            .map(|c| c.distance(center))
            .fold(0.0_f32, f32::max);
        // A sphere's size does not change as the camera turns; rounding it
        // keeps it from changing with tiny float differences.
        let radius = (radius * 16.0).ceil() / 16.0;

        // A one-texel border, so moving the centre onto whole texels below
        // cannot push the edge of the sphere out of the map.
        let resolution = tile_resolution.max(4) as f32;
        let radius = radius * resolution / (resolution - 2.0);
        // Move the centre in whole shadow-map texels, across the light.
        let texel = 2.0 * radius / resolution;
        let mut in_light = light_rotation.transform_point3(center);
        in_light.x = (in_light.x / texel).floor() * texel;
        in_light.y = (in_light.y / texel).floor() * texel;
        let center = light_rotation.inverse().transform_point3(in_light);

        let eye = center - dir * (radius + caster_margin);
        let view = glam::camera::rh::view::look_at_mat4(eye, center, up);
        let depth_range = 2.0 * radius + caster_margin;
        let projection = glam::camera::rh::proj::directx::orthographic(
            -radius,
            radius,
            -radius,
            radius,
            0.0,
            depth_range,
        );
        view_projections[i] = projection * view;
        depth_biases[i] = (2.0 * texel + 0.01) / depth_range;
    }

    Cascades {
        view_projections,
        splits,
        depth_biases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(position: Vec3, forward: Vec3) -> (Mat4, Mat4) {
        // look_to needs a unit direction.
        let view = glam::camera::rh::view::look_to_mat4(position, forward.normalize(), Vec3::Y);
        let projection = glam::camera::rh::proj::directx::perspective(
            70f32.to_radians(),
            16.0 / 9.0,
            0.1,
            200.0,
        );
        (view, projection * view)
    }

    #[test]
    fn every_slice_of_the_view_is_inside_its_cascade() {
        let (view, view_projection) = camera(Vec3::new(3.0, 2.0, 8.0), Vec3::new(0.3, -0.2, -1.0));
        let cascades = fit_cascades(
            view_projection.inverse(),
            0.1,
            200.0,
            Vec3::new(0.3, -1.0, 0.5),
            20.0,
            2048,
        );
        // Splits grow and the last one is where shadows end (2 * extent).
        assert!(cascades.splits.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(cascades.splits[CASCADES - 1], 40.0);
        assert!(cascades.splits[0] < 5.0, "{:?}", cascades.splits);

        // Points spread through each slice land inside that cascade's box.
        let inv = view_projection.inverse();
        let inv_view = view.inverse();
        for i in 0..CASCADES {
            let start = if i == 0 { 0.1 } else { cascades.splits[i - 1] };
            for depth in [
                start,
                (start + cascades.splits[i]) * 0.5,
                cascades.splits[i],
            ] {
                for (x, y) in [(-1.0, -1.0), (1.0, 1.0), (0.0, 0.0), (1.0, -1.0)] {
                    // The point at this view depth on the ray through (x, y).
                    let far = inv * glam::Vec4::new(x, y, 1.0, 1.0);
                    let far = far.xyz() / far.w;
                    let eye = inv_view.transform_point3(Vec3::ZERO);
                    let ray = (far - eye).normalize();
                    let forward = inv_view.transform_vector3(Vec3::NEG_Z);
                    let point = eye + ray * (depth / ray.dot(forward));
                    let clip = cascades.view_projections[i] * point.extend(1.0);
                    let ndc = clip.xyz() / clip.w;
                    assert!(
                        ndc.x.abs() <= 1.0001
                            && ndc.y.abs() <= 1.0001
                            && (0.0..=1.0).contains(&ndc.z),
                        "cascade {i} misses depth {depth}: {ndc}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_light_pointing_straight_down_works() {
        let (_, view_projection) = camera(Vec3::ZERO, Vec3::NEG_Z);
        let cascades = fit_cascades(
            view_projection.inverse(),
            0.1,
            200.0,
            Vec3::NEG_Y,
            20.0,
            2048,
        );
        assert!(cascades.view_projections.iter().all(|m| m.is_finite()));
    }

    #[test]
    fn shadow_cameras_move_in_whole_texels() {
        let fit = |x: f32| {
            let (_, view_projection) = camera(Vec3::new(x, 2.0, 8.0), Vec3::NEG_Z);
            fit_cascades(
                view_projection.inverse(),
                0.1,
                200.0,
                Vec3::new(0.0, -1.0, 0.0001),
                20.0,
                1024,
            )
        };
        let a = fit(0.0);
        let b = fit(0.013);
        for i in 0..CASCADES {
            // The light looks straight down, so a sideways camera move shifts
            // the shadow camera by a whole number of texels or not at all.
            let shift = (b.view_projections[i].w_axis.x - a.view_projections[i].w_axis.x) * 512.0;
            assert!(
                (shift - shift.round()).abs() < 1e-2,
                "cascade {i} moved {shift} texels"
            );
        }
    }
}
