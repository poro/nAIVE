//! Material parsing and texture decoding shared by native and headless scenes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::AssetError;

/// Material YAML deserialization type.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MaterialFile {
    #[serde(default)]
    pub shader: String,
    #[serde(default)]
    pub properties: MaterialProperties,
    #[serde(default = "default_opaque")]
    pub blend_mode: String,
    #[serde(default = "default_back")]
    pub cull_mode: String,
}

fn default_opaque() -> String {
    "opaque".to_string()
}
fn default_back() -> String {
    "back".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MaterialProperties {
    #[serde(default = "default_base_color")]
    pub base_color: [f32; 3],
    #[serde(default = "default_roughness")]
    pub roughness: f32,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default)]
    pub emission: [f32; 3],
    /// Project-relative albedo and normal texture paths.
    #[serde(default)]
    pub albedo_map: Option<String>,
    #[serde(default)]
    pub normal_map: Option<String>,
}

impl Default for MaterialProperties {
    fn default() -> Self {
        Self {
            base_color: default_base_color(),
            roughness: default_roughness(),
            metallic: 0.0,
            emission: [0.0; 3],
            albedo_map: None,
            normal_map: None,
        }
    }
}

fn default_base_color() -> [f32; 3] {
    [0.8, 0.8, 0.8]
}
fn default_roughness() -> f32 {
    0.5
}

impl Default for MaterialFile {
    fn default() -> Self {
        Self {
            shader: String::new(),
            properties: MaterialProperties::default(),
            blend_mode: default_opaque(),
            cull_mode: default_back(),
        }
    }
}

/// Load a material and validate every referenced texture without GPU resources.
/// Missing assets are errors; only `procedural:default` selects an engine default.
pub fn load_material(project_root: &Path, material_path: &str) -> Result<MaterialFile, AssetError> {
    if material_path == "procedural:default" {
        return Ok(MaterialFile::default());
    }
    let path = crate::project_path::resolve(project_root, material_path).map_err(AssetError)?;
    let contents = std::fs::read_to_string(&path).map_err(|error| {
        AssetError(format!(
            "Failed to read material '{material_path}': {error}"
        ))
    })?;
    let material: MaterialFile = serde_norway::from_str(&contents)
        .map_err(|error| AssetError(format!("Invalid material '{material_path}': {error}")))?;
    if material
        .properties
        .base_color
        .iter()
        .chain(material.properties.emission.iter())
        .chain([
            &material.properties.roughness,
            &material.properties.metallic,
        ])
        .any(|value| !value.is_finite())
    {
        return Err(AssetError(format!(
            "Material '{material_path}' has nonfinite properties"
        )));
    }
    for (kind, texture) in [
        ("albedo_map", material.properties.albedo_map.as_deref()),
        ("normal_map", material.properties.normal_map.as_deref()),
    ] {
        if let Some(texture) = texture {
            load_texture(project_root, texture).map_err(|error| {
                AssetError(format!("Material '{material_path}' {kind}: {error}"))
            })?;
        }
    }
    Ok(material)
}

/// Decode a confined project texture to RGBA8 for validation or GPU upload.
pub fn load_texture(
    project_root: &Path,
    texture_path: &str,
) -> Result<image::RgbaImage, AssetError> {
    let path = crate::project_path::resolve(project_root, texture_path).map_err(AssetError)?;
    image::open(&path)
        .map(image::DynamicImage::into_rgba8)
        .map_err(|error| AssetError(format!("Failed to load texture '{texture_path}': {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project(std::path::PathBuf);

    impl Project {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("naive-material-assets-{}-{id}", std::process::id(),));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, path: &str, contents: &str) {
            std::fs::write(self.0.join(path), contents).unwrap();
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn explicit_default_and_yaml_defaults_are_equivalent() {
        let project = Project::new();
        project.write("empty.yaml", "{}");
        project.write("partial.yaml", "properties: {metallic: 0.7}");
        let builtin = load_material(&project.0, "procedural:default").unwrap();
        let empty = load_material(&project.0, "empty.yaml").unwrap();
        assert_eq!(
            serde_json::to_value(&builtin).unwrap(),
            serde_json::to_value(&empty).unwrap()
        );
        let partial = load_material(&project.0, "partial.yaml").unwrap();
        assert_eq!(partial.properties.base_color, [0.8; 3]);
        assert_eq!(partial.properties.roughness, 0.5);
        assert_eq!(partial.properties.metallic, 0.7);
        assert_eq!(partial.blend_mode, "opaque");
        assert_eq!(partial.cull_mode, "back");
        assert!(load_material(&project.0, "procedural:unknown").is_err());
    }

    #[test]
    fn missing_and_malformed_materials_are_errors() {
        let project = Project::new();
        assert!(load_material(&project.0, "missing.yaml")
            .unwrap_err()
            .to_string()
            .contains("missing.yaml"));
        for source in [
            "properties: [",
            "properties: {base_color: [1, 2]}",
            "properties: {roughness: broken}",
        ] {
            project.write("bad.yaml", source);
            assert!(load_material(&project.0, "bad.yaml")
                .unwrap_err()
                .to_string()
                .contains("Invalid material"));
        }
    }

    #[test]
    fn material_properties_must_be_finite_without_clamping_hdr_values() {
        let project = Project::new();
        for properties in [
            "base_color: [.nan, 0, 0]",
            "emission: [0, .inf, 0]",
            "roughness: -.inf",
            "metallic: .nan",
        ] {
            project.write("properties.yaml", &format!("properties: {{{properties}}}"));
            assert!(load_material(&project.0, "properties.yaml")
                .unwrap_err()
                .to_string()
                .contains("nonfinite"));
        }
        project.write("properties.yaml", "properties: {emission: [100, 4, 2]}");
        assert_eq!(
            load_material(&project.0, "properties.yaml")
                .unwrap()
                .properties
                .emission,
            [100.0, 4.0, 2.0]
        );
    }

    #[test]
    fn materials_validate_both_maps_and_texture_pixels_decode() {
        let project = Project::new();
        assert!(load_texture(&project.0, "missing.png").is_err());
        project.write("invalid.png", "invalid image bytes");
        assert!(load_texture(&project.0, "invalid.png").is_err());
        let pixels = image::RgbaImage::from_pixel(2, 3, image::Rgba([12, 34, 56, 255]));
        pixels.save(project.0.join("valid.png")).unwrap();
        assert_eq!(load_texture(&project.0, "valid.png").unwrap(), pixels);
        for kind in ["albedo_map", "normal_map"] {
            for texture in ["missing.png", "invalid.png"] {
                project.write("maps.yaml", &format!("properties: {{{kind}: {texture}}}"));
                let error = load_material(&project.0, "maps.yaml")
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(kind) && error.contains(texture), "{error}");
            }
        }
        project.write(
            "maps.yaml",
            "properties: {albedo_map: valid.png, normal_map: valid.png}",
        );
        assert!(load_material(&project.0, "maps.yaml").is_ok());
    }

    #[test]
    fn materials_and_textures_cannot_escape_the_project() {
        let project = Project::new();
        for path in ["../outside.yaml", "/tmp/outside.yaml"] {
            assert!(load_material(&project.0, path).is_err());
        }
        for path in ["../outside.png", "/tmp/outside.png"] {
            assert!(load_texture(&project.0, path).is_err());
            project.write(
                "escape.yaml",
                &format!("properties: {{normal_map: '{path}'}}"),
            );
            assert!(load_material(&project.0, "escape.yaml").is_err());
        }
        #[cfg(unix)]
        {
            let outside = Project::new();
            outside.write("material.yaml", "{}");
            image::RgbaImage::new(1, 1)
                .save(outside.0.join("texture.png"))
                .unwrap();
            std::os::unix::fs::symlink(&outside.0, project.0.join("escape")).unwrap();
            assert!(load_material(&project.0, "escape/material.yaml").is_err());
            assert!(load_texture(&project.0, "escape/texture.png").is_err());
            project.write(
                "escape.yaml",
                "properties: {albedo_map: escape/texture.png}",
            );
            assert!(load_material(&project.0, "escape.yaml").is_err());
        }
    }
}
