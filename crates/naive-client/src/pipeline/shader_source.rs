//! Explicit engine shaders and required project shader entry points.

use std::path::{Path, PathBuf};

use super::{PassDef, PassType, PipelineError};

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
        let root = project_root
            .canonicalize()
            .map_err(PipelineError::IoError)?;
        let artifact = crate::shader::precompiled_wgsl_path(&path);
        let artifact_relative = artifact
            .strip_prefix(&root)
            .map_err(|error| PipelineError::ShaderError(error.to_string()))?;
        crate::project_path::resolve(project_root, artifact_relative)
            .map_err(PipelineError::ShaderError)?;
    }
    let source = crate::shader::load_required_shader(&path).map_err(|error| {
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
