//! Immediate-mode 2D overlay renderer for text, rectangles, and screen effects.
//! Draws on top of the 3D scene using LoadOp::Load to preserve the existing framebuffer.

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

// Each queue holds at most `MAX_QUADS` quads, so a merged frame's largest
// index (a textured vertex offset past every colored one) is below
// `2 * MAX_VERTICES` and fits the u16 index format.
const _: () = assert!(2 * MAX_VERTICES <= u16::MAX as usize + 1);

// ── UiRenderer ──────────────────────────────────────────────────────

pub struct UiRenderer {
    proj_buffer: wgpu::Buffer,
    proj_bind_group: wgpu::BindGroup,
    colored_pipeline: wgpu::RenderPipeline,
    textured_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    /// Draw commands queued for the next `render`.
    queue: UiQueue,
    /// Whether the "overlay is full" warning has been logged.
    warned_full: bool,
    /// What the last `render` drew, so a frame capture can include the HUD.
    last_frame: DrawnUi,
}

/// UI geometry queued for one frame: colored quads and textured (text)
/// quads in separate lists, each capped at `MAX_QUADS`. Kept apart from the
/// GPU objects so it can be tested without a device.
#[derive(Default)]
struct UiQueue {
    col_verts: Vec<Vertex2D>,
    col_idx: Vec<u16>,
    tex_verts: Vec<Vertex2D>,
    tex_idx: Vec<u16>,
    /// Quads refused since the last `clear` because their list was full.
    dropped: usize,
}

impl UiQueue {
    fn push_colored(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        let (verts, idx) = (&mut self.col_verts, &mut self.col_idx);
        if !push_quad(verts, idx, [x, y, w, h], [0.0; 4], color) {
            self.dropped += 1;
        }
    }

    fn push_textured(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], color: [f32; 4]) {
        let (verts, idx) = (&mut self.tex_verts, &mut self.tex_idx);
        if !push_quad(verts, idx, [x, y, w, h], uv, color) {
            self.dropped += 1;
        }
    }

    /// Quads queued since the last `clear`, including dropped ones.
    fn queued_quads(&self) -> usize {
        (self.col_verts.len() + self.tex_verts.len()) / 4 + self.dropped
    }

    /// One frame's draw data: colored quads first, then textured ones (their
    /// indices offset past the colored vertices), at most `MAX_QUADS` in all.
    fn merge(&self, width: u32, height: u32) -> DrawnUi {
        let col_vert_count = self.col_verts.len();
        let col_idx_count = self.col_idx.len();
        // Below MAX_VERTICES: push_quad caps each list.
        let offset = col_vert_count as u16;
        let mut verts = Vec::with_capacity(col_vert_count + self.tex_verts.len());
        verts.extend_from_slice(&self.col_verts);
        verts.extend_from_slice(&self.tex_verts);
        let mut idx = Vec::with_capacity(col_idx_count + self.tex_idx.len());
        idx.extend_from_slice(&self.col_idx);
        idx.extend(self.tex_idx.iter().map(|&i| i + offset));
        // Clamp to buffer capacity. Every quad is 4 vertices and 6 indices
        // in the same order in both lists, so both cut at the same quad.
        verts.truncate(MAX_VERTICES);
        idx.truncate(MAX_INDICES);
        DrawnUi {
            verts,
            idx,
            col_idx_count: col_idx_count.min(MAX_INDICES),
            width,
            height,
        }
    }

    fn clear(&mut self) {
        self.col_verts.clear();
        self.col_idx.clear();
        self.tex_verts.clear();
        self.tex_idx.clear();
        self.dropped = 0;
    }
}

/// The geometry of one drawn UI frame: colored quads first, then text.
#[derive(Default)]
struct DrawnUi {
    verts: Vec<Vertex2D>,
    idx: Vec<u16>,
    col_idx_count: usize,
    width: u32,
    height: u32,
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
    /// Consume CPU script intentions. Flash time advances in simulation, even
    /// when no frame is rendered; GPU rendering never advances that state.
    pub fn consume_frame(
        &mut self,
        frame: &mut naive_sim::presentation::UiFrame,
        viewport: naive_sim::presentation::Viewport,
        font: &BitmapFont,
    ) {
        let flash = frame.flash(viewport);
        for intent in frame.intents.drain(..).chain(flash) {
            match intent {
                naive_sim::presentation::UiIntent::Text {
                    x,
                    y,
                    text,
                    size,
                    color,
                } => self.draw_text(x, y, &text, size, color, font),
                naive_sim::presentation::UiIntent::Rect {
                    x,
                    y,
                    width,
                    height,
                    color,
                } => self.draw_rect(x, y, width, height, color),
            }
        }
    }

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
                bind_group_layouts: &[Some(&proj_bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("UI Colored Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[Some(VERTEX_LAYOUT)],
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
                multiview_mask: None,
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
                bind_group_layouts: &[Some(&proj_bgl), Some(&font.bind_group_layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("UI Textured Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[Some(VERTEX_LAYOUT)],
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
                multiview_mask: None,
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
            queue: UiQueue {
                col_verts: Vec::with_capacity(256),
                col_idx: Vec::with_capacity(384),
                tex_verts: Vec::with_capacity(1024),
                tex_idx: Vec::with_capacity(1536),
                dropped: 0,
            },
            warned_full: false,
            last_frame: DrawnUi::default(),
        }
    }

    // ── Draw commands ───────────────────────────────────────────────

    /// Queue a solid-color rectangle (screen-space pixels, origin top-left).
    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.queue.push_colored(x, y, w, h, color);
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
            let uv = font::glyph_uvs(font, ch);
            self.queue.push_textured(cx, y, char_w, char_h, uv, color);
            cx += char_w;
        }
    }

    // ── Render ──────────────────────────────────────────────────────

    /// Render all queued UI on top of the existing framebuffer.
    /// Call once per frame after 3D rendering. Clears draw commands after rendering.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        font: &BitmapFont,
        width: u32,
        height: u32,
        _dt: f32,
    ) {
        self.last_frame = self.queue.merge(width, height);
        let queued = self.queue.queued_quads();
        let drawn = self.last_frame.idx.len() / 6;
        if drawn < queued && !self.warned_full {
            tracing::warn!(
                "UI overlay full: drew {drawn} of {queued} quads queued this frame \
                 (max {MAX_QUADS}); further overflows are not logged"
            );
            self.warned_full = true;
        }
        self.draw(&self.last_frame, queue, encoder, view, font);
        self.clear();
    }

    /// Draw the UI that the last `render` drew again, on top of `view`, for
    /// a frame capture that includes the HUD. Draw commands queued since are
    /// not included.
    pub fn render_last(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        font: &BitmapFont,
    ) {
        self.draw(&self.last_frame, queue, encoder, view, font);
    }

    fn draw(
        &self,
        frame: &DrawnUi,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        font: &BitmapFont,
    ) {
        if frame.idx.is_empty() {
            return;
        }

        // Orthographic projection: (0,0) top-left, (w,h) bottom-right
        let w = frame.width.max(1) as f32;
        let h = frame.height.max(1) as f32;
        #[rustfmt::skip]
        let proj: [f32; 16] = [
            2.0 / w,  0.0,      0.0, 0.0,
            0.0,     -2.0 / h,  0.0, 0.0,
            0.0,      0.0,      1.0, 0.0,
           -1.0,      1.0,      0.0, 1.0,
        ];
        queue.write_buffer(&self.proj_buffer, 0, bytemuck::cast_slice(&proj));
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&frame.verts));
        queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(&frame.idx));

        // Render pass — LoadOp::Load preserves the 3D scene underneath
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("UI Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                depth_slice: None,
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
        let col_idx_count = frame.col_idx_count as u32;
        if col_idx_count > 0 {
            pass.set_pipeline(&self.colored_pipeline);
            pass.draw_indexed(0..col_idx_count, 0, 0..1);
        }

        // Draw textured text
        let tex_end = frame.idx.len() as u32;
        if tex_end > col_idx_count {
            pass.set_pipeline(&self.textured_pipeline);
            pass.set_bind_group(1, &font.bind_group, &[]);
            pass.draw_indexed(col_idx_count..tex_end, 0, 0..1);
        }
    }

    /// Drop every queued draw command. `render` calls it after drawing, and
    /// the engine calls it at the start of each frame so the geometry of a
    /// frame that was never drawn does not carry over into the next one.
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Append one quad, or return false when the list already holds `MAX_QUADS`
/// quads, which keeps its u16 indices below `MAX_VERTICES`. `quad` is
/// `[x, y, width, height]` in screen pixels, `uv` is `[u0, v0, u1, v1]`.
fn push_quad(
    verts: &mut Vec<Vertex2D>,
    idx: &mut Vec<u16>,
    [x, y, w, h]: [f32; 4],
    [u0, v0, u1, v1]: [f32; 4],
    color: [f32; 4],
) -> bool {
    if verts.len() >= MAX_VERTICES {
        return false;
    }
    let base = verts.len() as u16;
    verts.push(Vertex2D {
        position: [x, y],
        tex_coords: [u0, v0],
        color,
    });
    verts.push(Vertex2D {
        position: [x + w, y],
        tex_coords: [u1, v0],
        color,
    });
    verts.push(Vertex2D {
        position: [x + w, y + h],
        tex_coords: [u1, v1],
        color,
    });
    verts.push(Vertex2D {
        position: [x, y + h],
        tex_coords: [u0, v1],
        color,
    });
    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0; 4];
    const GLYPH: [f32; 4] = [0.0, 0.0, 0.1, 0.1];

    /// Every index of a merged frame addresses one of its own vertices.
    fn assert_well_formed(frame: &DrawnUi) {
        assert!(frame.verts.len() <= MAX_VERTICES);
        assert!(frame.idx.len() <= MAX_INDICES);
        assert!(frame.col_idx_count <= frame.idx.len());
        assert_eq!(frame.verts.len() / 4, frame.idx.len() / 6);
        let vertex_count = frame.verts.len();
        assert!(frame.idx.iter().all(|&i| usize::from(i) < vertex_count));
    }

    #[test]
    fn a_frame_keeps_colored_quads_first_and_offsets_text_indices() {
        let mut queue = UiQueue::default();
        queue.push_textured(0.0, 0.0, 8.0, 8.0, GLYPH, WHITE);
        queue.push_colored(0.0, 0.0, 10.0, 10.0, WHITE);
        queue.push_colored(20.0, 0.0, 10.0, 10.0, WHITE);

        let frame = queue.merge(640, 480);
        assert_well_formed(&frame);
        assert_eq!((frame.verts.len(), frame.idx.len()), (12, 18));
        assert_eq!(frame.col_idx_count, 12);
        assert_eq!(frame.idx[12..], [8, 9, 10, 8, 10, 11]);
        assert_eq!(queue.queued_quads(), 3);
        assert_eq!(queue.dropped, 0);
    }

    /// Regression: while the window was occluded, `render_frame` returned
    /// before drawing (and clearing) the UI, but each frame still queued the
    /// script HUD and editor overlay. The pile-up overflowed the u16 indices
    /// when merged: "attempt to add with overflow" in debug builds, garbage
    /// indices in release.
    #[test]
    fn geometry_from_many_undrawn_frames_cannot_overflow_u16_indices() {
        let mut queue = UiQueue::default();
        for _frame in 0..1_000 {
            for _ in 0..5 {
                queue.push_colored(0.0, 0.0, 450.0, 30.0, WHITE);
            }
            for _ in 0..60 {
                queue.push_textured(0.0, 0.0, 12.0, 16.0, GLYPH, WHITE);
            }
        }

        let frame = queue.merge(1280, 720);
        assert_well_formed(&frame);
        assert_eq!(frame.idx.len(), MAX_INDICES);
        assert_eq!(queue.col_verts.len(), MAX_VERTICES);
        assert_eq!(queue.tex_verts.len(), MAX_VERTICES);
        assert_eq!(queue.queued_quads(), 65_000);

        queue.clear();
        assert_eq!(queue.queued_quads(), 0);
        queue.push_colored(0.0, 0.0, 1.0, 1.0, WHITE);
        let frame = queue.merge(1280, 720);
        assert_well_formed(&frame);
        assert_eq!(frame.idx, [0, 1, 2, 0, 2, 3]);
    }
}
