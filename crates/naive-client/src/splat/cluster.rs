//! Spatial clusters of splats, so a sort can skip whole groups at once.
//!
//! At upload the splats are put in Morton (Z-curve) order, so splats near
//! each other in the buffer are near each other in space, and every
//! [`CLUSTER`] consecutive splats get a bounding sphere around their centres.
//! Each sort tests the clusters against the view volume the draw keeps splat
//! centres in, then each splat of the clusters that pass, and sorts and draws
//! only the splats left (`sort`).

use glam::{Mat4, Vec3, Vec4};

/// Splats per cluster: one workgroup of the sort's keys pass.
pub const CLUSTER: usize = 256;

/// The draw drops a splat whose centre is beyond this in normalized device
/// coordinates (its footprint can still reach into the view).
pub const NDC_LIMIT: f32 = 1.3;

/// How generous the cull is, relative to the size of the terms it compares,
/// so rounding never culls a splat the draw would keep.
pub const TOLERANCE: f32 = 1e-4;

/// Bits per axis of a Morton code.
const MORTON_BITS: u32 = 10;

/// The order that walks `positions` along a Morton curve through cubic
/// cells over their bounding box (cubes, so the clusters of a flat cloud are
/// not flat). Splats in the same cell keep their original order.
pub fn morton_order(positions: &[[f32; 3]]) -> Vec<u32> {
    let (min, max) = positions.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(min, max), &p| (min.min(p.into()), max.max(p.into())),
    );
    let cells = ((1u32 << MORTON_BITS) - 1) as f32;
    let extent = (max - min).max_element();
    let scale = if extent > 0.0 { cells / extent } else { 0.0 };
    let mut keyed: Vec<u64> = positions
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let cell = ((Vec3::from(p) - min) * scale)
                .clamp(Vec3::ZERO, Vec3::splat(cells))
                .as_uvec3();
            let code = spread(cell.x) | spread(cell.y) << 1 | spread(cell.z) << 2;
            u64::from(code) << 32 | i as u64
        })
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|key| key as u32).collect()
}

/// The low ten bits of `v`, moved to every third bit.
fn spread(v: u32) -> u32 {
    let mut x = v & 0x3FF;
    x = (x | x << 16) & 0x0300_00FF;
    x = (x | x << 8) & 0x0300_F00F;
    x = (x | x << 4) & 0x030C_30C3;
    (x | x << 2) & 0x0924_9249
}

/// A bounding sphere (centre, radius) around the centres of each run of
/// [`CLUSTER`] splats, taking the splats in `order`.
pub fn cluster_bounds(positions: &[[f32; 3]], order: &[u32]) -> Vec<[f32; 4]> {
    order
        .chunks(CLUSTER)
        .map(|cluster| {
            let point = |&i: &u32| Vec3::from(positions[i as usize]);
            let (min, max) = cluster.iter().map(point).fold(
                (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                |(min, max), p| (min.min(p), max.max(p)),
            );
            let centre = (min + max) * 0.5;
            let radius = cluster
                .iter()
                .map(|i| point(i).distance(centre))
                .fold(0.0, f32::max);
            [centre.x, centre.y, centre.z, radius]
        })
        .collect()
}

/// The view volume the draw keeps splat centres in, in the splat asset's
/// space: five planes (n, d) with n·p + d ≥ 0 inside and |n| = 1. The near
/// plane, then the left, right, bottom and top sides at [`NDC_LIMIT`].
///
/// The draw keeps a centre p when its depth is beyond `near` and
/// |clip.xy / clip.w| ≤ [`NDC_LIMIT`], with clip = projection · view ·
/// model · p. In front of the near plane clip.w > 0, so that is
/// |clip.x| ≤ [`NDC_LIMIT`] clip.w, and the same for y: half-spaces.
pub fn view_planes(model: Mat4, view: Mat4, projection: Mat4, near: f32) -> [Vec4; 5] {
    let view_model = view * model;
    let clip = projection * view_model;
    let w = clip.row(3) * NDC_LIMIT;
    [
        // Depth is -z in view space.
        -view_model.row(2) - Vec4::new(0.0, 0.0, 0.0, near),
        w + clip.row(0),
        w - clip.row(0),
        w + clip.row(1),
        w - clip.row(1),
    ]
    .map(|plane| {
        let length = plane.truncate().length();
        if length > 0.0 && length.is_finite() {
            plane / length
        } else {
            plane
        }
    })
}

/// Whether a sphere may hold a centre the draw keeps. It errs on keeping:
/// each plane allows [`TOLERANCE`] of the terms it adds up. The sort's
/// shader makes the same test (`may_be_visible`).
pub fn may_be_visible(planes: &[Vec4; 5], centre: Vec3, radius: f32) -> bool {
    // Written as "not outside" so a NaN plane keeps, as on the GPU.
    !planes.iter().any(|plane| {
        let along = plane.truncate().dot(centre);
        along + plane.w < -radius - TOLERANCE * (along.abs() + plane.w.abs() + radius)
    })
}

/// The draw's own test for a splat centre `p`, in f64: depth beyond the
/// near plane and the centre within [`NDC_LIMIT`]. How far inside it is, in
/// NDC or depth units; negative when outside.
#[cfg(test)]
pub(crate) fn draw_margin(model: Mat4, view: Mat4, projection: Mat4, near: f32, p: Vec3) -> f64 {
    let to_f64 = |m: Mat4| glam::DMat4::from_cols_array(&m.to_cols_array().map(f64::from));
    let view_position = to_f64(view) * to_f64(model) * p.as_dvec3().extend(1.0);
    let depth = -view_position.z;
    let clip = to_f64(projection) * view_position;
    let ndc = clip.truncate().truncate() / clip.w;
    let limit = f64::from(NDC_LIMIT);
    (depth - f64::from(near))
        .min(limit - ndc.x.abs())
        .min(limit - ndc.y.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random(seed: u64) -> impl FnMut() -> f32 {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    #[test]
    fn morton_codes_interleave_the_axes() {
        assert_eq!(spread(0b1), 0b1);
        assert_eq!(spread(0b11), 0b1001);
        assert_eq!(spread(0x3FF), 0x0924_9249);
        // Only ten bits count.
        assert_eq!(spread(0x7FF), spread(0x3FF));
    }

    /// A shuffled grid comes back in compact clusters: each run of 256
    /// splats is an 8 x 8 x 4 block, not a sample of the whole grid.
    #[test]
    fn morton_order_makes_compact_clusters() {
        let mut positions: Vec<[f32; 3]> = (0..64 * 64 * 4)
            .map(|i| [(i % 64) as f32, (i / 64 % 64) as f32, (i / 4096) as f32])
            .collect();
        let mut next = random(3);
        for i in (1..positions.len()).rev() {
            positions.swap(i, (next() * (i + 1) as f32) as usize % (i + 1));
        }
        let order = morton_order(&positions);
        let mut seen = vec![false; positions.len()];
        order.iter().for_each(|&i| seen[i as usize] = true);
        assert!(seen.iter().all(|&s| s), "a permutation");

        let bounds = cluster_bounds(&positions, &order);
        assert_eq!(bounds.len(), 64);
        let widest = bounds.iter().map(|b| b[3]).fold(0.0, f32::max);
        // Half the diagonal of a 7 x 7 x 3 box.
        assert!(widest <= 5.3, "widest cluster {widest}");
        let unordered: Vec<u32> = (0..positions.len() as u32).collect();
        let shuffled = cluster_bounds(&positions, &unordered);
        assert!(shuffled.iter().all(|b| b[3] > 30.0), "the grid's spread");
    }

    #[test]
    fn morton_order_handles_flat_and_tiny_clouds() {
        assert!(morton_order(&[]).is_empty());
        assert_eq!(morton_order(&[[1.0, 2.0, 3.0]; 3]), [0, 1, 2]);
        let flat: Vec<[f32; 3]> = (0..10).rev().map(|i| [i as f32, 0.0, 0.0]).collect();
        assert_eq!(morton_order(&flat), (0..10).rev().collect::<Vec<u32>>());
    }

    #[test]
    fn cluster_spheres_hold_their_splats() {
        let mut next = random(9);
        let positions: Vec<[f32; 3]> = (0..1000)
            .map(|_| [next() * 50.0 - 25.0, next() * 3.0, next() * 400.0])
            .collect();
        let order = morton_order(&positions);
        let bounds = cluster_bounds(&positions, &order);
        assert_eq!(bounds.len(), 4, "the last cluster is partial");
        for (cluster, sphere) in order.chunks(CLUSTER).zip(&bounds) {
            let centre = Vec3::new(sphere[0], sphere[1], sphere[2]);
            for &i in cluster {
                let distance = Vec3::from(positions[i as usize]).distance(centre);
                assert!(distance <= sphere[3], "{distance} > {}", sphere[3]);
            }
        }
    }

    fn cameras() -> Vec<(Mat4, Mat4, Mat4, f32)> {
        let model = Mat4::from_scale_rotation_translation(
            Vec3::new(1.0, 2.0, 0.5),
            glam::Quat::from_rotation_y(0.7) * glam::Quat::from_rotation_x(0.2),
            Vec3::new(-2.0, 0.5, 1.0),
        );
        let view = glam::camera::rh::view::look_at_mat4(
            Vec3::new(3.0, 4.0, 12.0),
            Vec3::new(0.0, 1.0, -3.0),
            Vec3::Y,
        );
        let wide = glam::camera::rh::proj::directx::perspective(1.2, 16.0 / 9.0, 0.1, 100.0);
        let narrow = glam::camera::rh::proj::directx::perspective(0.3, 0.5, 2.0, 50.0);
        let ortho =
            glam::camera::rh::proj::directx::orthographic(-8.0, 8.0, -5.0, 5.0, -20.0, 20.0);
        vec![
            (model, view, wide, 0.1),
            (Mat4::IDENTITY, view, narrow, 2.0),
            (model, view, ortho, -20.0),
        ]
    }

    /// The planes keep every centre the draw keeps, and only add centres
    /// within a hair of its limits.
    #[test]
    fn view_planes_keep_what_the_draw_keeps() {
        let mut next = random(21);
        for (model, view, projection, near) in cameras() {
            let planes = view_planes(model, view, projection, near);
            let mut kept = 0;
            for _ in 0..20_000 {
                let p = Vec3::new(next(), next(), next()) * 40.0 - Vec3::splat(20.0);
                let margin = draw_margin(model, view, projection, near, p);
                let culled = !may_be_visible(&planes, p, 0.0);
                assert!(
                    !(margin >= 0.0 && culled),
                    "culled {p} the draw keeps ({margin})"
                );
                if !culled {
                    kept += 1;
                    assert!(margin > -1e-3, "kept {p}, {margin} outside");
                }
            }
            assert!(kept > 100, "only {kept} kept");
        }
    }

    /// A sphere is only culled when no point in it would be drawn.
    #[test]
    fn spheres_are_culled_only_when_wholly_outside() {
        let mut next = random(5);
        for (model, view, projection, near) in cameras() {
            let planes = view_planes(model, view, projection, near);
            let mut culled = 0;
            for _ in 0..2_000 {
                let centre = Vec3::new(next(), next(), next()) * 60.0 - Vec3::splat(30.0);
                let radius = next() * 4.0;
                if may_be_visible(&planes, centre, radius) {
                    continue;
                }
                culled += 1;
                for _ in 0..200 {
                    let direction = Vec3::new(next(), next(), next()) * 2.0 - Vec3::ONE;
                    let p = centre + direction.normalize_or_zero() * radius * next().sqrt();
                    let margin = draw_margin(model, view, projection, near, p);
                    assert!(margin < 0.0, "culled sphere holds {p} ({margin})");
                }
            }
            assert!(culled > 100, "only {culled} culled");
        }
    }
}
