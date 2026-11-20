//! Explicit engine shaders and required project shader entry points.

use std::path::{Path, PathBuf};

use super::{PassDef, PassType, PipelineError, PipelineFile};

/// The pipeline schema this engine writes. In version 2 every pass names a
/// project shader that must exist, or an engine shader explicitly
/// (`builtin:<name>`). Version 1 is what `naive init` and the demo wrote up to
/// 0.1.22; [`migrate_pipeline`] gives those files their 0.1.22 meaning.
pub const CURRENT_PIPELINE_VERSION: u32 = 2;

/// Map a version 1 pipeline onto explicit shader references, returning one
/// note per pass whose reference changed. Version 2 pipelines are unchanged.
///
/// Version 1 meant:
/// - rasterize and shadow passes always used the engine's G-buffer and shadow
///   shaders, whatever file they named;
/// - a fullscreen or splat pass used its project shader when the file
///   existed (still required to compile, now strictly), and otherwise the
///   engine shader its pass name matched (`splat`, `light`, `bloom`,
///   `tonemap`, `fxaa`);
/// - with splat resources (`splat_color` and `splat_depth`), the lighting
///   pass always used the engine's splat-compositing lighting shader.
///
/// References that escape the project are left for the strict load to
/// reject, as 0.1.22 rejected them.
pub fn migrate_pipeline(
    pipeline: &mut PipelineFile,
    project_root: &Path,
) -> Result<Vec<String>, PipelineError> {
    match pipeline.version {
        CURRENT_PIPELINE_VERSION => return Ok(Vec::new()),
        1 => {}
        other => {
            return Err(PipelineError::InvalidFormat(format!(
                "Unsupported pipeline version {other}; this engine reads versions 1 and \
                 {CURRENT_PIPELINE_VERSION}"
            )))
        }
    }
    let has_splats = pipeline.resources.iter().any(|r| r.name == "splat_color")
        && pipeline.resources.iter().any(|r| r.name == "splat_depth");
    let mut notes = Vec::new();
    for pass in &mut pipeline.passes {
        if pass.shader.starts_with("builtin:") {
            continue;
        }
        let Ok(path) = crate::project_path::resolve(project_root, &pass.shader) else {
            continue;
        };
        let pass_type = PassType::from_name(&pass.pass_type);
        let lighting = pass_type == Some(PassType::Fullscreen)
            && fullscreen_role(&pass.shader, &pass.name) == FullscreenRole::Lighting;
        let builtin = match pass_type {
            Some(PassType::Rasterize) => Some("gbuffer"),
            Some(PassType::Shadow) => Some("shadow"),
            Some(PassType::Fullscreen) if lighting && has_splats => Some("deferred_light"),
            Some(PassType::Fullscreen | PassType::Splat) if !shader_exists(&path) => {
                legacy_builtin_for_pass_name(&pass.name)
            }
            _ => None,
        };
        if let Some(name) = builtin {
            let reference = format!("builtin:{name}");
            notes.push(format!(
                "pass '{}': '{}' -> '{reference}'",
                pass.name, pass.shader
            ));
            pass.shader = reference;
        }
    }
    Ok(notes)
}

/// The 0.1.22 fallback for a fullscreen or splat pass without its shader.
fn legacy_builtin_for_pass_name(name: &str) -> Option<&'static str> {
    if name.contains("splat") && !name.contains("light") {
        Some("gaussian_splat")
    } else if name.contains("light") {
        Some("deferred_light")
    } else if name.contains("bloom") {
        Some("bloom")
    } else if name.contains("tonemap") {
        Some("tonemap")
    } else if name.contains("fxaa") {
        Some("fxaa")
    } else {
        None
    }
}

/// A shader source, or the WGSL `naive build` compiled from it (a bundle
/// may ship only that).
fn shader_exists(path: &Path) -> bool {
    path.exists() || crate::shader::precompiled_wgsl_path(path).exists()
}

/// Load a project pipeline and give a version 1 file its 0.1.22 meaning,
/// logging each pass it maps.
pub fn load_project_pipeline(
    path: &Path,
    project_root: &Path,
) -> Result<PipelineFile, PipelineError> {
    let mut pipeline = super::load_pipeline(path)?;
    let notes = migrate_pipeline(&mut pipeline, project_root)?;
    if !notes.is_empty() {
        tracing::warn!(
            "{} is a version 1 pipeline; using its 0.1.22 shaders ({}). Write these \
             references and `version: {CURRENT_PIPELINE_VERSION}` to make them explicit.",
            path.display(),
            notes.join("; ")
        );
    }
    Ok(pipeline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FullscreenRole {
    Lighting,
    Bloom,
    Tonemap,
    Fxaa,
}

/// Engine shaders choose their own binding layout. Custom shaders retain the
/// existing pass-name convention for choosing one of the supported layouts.
pub(crate) fn fullscreen_role(reference: &str, pass_name: &str) -> FullscreenRole {
    match reference {
        "builtin:deferred_light" => FullscreenRole::Lighting,
        "builtin:bloom" => FullscreenRole::Bloom,
        "builtin:tonemap" => FullscreenRole::Tonemap,
        "builtin:fxaa" => FullscreenRole::Fxaa,
        _ if pass_name.contains("tonemap") => FullscreenRole::Tonemap,
        _ if pass_name.contains("fxaa") => FullscreenRole::Fxaa,
        _ if pass_name.contains("bloom") => FullscreenRole::Bloom,
        _ => FullscreenRole::Lighting,
    }
}

/// Built-ins are opt-in. A project file must never silently become an engine
/// shader just because compilation failed or its pass has a familiar name.
pub(crate) fn builtin_shader(
    reference: &str,
    pass_type: PassType,
    has_splats: bool,
) -> Result<Option<String>, PipelineError> {
    let Some(name) = reference.strip_prefix("builtin:") else {
        return Ok(None);
    };
    let (expected, source) = match name {
        "gbuffer" => (PassType::Rasterize, crate::shader::get_gbuffer_wgsl()),
        "shadow" => (PassType::Shadow, crate::shader::get_shadow_depth_wgsl()),
        "gaussian_splat" => (PassType::Splat, crate::shader::get_splat_render_wgsl()),
        "deferred_light" => (
            PassType::Fullscreen,
            if has_splats {
                crate::shader::get_deferred_light_with_splats_wgsl()
            } else {
                crate::shader::get_deferred_light_wgsl()
            },
        ),
        "bloom" => (PassType::Fullscreen, crate::shader::get_bloom_wgsl()),
        "tonemap" => (PassType::Fullscreen, crate::shader::get_tonemap_wgsl()),
        "fxaa" => (PassType::Fullscreen, crate::shader::get_fxaa_wgsl()),
        _ => {
            return Err(PipelineError::ShaderError(format!(
                "Unknown built-in shader '{reference}'"
            )))
        }
    };
    if pass_type != expected {
        return Err(PipelineError::ShaderError(format!(
            "Shader '{reference}' requires a {expected:?} pass"
        )));
    }
    Ok(Some(source))
}

pub(crate) fn load_pass_shader(
    project_root: &Path,
    pass: &PassDef,
    has_splats: bool,
) -> Result<(PathBuf, String), PipelineError> {
    let pass_type = PassType::from_name(&pass.pass_type).ok_or_else(|| {
        PipelineError::InvalidFormat(format!("Unknown pass type: '{}'", pass.pass_type))
    })?;
    if let Some(source) = builtin_shader(&pass.shader, pass_type, has_splats)? {
        return Ok((PathBuf::from(&pass.shader), source));
    }
    let path = crate::project_path::resolve(project_root, &pass.shader)
        .map_err(PipelineError::ShaderError)?;
    // Resolve the generated artifact separately: the source may be absent and
    // the artifact must obey the same project confinement as the source.
    if path
        .extension()
        .is_some_and(|extension| extension == "slang")
    {
        crate::shader::resolve_precompiled_wgsl(&path, project_root)
            .map_err(PipelineError::ShaderError)?;
    }
    let source = crate::shader::load_required_shader(&path, project_root).map_err(|error| {
        PipelineError::ShaderError(format!(
            "Required shader '{}' for pass '{}': {error}",
            pass.shader, pass.name
        ))
    })?;
    Ok((path, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline(version: u32, passes: &[(&str, &str, &str)]) -> PipelineFile {
        let passes: String = passes
            .iter()
            .map(|(name, kind, shader)| {
                format!("  - {{name: {name}, type: {kind}, shader: '{shader}'}}\n")
            })
            .collect();
        serde_norway::from_str(&format!("version: {version}\npasses:\n{passes}")).unwrap()
    }

    fn shaders(pipeline: &PipelineFile) -> Vec<&str> {
        pipeline
            .passes
            .iter()
            .map(|pass| pass.shader.as_str())
            .collect()
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "naive-pipeline-migration-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("shaders/passes")).unwrap();
        root
    }

    #[test]
    fn a_pipeline_from_the_0_1_22_init_keeps_its_engine_shaders() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/init-0.1.22");
        let pipeline = load_project_pipeline(&root.join("pipelines/render.yaml"), &root).unwrap();
        assert_eq!(
            shaders(&pipeline),
            [
                "builtin:shadow",
                "builtin:gbuffer",
                "builtin:deferred_light",
                "builtin:bloom",
                "builtin:tonemap",
                "builtin:fxaa",
            ]
        );
        for pass in &pipeline.passes {
            load_pass_shader(&root, pass, false).unwrap();
        }
    }

    #[test]
    fn version_1_maps_only_what_0_1_22_ignored_or_fell_back_from() {
        let root = temp_project("v1");
        for file in ["gbuffer.slang", "tonemap.slang"] {
            std::fs::write(root.join("shaders/passes").join(file), "// project shader").unwrap();
        }
        let mut file = pipeline(
            1,
            &[
                // 0.1.22 never read mesh-pass shaders, even existing ones.
                ("gbuffer", "rasterize", "shaders/passes/gbuffer.slang"),
                ("shadows", "shadow", "shaders/passes/shadow.slang"),
                // An existing fullscreen shader is the project's.
                ("tonemap", "fullscreen", "shaders/passes/tonemap.slang"),
                // A missing one fell back by its pass name ...
                (
                    "lighting",
                    "fullscreen",
                    "shaders/passes/deferred_light.slang",
                ),
                ("splats", "splat", "shaders/passes/gaussian_splat.slang"),
                // ... unless no engine shader matched it.
                ("vignette", "fullscreen", "shaders/passes/vignette.slang"),
                ("fxaa", "fullscreen", "builtin:fxaa"),
            ],
        );
        let notes = migrate_pipeline(&mut file, &root).unwrap();
        assert_eq!(
            shaders(&file),
            [
                "builtin:gbuffer",
                "builtin:shadow",
                "shaders/passes/tonemap.slang",
                "builtin:deferred_light",
                "builtin:gaussian_splat",
                "shaders/passes/vignette.slang",
                "builtin:fxaa",
            ]
        );
        assert_eq!(notes.len(), 4, "{notes:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_1_lighting_with_splats_uses_the_engine_compositor() {
        let root = temp_project("splats");
        std::fs::write(
            root.join("shaders/passes/deferred_light.slang"),
            "// project",
        )
        .unwrap();
        let mut file: PipelineFile = serde_norway::from_str(
            "version: 1\nresources:\n  - {name: splat_color, type: texture_2d, format: rgba16f}\n  - {name: splat_depth, type: texture_2d, format: depth32f}\npasses:\n  - {name: lighting, type: fullscreen, shader: shaders/passes/deferred_light.slang}\n  - {name: escape, type: fullscreen, shader: ../outside.slang}\n",
        )
        .unwrap();
        migrate_pipeline(&mut file, &root).unwrap();
        // 0.1.22 replaced the lighting shader whenever splats were composited;
        // an escaping reference is left for the strict load to reject.
        assert_eq!(
            shaders(&file),
            ["builtin:deferred_light", "../outside.slang"]
        );
        assert!(load_pass_shader(&root, &file.passes[1], true).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_2_references_are_strict_and_other_versions_are_rejected() {
        let root = temp_project("v2");
        let mut file = pipeline(
            2,
            &[(
                "lighting",
                "fullscreen",
                "shaders/passes/deferred_light.slang",
            )],
        );
        assert!(migrate_pipeline(&mut file, &root).unwrap().is_empty());
        assert_eq!(shaders(&file), ["shaders/passes/deferred_light.slang"]);
        let error = load_pass_shader(&root, &file.passes[0], false).unwrap_err();
        assert!(error.to_string().contains("Required shader"), "{error}");

        let mut file = pipeline(3, &[("lighting", "fullscreen", "builtin:deferred_light")]);
        let error = migrate_pipeline(&mut file, &root).unwrap_err();
        assert!(error.to_string().contains("version 3"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_engine_shader_selects_layout_independently_of_pass_name() {
        for (reference, expected) in [
            ("builtin:fxaa", FullscreenRole::Fxaa),
            ("builtin:bloom", FullscreenRole::Bloom),
            ("builtin:tonemap", FullscreenRole::Tonemap),
            ("builtin:deferred_light", FullscreenRole::Lighting),
        ] {
            assert_eq!(fullscreen_role(reference, "present"), expected);
            assert_eq!(fullscreen_role(reference, "fxaa_tonemap_bloom"), expected);
        }
        assert_eq!(
            fullscreen_role("effects/custom.slang", "fxaa_custom"),
            FullscreenRole::Fxaa
        );
    }
}
