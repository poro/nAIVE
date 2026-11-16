//! Sky and image-based lighting.
//!
//! A scene's `settings.sky` becomes an environment: an equirectangular image
//! (a gradient drawn here, or a picture from the project) with a mip chain.
//! The deferred lighting pass draws it behind the scene, lights surfaces with
//! its diffuse irradiance (nine spherical harmonics coefficients computed
//! here), and reflects it, sampling a blurrier mip the rougher the surface.
//!
//! Directions map to the image as `u = atan2(z, x) / 2pi + 0.5` and
//! `v = acos(y) / pi`, so the top row is straight up. The shaders use the
//! same mapping and the same harmonics basis.

use std::f32::consts::PI;
use std::path::Path;

use glam::Vec3;

use crate::scene::SkyDef;

/// Width and height of a generated gradient sky.
const GRADIENT_SIZE: (u32, u32) = (256, 128);

/// The lighting pass's view of the environment.
pub struct Environment {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    /// Diffuse light from the sky for a surface facing a direction: nine
    /// harmonics coefficients (rgb), already convolved and divided by pi, so
    /// the shader multiplies the evaluated sum by the surface colour.
    pub irradiance: [[f32; 4]; 9],
    /// Highest mip level, used for the roughest reflections.
    pub max_lod: f32,
    pub intensity: f32,
    /// The settings it was built from; `None` is the black default.
    pub source: Option<SkyDef>,
}

impl Environment {
    /// No sky: a black 1x1 texture (new textures start zeroed), for scenes
    /// without one.
    pub fn none(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("No Sky"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        Self {
            texture,
            view,
            sampler,
            irradiance: [[0.0; 4]; 9],
            max_lod: 0.0,
            intensity: 0.0,
            source: None,
        }
    }

    /// Whether the lighting pass should draw and use the sky.
    pub fn enabled(&self) -> bool {
        self.source.is_some()
    }
}

/// Build the environment for `sky`, loading its image from the project if it
/// names one. A missing or unreadable image falls back to the gradient.
pub fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    sky: &SkyDef,
    project_root: &Path,
) -> Environment {
    let base = sky
        .texture
        .as_deref()
        .and_then(|path| match load_image(project_root, path) {
            Ok(image) => Some(image),
            Err(e) => {
                tracing::warn!("Sky texture {}: {}; using the gradient", path, e);
                None
            }
        })
        .unwrap_or_else(|| gradient(sky));
    let mips = mip_chain(base);
    // Harmonics from a small mip: plenty for light this smooth, and fast.
    let small = mips
        .iter()
        .find(|m| m.width <= 64)
        .unwrap_or(mips.last().expect("at least one mip"));
    let irradiance = irradiance_coefficients(small);
    upload(
        device,
        queue,
        &mips,
        irradiance,
        Some(sky.clone()),
        sky.intensity.max(0.0),
    )
}

/// An image of linear RGB values, row by row from the top.
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<Vec3>,
}

impl Image {
    fn at(&self, x: u32, y: u32) -> Vec3 {
        self.pixels[(y * self.width + x) as usize]
    }
}

/// The direction at the centre of texel (x, y) of a width x height image.
fn texel_direction(x: u32, y: u32, width: u32, height: u32) -> Vec3 {
    let phi = ((x as f32 + 0.5) / width as f32 - 0.5) * 2.0 * PI;
    let theta = (y as f32 + 0.5) / height as f32 * PI;
    Vec3::new(
        theta.sin() * phi.cos(),
        theta.cos(),
        theta.sin() * phi.sin(),
    )
}

/// The gradient sky: zenith to horizon above, horizon to ground below.
fn gradient(sky: &SkyDef) -> Image {
    let (width, height) = GRADIENT_SIZE;
    let zenith = Vec3::from_array(sky.zenith);
    let horizon = Vec3::from_array(sky.horizon);
    let ground = Vec3::from_array(sky.ground);
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let up = texel_direction(x, y, width, height).y;
            pixels.push(if up >= 0.0 {
                horizon.lerp(zenith, up.powf(0.6))
            } else {
                horizon.lerp(ground, (-up).powf(0.4))
            });
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

fn load_image(project_root: &Path, relative: &str) -> Result<Image, String> {
    let path = crate::project_path::resolve(project_root, relative)?;
    let image = image::open(&path).map_err(|e| e.to_string())?.to_rgb8();
    // Large panoramas are wasted on lighting this soft; 1024 wide is plenty.
    let image = if image.width() > 1024 {
        let height = (1024 * image.height() / image.width()).max(1);
        image::imageops::resize(&image, 1024, height, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let to_linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Ok(Image {
        width: image.width(),
        height: image.height(),
        pixels: image
            .pixels()
            .map(|p| Vec3::new(to_linear(p[0]), to_linear(p[1]), to_linear(p[2])))
            .collect(),
    })
}

/// The image and successively halved copies, down to 4 texels high.
fn mip_chain(base: Image) -> Vec<Image> {
    let mut mips = vec![base];
    loop {
        let last = mips.last().expect("nonempty");
        if last.height <= 4 || last.width <= 4 {
            break;
        }
        let (width, height) = (last.width / 2, last.height / 2);
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let sum = last.at(2 * x, 2 * y)
                    + last.at(2 * x + 1, 2 * y)
                    + last.at(2 * x, 2 * y + 1)
                    + last.at(2 * x + 1, 2 * y + 1);
                pixels.push(sum * 0.25);
            }
        }
        mips.push(Image {
            width,
            height,
            pixels,
        });
    }
    mips
}

/// The real spherical harmonics basis up to l = 2, in the order the shaders
/// use.
fn harmonics(d: Vec3) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * d.y,
        0.488_603 * d.z,
        0.488_603 * d.x,
        1.092_548 * d.x * d.y,
        1.092_548 * d.y * d.z,
        0.315_392 * (3.0 * d.z * d.z - 1.0),
        1.092_548 * d.x * d.z,
        0.546_274 * (d.x * d.x - d.y * d.y),
    ]
}

/// Project the image onto the harmonics and convolve with a cosine lobe, so
/// that evaluating them for a normal gives the diffuse light it receives
/// (divided by pi).
fn irradiance_coefficients(image: &Image) -> [[f32; 4]; 9] {
    let mut sum = [Vec3::ZERO; 9];
    let texel_area = (2.0 * PI / image.width as f32) * (PI / image.height as f32);
    for y in 0..image.height {
        for x in 0..image.width {
            let d = texel_direction(x, y, image.width, image.height);
            // Solid angle of the texel: rows shrink toward the poles.
            let weight = texel_area * ((y as f32 + 0.5) / image.height as f32 * PI).sin();
            let radiance = image.at(x, y);
            for (s, basis) in sum.iter_mut().zip(harmonics(d)) {
                *s += radiance * basis * weight;
            }
        }
    }
    // Cosine convolution per band (pi, 2pi/3, pi/4), then divided by pi.
    let band = [
        1.0,
        2.0 / 3.0,
        2.0 / 3.0,
        2.0 / 3.0,
        0.25,
        0.25,
        0.25,
        0.25,
        0.25,
    ];
    let mut out = [[0.0; 4]; 9];
    for i in 0..9 {
        out[i] = (sum[i] * band[i]).extend(0.0).to_array();
    }
    out
}

fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mips: &[Image],
    irradiance: [[f32; 4]; 9],
    source: Option<SkyDef>,
    intensity: f32,
) -> Environment {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Sky Environment"),
        size: wgpu::Extent3d {
            width: mips[0].width,
            height: mips[0].height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let to_srgb = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5) as u8
    };
    for (level, mip) in mips.iter().enumerate() {
        let bytes: Vec<u8> = mip
            .pixels
            .iter()
            .flat_map(|p| [to_srgb(p.x), to_srgb(p.y), to_srgb(p.z), 255])
            .collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * mip.width),
                rows_per_image: Some(mip.height),
            },
            wgpu::Extent3d {
                width: mip.width,
                height: mip.height,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Sky Sampler"),
        // Around the horizon the image wraps; at the poles it stops.
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    Environment {
        texture,
        view,
        sampler,
        irradiance,
        max_lod: (mips.len() - 1) as f32,
        intensity,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate(coefficients: &[[f32; 4]; 9], n: Vec3) -> Vec3 {
        coefficients
            .iter()
            .zip(harmonics(n))
            .map(|(c, b)| Vec3::new(c[0], c[1], c[2]) * b)
            .sum()
    }

    #[test]
    fn a_uniform_sky_lights_every_direction_equally() {
        let image = Image {
            width: 64,
            height: 32,
            pixels: vec![Vec3::splat(0.5); 64 * 32],
        };
        let coefficients = irradiance_coefficients(&image);
        // A constant radiance L gives irradiance pi * L, i.e. L after the
        // division by pi.
        for n in [Vec3::X, Vec3::Y, Vec3::NEG_Y, Vec3::new(0.6, -0.48, 0.64)] {
            let e = evaluate(&coefficients, n);
            assert!(
                (e - Vec3::splat(0.5)).abs().max_element() < 0.01,
                "{n}: {e}"
            );
        }
    }

    #[test]
    fn a_white_upper_half_lights_up_facing_surfaces_fully_and_sideways_half() {
        // Exact answers: facing up sees all of the white half (1), facing
        // sideways half of it (0.5), facing down none (0). Nine harmonics
        // only approximate a sharp horizon, hence the tolerance.
        let (width, height) = (128, 64);
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |_| y))
            .map(|y| {
                if y < height / 2 {
                    Vec3::ONE
                } else {
                    Vec3::ZERO
                }
            })
            .collect();
        let coefficients = irradiance_coefficients(&Image {
            width,
            height,
            pixels,
        });
        let at = |n: Vec3| evaluate(&coefficients, n).x;
        assert!((at(Vec3::Y) - 1.0).abs() < 0.1, "{}", at(Vec3::Y));
        assert!((at(Vec3::X) - 0.5).abs() < 0.02, "{}", at(Vec3::X));
        assert!(at(Vec3::NEG_Y).abs() < 0.1, "{}", at(Vec3::NEG_Y));

        // The gradient sky with a dark ground: brighter from above.
        let sky = SkyDef {
            zenith: [1.0, 1.0, 1.0],
            horizon: [1.0, 1.0, 1.0],
            ground: [0.0, 0.0, 0.0],
            intensity: 1.0,
            texture: None,
        };
        let mips = mip_chain(gradient(&sky));
        assert_eq!(mips.last().unwrap().height, 4);
        let coefficients = irradiance_coefficients(&mips[2]);
        let at = |n: Vec3| evaluate(&coefficients, n).x;
        assert!(at(Vec3::Y) > at(Vec3::X) && at(Vec3::X) > at(Vec3::NEG_Y));
    }

    #[test]
    fn sky_images_load_from_the_project_as_linear_colour() {
        let root = std::env::temp_dir().join(format!("naive-sky-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let mut picture = image::RgbImage::new(8, 4);
        for (x, _, pixel) in picture.enumerate_pixels_mut() {
            *pixel = if x < 4 {
                image::Rgb([255, 255, 255])
            } else {
                image::Rgb([188, 0, 0])
            };
        }
        picture.save(root.join("assets/sky.png")).unwrap();
        let loaded = load_image(&root, "assets/sky.png").unwrap();
        assert_eq!((loaded.width, loaded.height), (8, 4));
        assert_eq!(loaded.at(0, 0), Vec3::ONE);
        // sRGB 188 is about half brightness in linear terms.
        assert!(
            (loaded.at(7, 3).x - 0.5).abs() < 0.01,
            "{}",
            loaded.at(7, 3)
        );
        assert!(load_image(&root, "../outside.png").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn texel_directions_follow_the_shader_mapping() {
        // The top row looks up, the bottom row down, and the middle of the
        // image along +x (u = 0.5 is phi = 0).
        assert!(texel_direction(0, 0, 256, 128).y > 0.99);
        assert!(texel_direction(0, 127, 256, 128).y < -0.99);
        let middle = texel_direction(128, 64, 256, 128);
        assert!(middle.x > 0.99, "{middle}");
    }
}
