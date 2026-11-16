//! nAIVE web playground — v0 proof-of-life.
//!
//! Goal: prove that wgpu + wasm-bindgen + WebGPU work end-to-end in the
//! browser. This crate intentionally does NOT depend on `naive-client` yet —
//! the next milestone pulls naive-client modules in behind a `web` feature
//! flag that gates out non-wasm deps (shader-slang, notify, ureq, full tokio).
//!
//! For v0 we render a clear color on a canvas the JS host passes in. If this
//! ships, the toolchain is green and we have a working build pipeline.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    log::info!("nAIVE web v{} booted", env!("CARGO_PKG_VERSION"));
}

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Initialize a WebGPU context on the given canvas element id and run a
/// single clear-color frame. Returns a JS Promise (because wgpu init is async).
#[wasm_bindgen]
pub async fn run_clear_demo(canvas_id: String) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window.document().ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas = document
        .get_element_by_id(&canvas_id)
        .ok_or_else(|| JsValue::from_str("canvas not found"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("element is not a canvas"))?;

    let width = canvas.width().max(1);
    let height = canvas.height().max(1);

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });

    let surface_target = wgpu::SurfaceTarget::Canvas(canvas);
    let surface = instance
        .create_surface(surface_target)
        .map_err(|e| JsValue::from_str(&format!("create_surface: {e}")))?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| JsValue::from_str("request_adapter: no suitable WebGPU adapter"))?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("naive-web device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("request_device: {e}")))?;

    let caps = surface.get_capabilities(&adapter);
    let format = caps.formats.first().copied().unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    let frame = surface
        .get_current_texture()
        .map_err(|e| JsValue::from_str(&format!("get_current_texture: {e}")))?;
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("naive-web clear encoder"),
    });
    {
        let _rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.043,
                        g: 0.051,
                        b: 0.071,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    queue.submit(std::iter::once(encoder.finish()));
    frame.present();

    log::info!("nAIVE web clear demo OK ({}x{})", width, height);
    Ok(())
}
