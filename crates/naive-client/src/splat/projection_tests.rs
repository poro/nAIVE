//! The splat pass's projection on a real GPU, against an independent CPU
//! version of 3D Gaussian Splatting's.
//!
//! Splats go through the engine's own upload, sort, pipeline
//! (`create_splat_pipeline`) and bind group, are drawn offscreen, and every
//! pixel is compared with the reference. The reference works in f64 and
//! shares no code with the shader: the projection's Jacobian is taken by
//! finite differences of the camera's own matrices, the rotation comes from
//! glam's quaternion, and each pixel's alpha is opacity · exp(-½ dᵀ Σ⁻¹ d)
//! at its centre, with Σ the splat's covariance carried to the screen plus
//! 0.3 px² of low-pass, as in 3DGS. Splats are composited farthest first.
//!
//! `cargo test -p naive-client --lib -- --ignored gpu_splat_projection`

use std::collections::HashMap;

use glam::{DMat2, DMat3, DMat4, DQuat, DVec2, DVec3, Mat4, Quat, Vec3};
use naive_sim::assets::{SplatData, SplatPoint};

use super::sort::{SplatCamera, SplatSorter};
use crate::camera::{CameraState, CameraUniform};
use crate::components::{Camera, SplatHandle, Transform};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;
// The draw's limits.
const MIN_ALPHA: f64 = 1.0 / 255.0;
const MAX_ALPHA: f64 = 0.99;
const LOW_PASS: f64 = 0.3;
const NDC_LIMIT: f64 = 1.3;

/// Premultiplied RGBA per pixel, rows from the top.
type Image = Vec<[f64; 4]>;

fn f64_matrix(m: [[f32; 4]; 4]) -> DMat4 {
    DMat4::from_cols_array_2d(&m.map(|column| column.map(f64::from)))
}

fn f64_vector(v: [f32; 3]) -> DVec3 {
    DVec3::from(v.map(f64::from))
}

/// What the draw should leave over a transparent black background.
fn reference(splats: &[SplatPoint], model: Mat4, camera: &CameraUniform) -> Image {
    let view = f64_matrix(camera.view);
    let projection = f64_matrix(camera.projection);
    let model = f64_matrix(model.to_cols_array_2d());
    let size = DVec2::new(f64::from(WIDTH), f64::from(HEIGHT));
    // A view-space point's position in pixels, x right and y down.
    let pixel = |p: DVec3| {
        let clip = projection * p.extend(1.0);
        let ndc = clip.truncate().truncate() / clip.w;
        DVec2::new((ndc.x + 1.0) * 0.5 * size.x, (1.0 - ndc.y) * 0.5 * size.y)
    };
    let centre =
        |splat: &SplatPoint| (view * model * f64_vector(splat.position).extend(1.0)).truncate();

    let mut farthest_first: Vec<&SplatPoint> = splats.iter().collect();
    farthest_first.sort_by(|a, b| centre(a).z.total_cmp(&centre(b).z));

    let mut image = vec![[0.0; 4]; (WIDTH * HEIGHT) as usize];
    for splat in farthest_first {
        let opacity = f64::from(splat.opacity).min(MAX_ALPHA);
        let centre = centre(splat);
        let clip = projection * centre.extend(1.0);
        let ndc = clip.truncate().truncate() / clip.w;
        let in_view = -centre.z > f64::from(camera.near_plane)
            && ndc.x.abs() <= NDC_LIMIT
            && ndc.y.abs() <= NDC_LIMIT;
        if opacity < MIN_ALPHA || !in_view {
            continue;
        }

        // The splat's covariance in view space: (W M R S)(W M R S)ᵀ.
        let [w, x, y, z] = splat.rotation.map(f64::from);
        let rotation = DMat3::from_quat(DQuat::from_xyzw(x, y, z, w).normalize());
        let axes = DMat3::from_mat4(view)
            * DMat3::from_mat4(model)
            * rotation
            * DMat3::from_diagonal(f64_vector(splat.scale));
        let covariance = axes * axes.transpose();

        // The projection's Jacobian at the centre (2 x 3), by central
        // differences, then the covariance on the screen: J Σ Jᵀ.
        let h = 1e-6 * -centre.z;
        let jacobian: [DVec2; 3] = std::array::from_fn(|k| {
            let mut step = DVec3::ZERO;
            step[k] = h;
            (pixel(centre + step) - pixel(centre - step)) / (2.0 * h)
        });
        let mut screen = [[0.0; 2]; 2];
        for (a, row) in screen.iter_mut().enumerate() {
            for (b, value) in row.iter_mut().enumerate() {
                for k in 0..3 {
                    for l in 0..3 {
                        *value += jacobian[k][a] * covariance.col(l)[k] * jacobian[l][b];
                    }
                }
            }
        }
        let inverse = DMat2::from_cols(
            DVec2::new(screen[0][0] + LOW_PASS, screen[1][0]),
            DVec2::new(screen[0][1], screen[1][1] + LOW_PASS),
        )
        .inverse();

        let at = pixel(centre);
        let color = splat.sh_dc.map(|c| f64::from(c).clamp(0.0, 1.0));
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let d = DVec2::new(f64::from(column) + 0.5, f64::from(row) + 0.5) - at;
                let alpha = opacity * (-0.5 * d.dot(inverse * d)).exp();
                if alpha < MIN_ALPHA {
                    continue;
                }
                let out = &mut image[(row * WIDTH + column) as usize];
                for c in 0..3 {
                    out[c] = color[c] * alpha + out[c] * (1.0 - alpha);
                }
                out[3] = alpha + out[3] * (1.0 - alpha);
            }
        }
    }
    image
}

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("no GPU adapter");
    pollster::block_on(adapter.request_device(&Default::default())).unwrap()
}

/// Draw `splats` as an entity at `model`, seen by `camera` at `eye`, the way
/// the engine does; and the camera uniform it drew with.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    splats: Vec<SplatPoint>,
    model: Mat4,
    camera: &Camera,
    eye: &Transform,
) -> (Image, CameraUniform) {
    let mut camera_state = CameraState::new(device);
    camera_state.update(queue, camera, eye, WIDTH, HEIGHT);
    let (layout, pipeline) = crate::pipeline::compiler::create_splat_pipeline(
        device,
        &crate::shader::get_splat_render_wgsl(),
        &["splat_color".to_string()],
        None,
        &HashMap::new(),
        &camera_state.bind_group_layout,
    );
    let splat = super::upload_splat(
        device,
        "test",
        SplatData {
            splats,
            sh_degree: 0,
            sh_rest: Vec::new(),
        },
    )
    .unwrap();
    let sorter = SplatSorter::new(device);
    let mut instance = sorter.instance(device, &splat, SplatHandle(0));
    instance.update(queue, model, SplatCamera::from(&camera_state.uniform));

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Splat projection test"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // What the pipeline makes for a target it has no resource for.
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let bytes_per_row = WIDTH * 8;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bytes_per_row * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        sorter.encode(&mut pass, &instance);
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &camera_state.bind_group, &[]);
        pass.set_bind_group(1, &instance.draw_bind_group(device, &layout, &splat), &[]);
        instance.draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.unwrap());
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("GPU poll");
    let bytes = readback
        .slice(..)
        .get_mapped_range()
        .expect("mapped readback")
        .to_vec();
    let halves: &[u16] = bytemuck::cast_slice(&bytes);
    let image = halves
        .as_chunks::<4>()
        .0
        .iter()
        .map(|texel| texel.map(|bits| f64::from(half::f16::from_bits(bits).to_f32())))
        .collect();
    (image, camera_state.uniform)
}

fn splat(position: [f32; 3], scale: [f32; 3], rotation: Quat) -> SplatPoint {
    SplatPoint {
        position,
        opacity: 0.9,
        scale,
        // 3DGS order: w, x, y, z.
        rotation: [rotation.w, rotation.x, rotation.y, rotation.z],
        sh_dc: [0.2, 0.6, 0.9],
    }
}

struct Case {
    name: &'static str,
    splats: Vec<SplatPoint>,
    model: Mat4,
    eye: Transform,
    /// Pixels the reference covers with alpha above 0.01, at least.
    covers: usize,
}

fn eye(position: Vec3, target: Vec3) -> Transform {
    Transform {
        position,
        rotation: Quat::from_rotation_arc(Vec3::NEG_Z, (target - position).normalize()),
        ..Default::default()
    }
}

fn cases() -> Vec<Case> {
    let front = eye(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
    let tilted = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 1.2);
    // At depth 6, the view's half-width is tan(30°) · 16/9 · 6.
    let half_width = 30f32.to_radians().tan() * 16.0 / 9.0 * 6.0;
    let mut faint = splat([0.0; 3], [0.2; 3], Quat::IDENTITY);
    faint.opacity = 0.003;
    vec![
        Case {
            name: "round splat in the middle",
            splats: vec![splat([0.0; 3], [0.15; 3], Quat::IDENTITY)],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 350,
        },
        Case {
            // The draw caps opacity at 0.99, as 3DGS does.
            name: "fully opaque splat",
            splats: vec![SplatPoint {
                opacity: 1.0,
                ..splat([0.0; 3], [0.15; 3], Quat::IDENTITY)
            }],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 350,
        },
        Case {
            // A w-first quaternion read in the wrong order turns it wrongly.
            name: "long splat turned 35 degrees about the view axis",
            splats: vec![splat(
                [0.0; 3],
                [0.5, 0.06, 0.06],
                Quat::from_rotation_z(35f32.to_radians()),
            )],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 500,
        },
        Case {
            name: "flat splat turned about an oblique axis",
            splats: vec![splat([0.2, -0.1, 0.3], [0.4, 0.15, 0.03], tilted)],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 800,
        },
        Case {
            // Far from the axis the Jacobian's slope terms matter.
            name: "splat near the corner of the view",
            splats: vec![splat(
                [4.5, 2.5, -1.0],
                [0.3, 0.1, 0.2],
                Quat::from_rotation_y(0.35),
            )],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 400,
        },
        Case {
            name: "centre just off screen, footprint reaching in",
            splats: vec![splat(
                [1.15 * half_width, 0.0, -1.0],
                [0.8; 3],
                Quat::IDENTITY,
            )],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 3500,
        },
        Case {
            // The draw drops a centre beyond 1.3 NDC even if its footprint
            // would reach in.
            name: "centre beyond the draw's limit",
            splats: vec![splat(
                [1.4 * half_width, 0.0, -1.0],
                [0.8; 3],
                Quat::IDENTITY,
            )],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 0,
        },
        Case {
            // The 0.3 px² low-pass makes a speck about a pixel wide.
            name: "splat smaller than a pixel",
            splats: vec![splat([0.01, 0.02, 0.0], [0.002; 3], Quat::IDENTITY)],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 6,
        },
        Case {
            name: "entity turned and stretched",
            splats: vec![splat([0.3, 0.2, 0.1], [0.1; 3], Quat::IDENTITY)],
            model: Mat4::from_scale_rotation_translation(
                Vec3::new(2.0, 0.5, 1.0),
                Quat::from_rotation_y(0.5) * Quat::from_rotation_x(0.35),
                Vec3::new(-1.0, 0.5, 0.0),
            ),
            eye: front.clone(),
            covers: 150,
        },
        Case {
            name: "camera above and to the side",
            splats: vec![splat([0.0; 3], [0.3, 0.08, 0.15], tilted)],
            model: Mat4::IDENTITY,
            eye: eye(Vec3::new(3.0, 2.0, 4.0), Vec3::ZERO),
            covers: 350,
        },
        Case {
            // Drawn farthest first, each over the ones behind it.
            name: "overlapping splats at three depths",
            splats: vec![
                splat(
                    [0.0, 0.0, 1.0],
                    [0.25, 0.12, 0.1],
                    Quat::from_rotation_z(0.4),
                ),
                splat([0.3, 0.1, 0.0], [0.3; 3], Quat::IDENTITY),
                SplatPoint {
                    sh_dc: [0.9, 0.3, 0.1],
                    opacity: 0.6,
                    ..splat([0.6, 0.2, -1.0], [0.2, 0.5, 0.2], Quat::IDENTITY)
                },
            ],
            model: Mat4::IDENTITY,
            eye: front.clone(),
            covers: 1700,
        },
        Case {
            name: "too faint, and behind the camera",
            splats: vec![faint, splat([0.0, 0.0, 7.0], [0.5; 3], Quat::IDENTITY)],
            model: Mat4::IDENTITY,
            eye: front,
            covers: 0,
        },
    ]
}

/// Every case's draw matches the reference to within the render target's
/// half-float precision, pixel by pixel.
#[test]
#[ignore = "needs a GPU"]
fn gpu_splat_projection_matches_the_3dgs_reference() {
    let (device, queue) = gpu();
    let camera = Camera {
        fov_degrees: 60.0,
        ..Default::default()
    };
    for case in cases() {
        let (drawn, uniform) = render(
            &device,
            &queue,
            case.splats.clone(),
            case.model,
            &camera,
            &case.eye,
        );
        let expected = reference(&case.splats, case.model, &uniform);
        let covered = expected.iter().filter(|p| p[3] > 0.01).count();
        assert!(
            covered >= case.covers && (case.covers > 0 || covered == 0),
            "{}: the reference covers {covered} pixels",
            case.name
        );
        let mut worst = (0.0, 0, 0);
        for (i, (got, want)) in drawn.iter().zip(&expected).enumerate() {
            // A pixel this close to the 1/255 cutoff may land either side.
            if got[3] < 2.0 * MIN_ALPHA && want[3] < 2.0 * MIN_ALPHA {
                continue;
            }
            for c in 0..4 {
                let difference = (got[c] - want[c]).abs();
                if difference > worst.0 {
                    worst = (difference, i, c);
                }
            }
        }
        let (difference, i, c) = worst;
        assert!(
            difference < 3e-3,
            "{}: pixel ({}, {}) channel {c} drawn {:.4}, expected {:.4}",
            case.name,
            i as u32 % WIDTH,
            i as u32 / WIDTH,
            drawn[i][c],
            expected[i][c]
        );
    }
}
