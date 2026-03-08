/// Debug wireframe renderer for physics collider visualization.
/// Toggle with 'H' key. Draws world-space wireframe lines over the 3D scene.

use glam::{Vec3, Quat};
use wgpu::util::DeviceExt;

use crate::camera::CameraState;
use crate::physics::PhysicsWorld;

// ── Vertex ──────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct DebugVertex {
    position: [f32; 3],
    color: [f32; 4],
}

const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<DebugVertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 12,
            shader_location: 1,
            format: wgpu::VertexFormat::Float32x4,
        },
    ],
};

// ── WGSL shader ─────────────────────────────────────────────────────

const DEBUG_WGSL: &str = r#"
struct Camera {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    view_projection: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) col: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) col: vec4<f32>,
};

@vertex fn vs(v: VIn) -> VOut {
    var o: VOut;
    o.clip = camera.view_projection * vec4<f32>(v.pos, 1.0);
    o.col = v.col;
    return o;
}
@fragment fn fs(v: VOut) -> @location(0) vec4<f32> {
    return v.col;
}
"#;

// ── Colors ──────────────────────────────────────────────────────────

const COLOR_BOX: [f32; 4] = [0.0, 1.0, 1.0, 0.8];       // cyan
const COLOR_SPHERE: [f32; 4] = [0.0, 1.0, 0.3, 0.8];     // green
const COLOR_CAPSULE: [f32; 4] = [1.0, 1.0, 0.0, 0.8];    // yellow
const COLOR_TRIMESH: [f32; 4] = [1.0, 0.3, 1.0, 0.8];    // magenta

// ── Renderer ────────────────────────────────────────────────────────

pub struct DebugDrawRenderer {
    pipeline: wgpu::RenderPipeline,
}

impl DebugDrawRenderer {
    pub fn new(
        device: &wgpu::Device,
        camera_bind_group_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Debug Wireframe Shader"),
            source: wgpu::ShaderSource::Wgsl(DEBUG_WGSL.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Debug Wireframe Pipeline Layout"),
            bind_group_layouts: &[camera_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Debug Wireframe Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[VERTEX_LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        Self { pipeline }
    }

    /// Render wireframe colliders by reading shapes directly from the Rapier physics world.
    pub fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        camera_state: &CameraState,
        physics_world: &PhysicsWorld,
    ) {
        let mut vertices: Vec<DebugVertex> = Vec::new();

        // Iterate all colliders in the physics world
        for (_handle, collider) in physics_world.collider_set.iter() {
            let pos_iso = collider.position();
            let pos = Vec3::new(
                pos_iso.translation.x,
                pos_iso.translation.y,
                pos_iso.translation.z,
            );
            let rot = Quat::from_xyzw(
                pos_iso.rotation.i,
                pos_iso.rotation.j,
                pos_iso.rotation.k,
                pos_iso.rotation.w,
            );

            let shape = collider.shape();
            draw_shape(&mut vertices, pos, rot, shape);
        }

        if vertices.is_empty() {
            return;
        }

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Debug Wireframe VB"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Debug Wireframe Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &camera_state.bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
    }
}

// ── Draw shapes from Rapier directly ────────────────────────────────

fn draw_shape(
    verts: &mut Vec<DebugVertex>,
    pos: Vec3,
    rot: Quat,
    shape: &dyn rapier3d::parry::shape::Shape,
) {
    use rapier3d::parry::shape::ShapeType;

    let transform = move |local: Vec3| -> Vec3 {
        pos + rot * local
    };

    match shape.shape_type() {
        ShapeType::Cuboid => {
            if let Some(cuboid) = shape.as_cuboid() {
                let h = Vec3::new(cuboid.half_extents.x, cuboid.half_extents.y, cuboid.half_extents.z);
                push_box_wireframe(verts, &transform, h, COLOR_BOX);
            }
        }
        ShapeType::Ball => {
            if let Some(ball) = shape.as_ball() {
                push_sphere_wireframe(verts, &transform, ball.radius, COLOR_SPHERE);
            }
        }
        ShapeType::Capsule => {
            if let Some(capsule) = shape.as_capsule() {
                let half_height = capsule.half_height();
                let radius = capsule.radius;
                push_capsule_wireframe(verts, &transform, half_height, radius, COLOR_CAPSULE);
            }
        }
        ShapeType::TriMesh => {
            if let Some(trimesh) = shape.as_trimesh() {
                let vertices = trimesh.vertices();
                let indices = trimesh.indices();
                for tri in indices {
                    let a = transform(Vec3::new(vertices[tri[0] as usize].x, vertices[tri[0] as usize].y, vertices[tri[0] as usize].z));
                    let b = transform(Vec3::new(vertices[tri[1] as usize].x, vertices[tri[1] as usize].y, vertices[tri[1] as usize].z));
                    let c = transform(Vec3::new(vertices[tri[2] as usize].x, vertices[tri[2] as usize].y, vertices[tri[2] as usize].z));
                    push_line(verts, a, b, COLOR_TRIMESH);
                    push_line(verts, b, c, COLOR_TRIMESH);
                    push_line(verts, c, a, COLOR_TRIMESH);
                }
            }
        }
        ShapeType::Compound => {
            if let Some(compound) = shape.as_compound() {
                for (sub_iso, sub_shape) in compound.shapes() {
                    let sub_pos = pos + rot * Vec3::new(sub_iso.translation.x, sub_iso.translation.y, sub_iso.translation.z);
                    let sub_rot_q = Quat::from_xyzw(sub_iso.rotation.i, sub_iso.rotation.j, sub_iso.rotation.k, sub_iso.rotation.w);
                    let sub_rot = rot * sub_rot_q;
                    draw_shape(verts, sub_pos, sub_rot, sub_shape.as_ref());
                }
            }
        }
        ShapeType::ConvexPolyhedron => {
            if let Some(convex) = shape.as_convex_polyhedron() {
                let points = convex.points();
                // Draw edges from face topology
                for face in convex.faces() {
                    let first = face.first_vertex_or_edge as usize;
                    let count = face.num_vertices_or_edges as usize;
                    for j in 0..count {
                        let idx_a = convex.vertices_adj_to_face()[first + j] as usize;
                        let idx_b = convex.vertices_adj_to_face()[first + (j + 1) % count] as usize;
                        if idx_a < points.len() && idx_b < points.len() {
                            let a = transform(Vec3::new(points[idx_a].x, points[idx_a].y, points[idx_a].z));
                            let b = transform(Vec3::new(points[idx_b].x, points[idx_b].y, points[idx_b].z));
                            push_line(verts, a, b, COLOR_TRIMESH);
                        }
                    }
                }
            }
        }
        _ => {
            // Unsupported shape — skip
        }
    }
}

// ── Shape wireframe generators ──────────────────────────────────────

fn push_line(verts: &mut Vec<DebugVertex>, a: Vec3, b: Vec3, color: [f32; 4]) {
    verts.push(DebugVertex { position: a.into(), color });
    verts.push(DebugVertex { position: b.into(), color });
}

fn push_box_wireframe(
    verts: &mut Vec<DebugVertex>,
    transform: &dyn Fn(Vec3) -> Vec3,
    half: Vec3,
    color: [f32; 4],
) {
    let corners = [
        Vec3::new(-half.x, -half.y, -half.z),
        Vec3::new( half.x, -half.y, -half.z),
        Vec3::new( half.x,  half.y, -half.z),
        Vec3::new(-half.x,  half.y, -half.z),
        Vec3::new(-half.x, -half.y,  half.z),
        Vec3::new( half.x, -half.y,  half.z),
        Vec3::new( half.x,  half.y,  half.z),
        Vec3::new(-half.x,  half.y,  half.z),
    ];
    let world: Vec<Vec3> = corners.iter().map(|c| transform(*c)).collect();

    let edges = [
        (0,1),(1,2),(2,3),(3,0),
        (4,5),(5,6),(6,7),(7,4),
        (0,4),(1,5),(2,6),(3,7),
    ];
    for (a, b) in edges {
        push_line(verts, world[a], world[b], color);
    }
}

fn push_sphere_wireframe(
    verts: &mut Vec<DebugVertex>,
    transform: &dyn Fn(Vec3) -> Vec3,
    radius: f32,
    color: [f32; 4],
) {
    let segments = 24;
    for ring in 0..3 {
        for i in 0..segments {
            let a0 = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let a1 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;
            let (p0, p1) = match ring {
                0 => (
                    Vec3::new(a0.cos() * radius, a0.sin() * radius, 0.0),
                    Vec3::new(a1.cos() * radius, a1.sin() * radius, 0.0),
                ),
                1 => (
                    Vec3::new(a0.cos() * radius, 0.0, a0.sin() * radius),
                    Vec3::new(a1.cos() * radius, 0.0, a1.sin() * radius),
                ),
                _ => (
                    Vec3::new(0.0, a0.cos() * radius, a0.sin() * radius),
                    Vec3::new(0.0, a1.cos() * radius, a1.sin() * radius),
                ),
            };
            push_line(verts, transform(p0), transform(p1), color);
        }
    }
}

fn push_capsule_wireframe(
    verts: &mut Vec<DebugVertex>,
    transform: &dyn Fn(Vec3) -> Vec3,
    half_height: f32,
    radius: f32,
    color: [f32; 4],
) {
    let segments = 16;

    // Vertical lines
    for i in 0..4 {
        let angle = (i as f32 / 4.0) * std::f32::consts::TAU;
        let x = angle.cos() * radius;
        let z = angle.sin() * radius;
        push_line(verts, transform(Vec3::new(x, half_height, z)), transform(Vec3::new(x, -half_height, z)), color);
    }

    // Top and bottom circles
    for &y in &[half_height, -half_height] {
        for i in 0..segments {
            let a0 = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let a1 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;
            push_line(
                verts,
                transform(Vec3::new(a0.cos() * radius, y, a0.sin() * radius)),
                transform(Vec3::new(a1.cos() * radius, y, a1.sin() * radius)),
                color,
            );
        }
    }
}
