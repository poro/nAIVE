//! `naive build` — bundle a game project for standalone distribution.

use std::fs;
use std::path::Path;

use crate::project_config::NaiveConfig;

/// Content directories to include in the bundle.
const CONTENT_DIRS: &[&str] = &[
    "scenes",
    "logic",
    "assets",
    "shaders",
    "pipelines",
    "input",
    "events",
    "tests",
];

pub fn bundle_project(
    config: &NaiveConfig,
    project_root: &Path,
    target: Option<&str>,
) -> Result<(), String> {
    // The bundle contains the engine binary this command is running as, so it
    // can only be for the platform it is running on.
    let host = current_platform();
    let target_name = target.unwrap_or(host);
    if target_name != host {
        return Err(format!(
            "Cannot build for '{}' on {}: `naive build` bundles the engine binary it is \
             running as and cannot cross-compile. Run it on the platform you want to ship.",
            target_name, host
        ));
    }
    validate_bundle_name(&config.name)?;
    let dist_name = format!("{}-{}", config.name, target_name);
    let project_root = project_root.canonicalize().map_err(|e| e.to_string())?;
    let dist_parent = project_root.join("dist");
    reject_symlink(&dist_parent)?;
    reject_symlink(&dist_parent.join(&dist_name))?;
    let dist_dir = crate::project_path::resolve(&project_root, Path::new("dist").join(&dist_name))?;

    // Validate all source content before replacing a previous working bundle.
    // Reject symlinks (including internal ones) so packaging cannot recurse into
    // itself, follow an external link, or copy a link's target accidentally.
    for directory in CONTENT_DIRS {
        let source = project_root.join(directory);
        if source.symlink_metadata().is_ok() {
            validate_content_tree(&source)?;
        }
    }
    let config_src = project_root.join("naive.yaml");
    if config_src.symlink_metadata().is_ok() {
        validate_content_tree(&config_src)?;
    }

    println!("Building {} for {}...", config.name, target_name);

    // Clean and create dist directory
    if dist_dir.exists() {
        fs::remove_dir_all(&dist_dir).map_err(|e| format!("Failed to clean dist/: {}", e))?;
    }
    fs::create_dir_all(&dist_dir).map_err(|e| format!("Failed to create dist/: {}", e))?;

    // Copy the running binary
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to find runtime binary: {}", e))?;
    let dest_binary = dist_dir.join("naive-runtime");
    fs::copy(&exe_path, &dest_binary)
        .map_err(|e| format!("Failed to copy runtime binary: {}", e))?;

    // Copy naive.yaml
    if config_src.exists() {
        fs::copy(&config_src, dist_dir.join("naive.yaml"))
            .map_err(|e| format!("Failed to copy naive.yaml: {}", e))?;
    }

    // Copy content directories
    let mut total_size: u64 = 0;
    for dir_name in CONTENT_DIRS {
        let src = project_root.join(dir_name);
        if src.is_dir() {
            let dest = dist_dir.join(dir_name);
            let size = copy_dir_recursive(&src, &dest)?;
            total_size += size;
            println!("  {} ({} files)", dir_name, count_files(&dest));
        }
    }

    // Add the binary size
    total_size += fs::metadata(&dest_binary).map(|m| m.len()).unwrap_or(0);

    // Write launcher script
    let scene_arg = config
        .default_scene
        .as_deref()
        .unwrap_or("scenes/main.yaml");

    let quoted_scene = format!("'{}'", scene_arg.replace('\'', "'\\''"));
    let launcher = format!(
        "#!/bin/sh\nset -eu\ncd \"$(dirname \"$0\")\"\nexec ./naive-runtime --project . --scene {} \"$@\"\n",
        quoted_scene
    );
    let launcher_path = dist_dir.join("launch.sh");
    fs::write(&launcher_path, launcher).map_err(|e| format!("Failed to write launcher: {}", e))?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set launcher permissions: {}", e))?;
        fs::set_permissions(&dest_binary, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set binary permissions: {}", e))?;
    }

    println!();
    println!(
        "  Bundle ready: dist/{} ({:.1} MB)",
        dist_name,
        total_size as f64 / (1024.0 * 1024.0)
    );

    Ok(())
}

/// The platform this binary was built for. Windows is not supported yet.
fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn validate_bundle_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
    {
        return Err("Project name must start with a letter or number and contain only letters, numbers, '.', '_' or '-'".into());
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Refusing symlink in bundle path: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot inspect {}: {e}", path.display())),
    }
}

fn validate_content_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Bundle content must not contain symlinks: {}",
            path.display()
        ));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            validate_content_tree(&entry.map_err(|e| e.to_string())?.path())?;
        }
    } else if !metadata.is_file() {
        return Err(format!(
            "Bundle content must be regular files: {}",
            path.display()
        ));
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<u64, String> {
    fs::create_dir_all(dest).map_err(|e| format!("Failed to create {}: {}", dest.display(), e))?;

    let mut total = 0u64;
    for entry in
        fs::read_dir(src).map_err(|e| format!("Failed to read {}: {}", src.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Directory entry error: {}", e))?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        reject_symlink(&src_path)?;

        if src_path.is_dir() {
            total += copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("Failed to copy {}: {}", src_path.display(), e))?;
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}

fn count_files(dir: &Path) -> usize {
    if !dir.is_dir() {
        return 0;
    }
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| {
            if e.path().is_dir() {
                count_files(&e.path())
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_cannot_redirect_bundle_deletion() {
        for name in [
            "",
            ".",
            "..",
            "../victim",
            "a/../../victim",
            "/tmp/victim",
            "a\\victim",
        ] {
            assert!(validate_bundle_name(name).is_err(), "accepted {name}");
        }
        assert!(validate_bundle_name("my-game_1.2").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn bundle_rejects_symlinked_output_and_content_before_deleting_old_bundle() {
        let project =
            std::env::temp_dir().join(format!("naive-build-links-{}", std::process::id()));
        fs::create_dir_all(project.join("keep")).unwrap();
        fs::write(project.join("keep/important"), "preserve").unwrap();
        let config: NaiveConfig = serde_yaml::from_str("name: demo\nversion: '1'\n").unwrap();
        std::os::unix::fs::symlink(project.join("keep"), project.join("dist")).unwrap();
        assert!(bundle_project(&config, &project, None).is_err());
        assert_eq!(
            fs::read_to_string(project.join("keep/important")).unwrap(),
            "preserve"
        );
        fs::remove_file(project.join("dist")).unwrap();
        let bundle = project
            .join("dist")
            .join(format!("demo-{}", current_platform()));
        fs::create_dir(project.join("dist")).unwrap();
        std::os::unix::fs::symlink(project.join("keep"), &bundle).unwrap();
        assert!(bundle_project(&config, &project, None).is_err());
        assert!(project.join("keep/important").exists());
        fs::remove_file(&bundle).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("previous-bundle"), "preserve").unwrap();
        fs::create_dir(project.join("assets")).unwrap();
        std::os::unix::fs::symlink(project.join("keep"), project.join("assets/external")).unwrap();
        assert!(bundle_project(&config, &project, None).is_err());
        assert!(bundle.join("previous-bundle").exists());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn building_for_another_platform_is_refused() {
        let config: NaiveConfig = serde_yaml::from_str("name: demo\nversion: \"0.1.0\"\n").unwrap();
        let project = std::env::temp_dir().join("naive_test_build_other_platform");
        let _ = fs::remove_dir_all(&project);
        fs::create_dir_all(&project).unwrap();

        for target in ["windows", "some-other-os"] {
            let err = bundle_project(&config, &project, Some(target)).unwrap_err();
            assert!(err.contains("cannot cross-compile"), "{err}");
        }

        // A refused build writes nothing.
        assert!(!project.join("dist").exists());
        let _ = fs::remove_dir_all(&project);
    }
}
