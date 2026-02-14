/// Immediate-mode 2D overlay renderer for text, rectangles, and screen effects.
/// Draws on top of the 3D scene using LoadOp::Load to preserve the existing framebuffer.

use crate::font::{self, BitmapFont};

// ── Vertex ──────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex2D {
    position: [f32; 2],
    tex_coords: [f32; 2],
    color: [f32; 4],
}

const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<Vertex2D>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x2,
        },
        wgpu::VertexAttribute {
            offset: 8,
            shader_location: 1,
            format: wgpu::VertexFormat::Float32x2,
        },
        wgpu::VertexAttribute {
            offset: 16,
            shader_location: 2,
            format: wgpu::VertexFormat::Float32x4,
        },
    ],
};

// ── WGSL shaders ────────────────────────────────────────────────────

const COLORED_WGSL: &str = r#"
struct Proj { m: mat4x4<f32> };
@group(0) @binding(0) var<uniform> proj: Proj;

struct VIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv:  vec2<f32>,
    @location(2) col: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) col: vec4<f32>,
};

@vertex fn vs(v: VIn) -> VOut {
    var o: VOut;
    o.clip = proj.m * vec4<f32>(v.pos, 0.0, 1.0);
    o.col  = v.col;
    return o;
}
@fragment fn fs(v: VOut) -> @location(0) vec4<f32> {
    return v.col;
}
"#;

const TEXTURED_WGSL: &str = r#"
struct Proj { m: mat4x4<f32> };
@group(0) @binding(0) var<uniform> proj: Proj;
@group(1) @binding(0) var font_tex: texture_2d<f32>;
@group(1) @binding(1) var font_smp: sampler;

struct VIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv:  vec2<f32>,
    @location(2) col: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv:  vec2<f32>,
    @location(1) col: vec4<f32>,
};

@vertex fn vs(v: VIn) -> VOut {
    var o: VOut;
    o.clip = proj.m * vec4<f32>(v.pos, 0.0, 1.0);
    o.uv   = v.uv;
    o.col  = v.col;
    return o;
}
@fragment fn fs(v: VOut) -> @location(0) vec4<f32> {
    let t = textureSample(font_tex, font_smp, v.uv);
    return vec4<f32>(v.col.rgb, v.col.a * t.a);
}
"#;

// ── Constants ───────────────────────────────────────────────────────

const MAX_QUADS: usize = 4096;
const MAX_VERTICES: usize = MAX_QUADS * 4;
const MAX_INDICES: usize = MAX_QUADS * 6;

// ── UiRenderer ──────────────────────────────────────────────────────

pub struct UiRenderer {
    proj_buffer: wgpu::Buffer,
    proj_bind_group: wgpu::BindGroup,
    colored_pipeline: wgpu::RenderPipeline,
    textured_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    // Per-frame draw data
    col_verts: Vec<Vertex2D>,
    col_idx: Vec<u16>,
    tex_verts: Vec<Vertex2D>,
    tex_idx: Vec<u16>,
    // Screen flash
    flash_color: [f32; 4],
    flash_remaining: f32,
    flash_duration: f32,
}

fn alpha_blend_state() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

impl UiRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        font: &BitmapFont,
    ) -> Self {
        // Projection uniform buffer (64 bytes = mat4x4<f32>)
        let proj_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI Projection"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let proj_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("UI Proj BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let proj_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("UI Proj BG"),
            layout: &proj_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: proj_buffer.as_entire_binding(),
            }],
        });

        // Colored pipeline (group 0 = projection only)
        let colored_pipeline = {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("UI Colored Shader"),
                source: wgpu::ShaderSource::Wgsl(COLORED_WGSL.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("UI Colored PL"),
                bind_group_layouts: &[&proj_bgl],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("UI Colored Pipeline"),
                layout: Some(&layout),
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
                        blend: Some(alpha_blend_state()),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        // Textured pipeline (group 0 = projection, group 1 = font atlas)
        let textured_pipeline = {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("UI Textured Shader"),
                source: wgpu::ShaderSource::Wgsl(TEXTURED_WGSL.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("UI Textured PL"),
                bind_group_layouts: &[&proj_bgl, &font.bind_group_layout],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("UI Textured Pipeline"),
                layout: Some(&layout),
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
                        blend: Some(alpha_blend_state()),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        // Pre-allocated GPU buffers
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI Vertices"),
            size: (MAX_VERTICES * std::mem::size_of::<Vertex2D>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI Indices"),
            size: (MAX_INDICES * std::mem::size_of::<u16>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        tracing::info!("UI renderer created (max {} quads)", MAX_QUADS);

        Self {
            proj_buffer,
            proj_bind_group,
            colored_pipeline,
            textured_pipeline,
            vertex_buffer,
            index_buffer,
            col_verts: Vec::with_capacity(256),
            col_idx: Vec::with_capacity(384),
            tex_verts: Vec::with_capacity(1024),
            tex_idx: Vec::with_capacity(1536),
            flash_color: [0.0; 4],
            flash_remaining: 0.0,
            flash_duration: 0.0,
        }
    }

    // ── Draw commands ───────────────────────────────────────────────

    /// Queue a solid-color rectangle (screen-space pixels, origin top-left).
    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        push_quad(
            &mut self.col_verts,
            &mut self.col_idx,
            x, y, w, h,
            0.0, 0.0, 0.0, 0.0,
            color,
        );
    }

    /// Queue a text string (screen-space pixels, origin top-left).
    /// `size` is the pixel height of each character (width auto-scales to maintain 6:8 ratio).
    pub fn draw_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        size: f32,
        color: [f32; 4],
        font: &BitmapFont,
    ) {
        let scale = size / font.glyph_h;
        let char_w = font.glyph_w * scale;
        let char_h = size;
        let mut cx = x;
        for ch in text.chars() {
            if ch == '\n' {
                // Not handled in this simple renderer
                continue;
            }
            let [u0, v0, u1, v1] = font::glyph_uvs(font, ch);
            push_quad(
                &mut self.tex_verts,
                &mut self.tex_idx,
                cx, y, char_w, char_h,
                u0, v0, u1, v1,
                color,
            );
            cx += char_w;
        }
    }

    /// Start a screen flash effect. Color includes alpha. Duration in seconds.
    pub fn set_flash(&mut self, color: [f32; 4], duration: f32) {
        self.flash_color = color;
        self.flash_duration = duration;
        self.flash_remaining = duration;
    }

    // ── Render ──────────────────────────────────────────────────────

    /// Render all queued UI on top of the existing framebuffer.
    /// Call once per frame after 3D rendering. Clears draw commands after rendering.
    pub fn render(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        font: &BitmapFont,
        width: u32,
        height: u32,
        dt: f32,
    ) {
        // Tick screen flash
        if self.flash_remaining > 0.0 {
            self.flash_remaining = (self.flash_remaining - dt).max(0.0);
            let alpha = self.flash_color[3] * (self.flash_remaining / self.flash_duration.max(0.001));
            self.draw_rect(
                0.0, 0.0,
                width as f32, height as f32,
                [self.flash_color[0], self.flash_color[1], self.flash_color[2], alpha],
            );
        }

        let col_vert_count = self.col_verts.len();
        let col_idx_count = self.col_idx.len();
        let tex_idx_count = self.tex_idx.len();
        let total_idx = col_idx_count + tex_idx_count;

        if total_idx == 0 {
            self.clear();
            return;
        }

        // Upload orthographic projection: (0,0) top-left, (w,h) bottom-right
        let w = width as f32;
        let h = height as f32;
        #[rustfmt::skip]
        let proj: [f32; 16] = [
            2.0 / w,  0.0,      0.0, 0.0,
            0.0,     -2.0 / h,  0.0, 0.0,
            0.0,      0.0,      1.0, 0.0,
           -1.0,      1.0,      0.0, 1.0,
        ];
        queue.write_buffer(&self.proj_buffer, 0, bytemuck::cast_slice(&proj));

        // Merge vertices: colored first, then textured (with offset indices)
        let offset = col_vert_count as u16;
        let mut all_verts = Vec::with_capacity(col_vert_count + self.tex_verts.len());
        all_verts.extend_from_slice(&self.col_verts);
        all_verts.extend_from_slice(&self.tex_verts);

        let mut all_idx = Vec::with_capacity(total_idx);
        all_idx.extend_from_slice(&self.col_idx);
        for &i in &self.tex_idx {
            all_idx.push(i + offset);
        }

        // Clamp to buffer capacity
        let max_v = all_verts.len().min(MAX_VERTICES);
        let max_i = all_idx.len().min(MAX_INDICES);
        let all_verts = &all_verts[..max_v];
        let all_idx = &all_idx[..max_i];

        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(all_verts));
        queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(all_idx));

        // Render pass — LoadOp::Load preserves the 3D scene underneath
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            pass.set_bind_group(0, &self.proj_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

            // Draw colored rectangles
            let col_idx_count = col_idx_count.min(max_i) as u32;
            if col_idx_count > 0 {
                pass.set_pipeline(&self.colored_pipeline);
                pass.draw_indexed(0..col_idx_count, 0, 0..1);
            }

            // Draw textured text
            let tex_end = max_i as u32;
            if tex_end > col_idx_count {
                pass.set_pipeline(&self.textured_pipeline);
                pass.set_bind_group(1, &font.bind_group, &[]);
                pass.draw_indexed(col_idx_count..tex_end, 0, 0..1);
            }
        }

        self.clear();
    }

    /// Clear per-frame draw state. Called automatically by `render()`.
    pub fn clear(&mut self) {
        self.col_verts.clear();
        self.col_idx.clear();
        self.tex_verts.clear();
        self.tex_idx.clear();
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn push_quad(
    verts: &mut Vec<Vertex2D>,
    idx: &mut Vec<u16>,
    x: f32, y: f32, w: f32, h: f32,
    u0: f32, v0: f32, u1: f32, v1: f32,
    color: [f32; 4],
) {
    let base = verts.len() as u16;
    verts.push(Vertex2D { position: [x, y],         tex_coords: [u0, v0], color });
    verts.push(Vertex2D { position: [x + w, y],     tex_coords: [u1, v0], color });
    verts.push(Vertex2D { position: [x + w, y + h], tex_coords: [u1, v1], color });
    verts.push(Vertex2D { position: [x, y + h],     tex_coords: [u0, v1], color });
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}
