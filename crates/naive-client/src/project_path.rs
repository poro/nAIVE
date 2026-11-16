//! Resolve paths supplied by scenes, scripts and editor commands inside a project.
//!
//! Existing ancestors are canonicalized, so a symlink cannot silently redirect
//! a read or a new output outside the project. Projects must not be modified by
//! an untrusted process concurrently with engine filesystem operations.

use std::path::{Component, Path, PathBuf};

pub fn resolve(root: &Path, relative: impl AsRef<Path>) -> Result<PathBuf, String> {
    let relative = relative.as_ref();
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("path must be relative to the project, nonempty and contain no '..'".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve project {}: {e}", root.display()))?;
    let mut resolved = root.clone();
    for part in relative.components() {
        if let Component::Normal(name) = part {
            resolved.push(name);
            match std::fs::symlink_metadata(&resolved) {
                Ok(_) => {
                    resolved = resolved.canonicalize().map_err(|e| {
                        format!("cannot resolve project path {}: {e}", resolved.display())
                    })?;
                    if !resolved.starts_with(&root) {
                        return Err("path resolves outside the project through a symlink".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot inspect {}: {e}", resolved.display())),
            }
        }
    }
    if resolved == root {
        return Err("path must name a file inside the project".into());
    }
    Ok(resolved)
}

pub fn resolve_with_extension(
    root: &Path,
    relative: impl AsRef<Path>,
    extension: &str,
) -> Result<PathBuf, String> {
    let relative = relative.as_ref();
    if !relative
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
    {
        return Err(format!("path must end in .{extension}"));
    }
    resolve(root, relative)
}

/// glTF can reference external buffers and images. Check those before the
/// importer opens them; checking just the top-level .gltf/.glb is insufficient.
pub fn validate_gltf_resources(root: &Path, file: &Path) -> Result<(), String> {
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let file = file.canonicalize().map_err(|e| e.to_string())?;
    if !file.starts_with(&canonical_root) {
        return Err("glTF path resolves outside the project".into());
    }
    let document = gltf::Gltf::open(&file).map_err(|e| format!("cannot inspect glTF: {e}"))?;
    let base = file.parent().ok_or("glTF has no parent directory")?;
    let base_relative = base
        .strip_prefix(&canonical_root)
        .map_err(|e| e.to_string())?;
    let uris = document
        .buffers()
        .filter_map(|buffer| match buffer.source() {
            gltf::buffer::Source::Uri(uri) => Some(uri),
            _ => None,
        })
        .chain(document.images().filter_map(|image| match image.source() {
            gltf::image::Source::Uri { uri, .. } => Some(uri),
            _ => None,
        }));
    for uri in uris {
        if uri.starts_with("data:") {
            continue;
        }
        // Percent-encoded URI paths are decoded by gltf's importer. Reject
        // them here rather than validate a different path from the one opened.
        if uri.contains('%') || uri.contains(':') || Path::new(uri).is_absolute() {
            return Err("external glTF resources must use plain relative project paths".into());
        }
        resolve(root, base_relative.join(uri))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gltf_secondary_files_cannot_escape_the_project() {
        let root = std::env::temp_dir().join(format!("naive-gltf-path-{}", std::process::id()));
        std::fs::create_dir_all(root.join("meshes")).unwrap();
        let file = root.join("meshes/test.gltf");
        for uri in [
            "../../outside.bin",
            "/tmp/outside.bin",
            "%2e%2e/outside.bin",
            "file:///tmp/outside.bin",
        ] {
            let document = serde_json::json!({"asset":{"version":"2.0"},"buffers":[{"uri":uri,"byteLength":4}]});
            std::fs::write(&file, document.to_string()).unwrap();
            assert!(
                validate_gltf_resources(&root, &file).is_err(),
                "accepted {uri}"
            );
        }
        let document = serde_json::json!({"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,AAAAAA==","byteLength":4}]});
        std::fs::write(&file, document.to_string()).unwrap();
        assert!(validate_gltf_resources(&root, &file).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paths_cannot_escape_through_traversal_or_symlinks() {
        let parent = std::env::temp_dir().join(format!(
            "naive-path-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = parent.join("project");
        let outside = parent.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        assert_eq!(
            resolve(&root, "captures/new.png").unwrap(),
            canonical_root.join("captures/new.png")
        );
        for path in ["", ".", "..", "../outside/a", "captures/../../a", "/tmp/a"] {
            assert!(resolve(&root, path).is_err(), "accepted {path}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
            std::os::unix::fs::symlink(outside.join("missing"), root.join("dangling")).unwrap();
            assert!(resolve(&root, "escape/new.yaml").is_err());
            assert!(resolve(&root, "dangling").is_err());
            std::fs::create_dir(root.join("assets")).unwrap();
            std::os::unix::fs::symlink(root.join("assets"), root.join("inside")).unwrap();
            assert_eq!(
                resolve(&root, "inside/new.yaml").unwrap(),
                canonical_root.join("assets/new.yaml")
            );
        }
        std::fs::remove_dir_all(parent).unwrap();
    }
}
