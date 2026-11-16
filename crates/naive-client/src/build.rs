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
    let dist_name = format!("{}-{}", config.name, target_name);
    let dist_dir = project_root.join("dist").join(&dist_name);

    println!("Building {} for {}...", config.name, target_name);

    // Clean and create dist directory
    if dist_dir.exists() {
        fs::remove_dir_all(&dist_dir)
            .map_err(|e| format!("Failed to clean dist/: {}", e))?;
    }
    fs::create_dir_all(&dist_dir)
        .map_err(|e| format!("Failed to create dist/: {}", e))?;

    // Copy the running binary
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to find runtime binary: {}", e))?;
    let dest_binary = dist_dir.join("naive-runtime");
    fs::copy(&exe_path, &dest_binary)
        .map_err(|e| format!("Failed to copy runtime binary: {}", e))?;

    // Copy naive.yaml
    let config_src = project_root.join("naive.yaml");
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
    total_size += fs::metadata(&dest_binary)
        .map(|m| m.len())
        .unwrap_or(0);

    // Write launcher script
    let scene_arg = config
        .default_scene
        .as_deref()
        .unwrap_or("scenes/main.yaml");

    let launcher = format!(
        "#!/bin/sh\ncd \"$(dirname \"$0\")\"\n./naive-runtime --project . --scene {}\n",
        scene_arg
    );
    let launcher_path = dist_dir.join("launch.sh");
    fs::write(&launcher_path, launcher)
        .map_err(|e| format!("Failed to write launcher: {}", e))?;

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

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<u64, String> {
    fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create {}: {}", dest.display(), e))?;

    let mut total = 0u64;
    for entry in fs::read_dir(src).map_err(|e| format!("Failed to read {}: {}", src.display(), e))? {
        let entry = entry.map_err(|e| format!("Directory entry error: {}", e))?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());

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
