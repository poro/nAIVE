use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Handle to a cached texture bind group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle(pub usize);

/// Cache of loaded textures from disk (PNG, JPG, WEBP).
/// Each entry is a wgpu bind group (texture view + sampler) matching the
/// texture bind group layout (group 2).
pub struct TextureCache {
    bind_groups: Vec<wgpu::BindGroup>,
    path_to_handle: HashMap<PathBuf, TextureHandle>,
}

impl TextureCache {
    pub fn new() -> Self {
        Self {
            bind_groups: Vec::new(),
            path_to_handle: HashMap::new(),
        }
    }

    /// Load a texture from disk, or return a cached handle.
    pub fn get_or_load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        project_root: &Path,
        texture_path: &str,
    ) -> Result<TextureHandle, String> {
        let key = PathBuf::from(texture_path);
        if let Some(&handle) = self.path_to_handle.get(&key) {
            return Ok(handle);
        }

        let img = naive_sim::assets::load_texture(project_root, texture_path)
            .map_err(|error| error.to_string())?;

        let (width, height) = img.dimensions();
        let pixels = img.into_raw();

        let bind_group = create_texture_bind_group_from_rgba(
            device,
            queue,
            layout,
            &pixels,
            width,
            height,
            &format!("Texture: {}", texture_path),
        );

        let handle = TextureHandle(self.bind_groups.len());
        self.bind_groups.push(bind_group);
        self.path_to_handle.insert(key, handle);
        tracing::info!("Loaded texture: {} ({}x{})", texture_path, width, height);
        Ok(handle)
    }

    /// Get the bind group for a texture handle.
    pub fn get(&self, handle: TextureHandle) -> &wgpu::BindGroup {
        &self.bind_groups[handle.0]
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a texture bind group from raw RGBA8 pixel data.
pub fn create_texture_bind_group_from_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    pixels: &[u8],
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::BindGroup {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        ..Default::default()
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&format!("{} BG", label)),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}
