//! Saving a rendered frame to a PNG file, for the `capture_frame` command.
//!
//! The engine renders the scene a second time into an offscreen texture and
//! reads that back, so a capture never depends on what the window shows.

use std::path::{Path, PathBuf};

use crate::renderer::GpuState;

/// The file a capture requested as `relative` is written to. Captures stay
/// inside the project: absolute paths and `..` are refused, and so is
/// anything that is not a `.png`.
pub fn capture_path(project_root: &Path, relative: &str) -> Result<PathBuf, String> {
    crate::project_path::resolve_with_extension(project_root, relative, "png")
}

/// Create a texture the size and format of the window surface that the scene
/// can be rendered into and then read back.
pub fn create_capture_target(gpu: &GpuState) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Frame capture"),
        size: wgpu::Extent3d {
            width: gpu.config.width,
            height: gpu.config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gpu.config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Read `texture` back from the GPU and write it to `path` as an opaque PNG.
/// Returns the image's width and height.
pub fn save_texture_png(
    gpu: &GpuState,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<(u32, u32), String> {
    let format = texture.format();
    let bgra = match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        other => return Err(format!("cannot capture a {:?} surface", other)),
    };
    let (width, height) = (texture.width(), texture.height());

    // Rows in the copy buffer are padded to wgpu's alignment.
    let row_bytes = width * 4;
    let padded_row_bytes =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Frame capture readback"),
        size: u64::from(padded_row_bytes) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Frame capture copy"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));

    // Wait for the copy and map the result.
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    gpu.device.poll(wgpu::Maintain::Wait);
    receiver
        .recv()
        .map_err(|e| format!("the GPU never answered: {}", e))?
        .map_err(|e| format!("could not read the frame back: {}", e))?;

    // Drop the row padding and the alpha channel. The alpha the renderer
    // leaves behind is meaningless, and would make the PNG see-through.
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    {
        let data = slice.get_mapped_range();
        for row in data.chunks_exact(padded_row_bytes as usize) {
            for pixel in row[..row_bytes as usize].as_chunks::<4>().0 {
                if bgra {
                    rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                } else {
                    rgb.extend_from_slice(&pixel[..3]);
                }
            }
        }
    }
    buffer.unmap();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
    }
    let image = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| "the frame had an unexpected size".to_string())?;
    image
        .save(path)
        .map_err(|e| format!("could not write {}: {}", path.display(), e))?;
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_paths_stay_inside_the_project() {
        let root_buf = std::env::temp_dir();
        let root = root_buf.as_path();

        assert_eq!(
            capture_path(root, "captures/frame.png").unwrap(),
            root.canonicalize().unwrap().join("captures/frame.png")
        );
        assert_eq!(
            capture_path(root, "./shot.PNG").unwrap(),
            root.canonicalize().unwrap().join("shot.PNG")
        );

        for escaping in ["/tmp/frame.png", "../frame.png", "captures/../../frame.png"] {
            assert!(
                capture_path(root, escaping).is_err(),
                "{escaping} should be refused"
            );
        }
        for not_png in ["captures/frame.jpg", "captures/frame", "naive.yaml"] {
            assert!(
                capture_path(root, not_png).is_err(),
                "{not_png} should be refused"
            );
        }
    }
}
