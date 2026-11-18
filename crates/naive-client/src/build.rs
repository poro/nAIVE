//! `naive build` — bundle a game project for standalone distribution.
//!
//! A bundle holds a player binary and the game's content. The player is the
//! `naive` program built without the Slang SDK (the `naive-player` crate), so
//! it needs no shader compiler or SDK libraries at run time. Slang shaders are
//! compiled to WGSL here instead, at build time, and written next to their
//! source as `<name>.slang.wgsl`, which the player loads.
//!
//! On macOS the bundle is `dist/<name>.app`; on Linux it is a directory,
//! `dist/<name>-linux`. Either way, starting the player with no arguments runs
//! the game packaged beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Directory holding the game's content inside a bundle: `Contents/Resources/game`
/// in a macOS app, `game` next to the binary on Linux.
pub const GAME_DIR: &str = "game";

pub fn bundle_project(
    config: &NaiveConfig,
    project_root: &Path,
    target: Option<&str>,
) -> Result<PathBuf, String> {
    // The bundle contains a binary built for this machine, so it can only be
    // for the platform it is running on.
    let host = current_platform();
    let target_name = target.unwrap_or(host);
    if target_name != host {
        return Err(format!(
            "Cannot build for '{}' on {}: `naive build` bundles an engine binary built for \
             this machine and cannot cross-compile. Run it on the platform you want to ship.",
            target_name, host
        ));
    }
    let player = player_binary()?;
    bundle_with_player(config, project_root, target_name, &player)
}

/// The binary to ship: `naive-player` next to this executable, or this
/// executable itself when it was built without Slang and so needs no SDK.
fn player_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Failed to find this binary: {e}"))?;
    let player = exe.with_file_name("naive-player");
    if player.is_file() {
        return Ok(player);
    }
    if !cfg!(feature = "slang") {
        return Ok(exe);
    }
    Err(format!(
        "naive-player was not found next to {}. Bundles ship it instead of this binary because \
         it runs without the Slang SDK. Build it with `cargo build --release -p naive-player` \
         (on its own, so Slang stays out of it).",
        exe.display()
    ))
}

/// Find the game a bundled player was shipped with, if this binary is one.
pub fn bundled_game_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?;
    [dir.join("../Resources").join(GAME_DIR), dir.join(GAME_DIR)]
        .into_iter()
        .find(|candidate| candidate.join("naive.yaml").is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn bundle_with_player(
    config: &NaiveConfig,
    project_root: &Path,
    target_name: &str,
    player: &Path,
) -> Result<PathBuf, String> {
    validate_bundle_name(&config.name)?;
    let macos = target_name == "macos";
    let dist_name = if macos {
        format!("{}.app", config.name)
    } else {
        format!("{}-{}", config.name, target_name)
    };
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

    // Compile required entry points before touching an existing good bundle.
    // Import-only modules are dependencies of these files, not entry points.
    let shaders = prepare_shaders(config, &project_root)?;

    println!("Building {} for {}...", config.name, target_name);

    // Clean and create dist directory
    if dist_dir.exists() {
        fs::remove_dir_all(&dist_dir).map_err(|e| format!("Failed to clean dist/: {}", e))?;
    }
    let (binary_dir, game_dir) = if macos {
        let contents = dist_dir.join("Contents");
        (
            contents.join("MacOS"),
            contents.join("Resources").join(GAME_DIR),
        )
    } else {
        (dist_dir.clone(), dist_dir.join(GAME_DIR))
    };
    for dir in [&binary_dir, &game_dir] {
        fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    }

    let dest_binary = binary_dir.join(&config.name);
    fs::copy(player, &dest_binary).map_err(|e| format!("Failed to copy player binary: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest_binary, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set binary permissions: {}", e))?;
    }

    if config_src.exists() {
        fs::copy(&config_src, game_dir.join("naive.yaml"))
            .map_err(|e| format!("Failed to copy naive.yaml: {}", e))?;
    }

    let mut total_size: u64 = fs::metadata(&dest_binary).map(|m| m.len()).unwrap_or(0);
    for dir_name in CONTENT_DIRS {
        let src = project_root.join(dir_name);
        if src.is_dir() {
            let dest = game_dir.join(dir_name);
            total_size += copy_dir_recursive(&src, &dest)?;
            println!("  {} ({} files)", dir_name, count_files(&dest));
        }
    }

    for pipeline in &shaders.pipelines {
        let relative = pipeline
            .strip_prefix(&project_root)
            .map_err(|e| e.to_string())?;
        total_size += copy_project_file(pipeline, &game_dir.join(relative))?;
    }
    for (relative, shader) in &shaders.artifacts {
        // Keep sources for inspection, including shaders outside CONTENT_DIRS.
        if shader.source.is_file() {
            total_size +=
                copy_project_file(&shader.source, &game_dir.join(&shader.relative_source))?;
        }
        let out = game_dir.join(relative);
        fs::create_dir_all(out.parent().ok_or("Shader has no parent directory")?)
            .map_err(|e| e.to_string())?;
        fs::write(&out, &shader.wgsl)
            .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
        total_size += shader.wgsl.len() as u64;
    }
    if !shaders.artifacts.is_empty() {
        println!(
            "  Required project shaders packaged as WGSL: {}",
            shaders.artifacts.len()
        );
    }

    if macos {
        write_info_plist(config, &dist_dir.join("Contents/Info.plist"))?;
        sign_ad_hoc(&dist_dir);
    }

    println!();
    println!(
        "  Bundle ready: dist/{} ({:.1} MB)",
        dist_name,
        total_size as f64 / (1024.0 * 1024.0)
    );
    if macos {
        println!("  Run it with: open dist/{dist_name}");
    } else {
        println!("  Run it with: ./dist/{dist_name}/{}", config.name);
    }

    Ok(dist_dir)
}

struct PackagedShader {
    source: PathBuf,
    relative_source: PathBuf,
    wgsl: String,
}

#[derive(Default)]
struct ShaderPackage {
    pipelines: BTreeSet<PathBuf>,
    artifacts: BTreeMap<PathBuf, PackagedShader>,
}

/// Discover pipeline YAML in packaged content, plus an explicitly configured
/// pipeline anywhere inside the project. Compile only referenced entry points;
/// Slang itself loads their imports. A missing vs_main/fs_main on a referenced
/// file is a broken pass, never evidence that it was merely an import module.
fn prepare_shaders(config: &NaiveConfig, project_root: &Path) -> Result<ShaderPackage, String> {
    let canonical_root = project_root.canonicalize().map_err(|e| e.to_string())?;
    let project_root = canonical_root.as_path();
    let mut package = ShaderPackage::default();
    let configured = config
        .default_pipeline
        .as_ref()
        .map(|relative| validate_packaged_reference(project_root, Path::new(relative)))
        .transpose()?;
    let mut candidates = BTreeSet::new();
    for directory in CONTENT_DIRS {
        let directory = project_root.join(directory);
        if directory.is_dir() {
            let mut files = Vec::new();
            collect_files(&directory, "yaml", &mut files)?;
            collect_files(&directory, "yml", &mut files)?;
            candidates.extend(files);
        }
    }
    if let Some(path) = &configured {
        validate_content_tree(path)?;
        candidates.insert(path.clone());
    }
    for path in candidates {
        let contents = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let required_pipeline =
            path.starts_with(project_root.join("pipelines")) || configured.as_ref() == Some(&path);
        // Other packaged YAML may contain scenes, materials or input maps.
        let has_passes = serde_norway::from_str::<serde_norway::Value>(&contents)
            .ok()
            .is_some_and(|value| value.get("passes").is_some());
        if !required_pipeline && !has_passes {
            continue;
        }
        let mut pipeline: crate::pipeline::PipelineFile = serde_norway::from_str(&contents)
            .map_err(|e| format!("Invalid packaged pipeline {}: {e}", path.display()))?;
        // The bundled player reads the file verbatim and migrates it the same
        // way, so package the shaders the migrated pipeline uses.
        let notes = crate::pipeline::migrate_pipeline(&mut pipeline, project_root)
            .map_err(|e| format!("Packaged pipeline {}: {e}", path.display()))?;
        if !notes.is_empty() {
            println!(
                "  {} is a version 1 pipeline; packaging its 0.1.22 shaders ({})",
                path.display(),
                notes.join("; ")
            );
        }
        let has_splats = pipeline.resources.iter().any(|r| r.name == "splat_color")
            && pipeline.resources.iter().any(|r| r.name == "splat_depth");
        for pass in pipeline.passes {
            if !pass.shader.starts_with("builtin:") {
                let reference = Path::new(&pass.shader);
                validate_packaged_reference(project_root, reference)?;
                if reference.extension().is_some_and(|ext| ext == "slang") {
                    validate_packaged_reference(
                        project_root,
                        &crate::shader::precompiled_wgsl_path(reference),
                    )?;
                }
            }
            let (source, wgsl) =
                crate::pipeline::shader_source::load_pass_shader(project_root, &pass, has_splats)
                    .map_err(|e| format!("Packaged pipeline {}: {e}", path.display()))?;
            if pass.shader.starts_with("builtin:") {
                continue;
            }
            let relative_source = source
                .strip_prefix(project_root)
                .map_err(|error| error.to_string())?
                .to_path_buf();
            let relative_artifact = if source.extension().is_some_and(|ext| ext == "wgsl") {
                relative_source.clone()
            } else {
                crate::shader::precompiled_wgsl_path(&relative_source)
            };
            if let Some(existing) = package.artifacts.get(&relative_artifact) {
                if existing.wgsl != wgsl {
                    return Err(format!(
                        "Conflicting required shaders write different WGSL to {} (pass '{}')",
                        relative_artifact.display(),
                        pass.name
                    ));
                }
            }
            package.artifacts.insert(
                relative_artifact,
                PackagedShader {
                    source,
                    relative_source,
                    wgsl,
                },
            );
        }
        package.pipelines.insert(path);
    }
    package_forward_shader(project_root, &mut package)?;
    Ok(package)
}

/// Scenes drawn without a render pipeline use the project's forward shader
/// when it has one. The player cannot compile Slang, so package its WGSL;
/// one that does not compile fails the build rather than shipping a game
/// that silently draws with the engine's shader instead.
fn package_forward_shader(project_root: &Path, package: &mut ShaderPackage) -> Result<(), String> {
    let relative_source = PathBuf::from(crate::shader::FORWARD_SHADER);
    let relative_artifact = crate::shader::precompiled_wgsl_path(&relative_source);
    if !project_root.join(&relative_source).exists()
        && !project_root.join(&relative_artifact).exists()
    {
        return Ok(());
    }
    let source = validate_packaged_reference(project_root, &relative_source)?;
    validate_packaged_reference(project_root, &relative_artifact)?;
    let wgsl = crate::shader::load_required_shader(&source)
        .map_err(|error| format!("Forward shader '{}': {error}", relative_source.display()))?;
    if let Some(existing) = package.artifacts.get(&relative_artifact) {
        if existing.wgsl != wgsl {
            return Err(format!(
                "Conflicting required shaders write different WGSL to {} (forward shader)",
                relative_artifact.display()
            ));
        }
    }
    package.artifacts.insert(
        relative_artifact,
        PackagedShader {
            source,
            relative_source,
            wgsl,
        },
    );
    Ok(())
}

/// Check the authored path before canonicalization erases internal symlink
/// aliases. The bundle retains naive.yaml and pipeline references verbatim.
fn validate_packaged_reference(project_root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let resolved = crate::project_path::resolve(project_root, relative)?;
    let mut authored = project_root.to_path_buf();
    for component in relative.components() {
        if let std::path::Component::Normal(name) = component {
            authored.push(name);
            reject_symlink(&authored)?;
        }
    }
    Ok(resolved)
}

fn copy_project_file(source: &Path, destination: &Path) -> Result<u64, String> {
    fs::create_dir_all(destination.parent().ok_or("File has no parent directory")?)
        .map_err(|e| e.to_string())?;
    fs::copy(source, destination).map_err(|e| format!("Failed to copy {}: {e}", source.display()))
}

fn collect_files(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_files(&path, extension, out)?;
        } else if path.extension().is_some_and(|ext| ext == extension) {
            out.push(path);
        }
    }
    Ok(())
}

fn write_info_plist(config: &NaiveConfig, path: &Path) -> Result<(), String> {
    let name = &config.name;
    let version = xml_escape(&config.version);
    let identifier = format!("dev.naive.game.{}", name.replace('_', "-"));
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>{name}</string>
    <key>CFBundleIdentifier</key>
    <string>{identifier}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>{name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.games</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
"#
    );
    fs::write(path, plist).map_err(|e| format!("Failed to write Info.plist: {}", e))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Seal the app with an ad-hoc signature so macOS runs it on this Mac. To
/// ship it to other Macs it must be signed with a Developer ID and notarized
/// (docs/DEVELOPMENT.md).
fn sign_ad_hoc(app: &Path) {
    match std::process::Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .output()
    {
        Ok(output) if output.status.success() => println!("  Signed ad hoc (runs on this Mac)"),
        Ok(output) => println!(
            "  Warning: ad-hoc signing failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(e) => println!("  Warning: codesign unavailable ({e}); the app is unsigned"),
    }
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

    fn temp_project(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "naive-build-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        let project = temp_project("links");
        let player = project.join("player");
        fs::write(&player, "player").unwrap();
        fs::create_dir_all(project.join("keep")).unwrap();
        fs::write(project.join("keep/important"), "preserve").unwrap();
        let config: NaiveConfig = serde_norway::from_str("name: demo\nversion: '1'\n").unwrap();
        let platform = current_platform();
        let bundle_name = if platform == "macos" {
            "demo.app".to_string()
        } else {
            format!("demo-{platform}")
        };
        std::os::unix::fs::symlink(project.join("keep"), project.join("dist")).unwrap();
        assert!(bundle_with_player(&config, &project, platform, &player).is_err());
        assert_eq!(
            fs::read_to_string(project.join("keep/important")).unwrap(),
            "preserve"
        );
        fs::remove_file(project.join("dist")).unwrap();
        let bundle = project.join("dist").join(&bundle_name);
        fs::create_dir(project.join("dist")).unwrap();
        std::os::unix::fs::symlink(project.join("keep"), &bundle).unwrap();
        assert!(bundle_with_player(&config, &project, platform, &player).is_err());
        assert!(project.join("keep/important").exists());
        fs::remove_file(&bundle).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("previous-bundle"), "preserve").unwrap();
        fs::create_dir(project.join("assets")).unwrap();
        std::os::unix::fs::symlink(project.join("keep"), project.join("assets/external")).unwrap();
        assert!(bundle_with_player(&config, &project, platform, &player).is_err());
        assert!(bundle.join("previous-bundle").exists());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn building_for_another_platform_is_refused() {
        let config: NaiveConfig =
            serde_norway::from_str("name: demo\nversion: \"0.1.0\"\n").unwrap();
        let project = temp_project("other-platform");

        for target in ["windows", "some-other-os"] {
            let err = bundle_project(&config, &project, Some(target)).unwrap_err();
            assert!(err.contains("cannot cross-compile"), "{err}");
        }

        // A refused build writes nothing.
        assert!(!project.join("dist").exists());
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn bundle_layout_puts_the_player_beside_its_game() {
        let project = temp_project("layout");
        let player = project.join("player-binary");
        fs::write(&player, "player").unwrap();
        fs::write(
            project.join("naive.yaml"),
            "name: layout\nversion: '1.2 <beta>'\n",
        )
        .unwrap();
        fs::create_dir_all(project.join("scenes")).unwrap();
        fs::write(project.join("scenes/main.yaml"), "name: main\n").unwrap();
        let config: NaiveConfig =
            serde_norway::from_str(&fs::read_to_string(project.join("naive.yaml")).unwrap())
                .unwrap();
        let platform = current_platform();
        let bundle = bundle_with_player(&config, &project, platform, &player).unwrap();
        let (binary, game) = if platform == "macos" {
            let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
            assert!(plist.contains("<string>layout</string>"));
            assert!(plist.contains("1.2 &lt;beta&gt;"));
            (
                bundle.join("Contents/MacOS/layout"),
                bundle.join("Contents/Resources/game"),
            )
        } else {
            (bundle.join("layout"), bundle.join("game"))
        };
        assert_eq!(fs::read_to_string(binary).unwrap(), "player");
        assert!(game.join("naive.yaml").is_file());
        assert!(game.join("scenes/main.yaml").is_file());
        fs::remove_dir_all(project).unwrap();
    }

    fn shader_config(project: &Path, shader: &str) -> NaiveConfig {
        fs::create_dir_all(project.join("effects")).unwrap();
        fs::write(
            project.join("effects/render.yaml"),
            format!("version: 2\npasses:\n  - name: fxaa_custom\n    type: fullscreen\n    shader: {shader}\n"),
        ).unwrap();
        let yaml = "name: shader-test\nversion: '1'\ndefault_pipeline: effects/render.yaml\n";
        fs::write(project.join("naive.yaml"), yaml).unwrap();
        serde_norway::from_str(yaml).unwrap()
    }

    #[test]
    fn artifact_only_shader_and_pipeline_outside_standard_directories_are_packaged() {
        let project = temp_project("artifact");
        let config = shader_config(&project, "effects/custom.slang");
        let wgsl = crate::shader::get_triangle_wgsl();
        fs::write(project.join("effects/custom.slang.wgsl"), &wgsl).unwrap();
        let player = project.join("player");
        fs::write(&player, "player").unwrap();
        let bundle = bundle_with_player(&config, &project, "linux", &player).unwrap();
        let game = bundle.join("game");
        assert!(game.join("effects/render.yaml").is_file());
        assert!(!game.join("effects/custom.slang").exists());
        assert_eq!(
            fs::read_to_string(game.join("effects/custom.slang.wgsl")).unwrap(),
            wgsl
        );
        let pipeline = crate::pipeline::load_pipeline(&game.join("effects/render.yaml")).unwrap();
        let (_, loaded) =
            crate::pipeline::shader_source::load_pass_shader(&game, &pipeline.passes[0], false)
                .unwrap();
        assert_eq!(loaded, wgsl);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn missing_or_invalid_required_shader_cannot_replace_a_good_bundle() {
        let project = temp_project("required");
        let config = shader_config(&project, "effects/custom.slang");
        let previous = project.join("dist/shader-test-linux/previous");
        fs::create_dir_all(previous.parent().unwrap()).unwrap();
        fs::write(&previous, "good bundle").unwrap();
        let player = project.join("player");
        fs::write(&player, "player").unwrap();
        for content in [None, Some("invalid WGSL"), Some("fn helper() {}")] {
            if let Some(content) = content {
                fs::write(project.join("effects/custom.slang.wgsl"), content).unwrap();
            }
            let error = bundle_with_player(&config, &project, "linux", &player).unwrap_err();
            assert!(
                error.contains("Required shader 'effects/custom.slang' for pass 'fxaa_custom'"),
                "{error}"
            );
            assert_eq!(fs::read_to_string(&previous).unwrap(), "good bundle");
        }
        fs::remove_dir_all(project).unwrap();
    }

    /// A project's own forward shader reaches the bundle as WGSL that the
    /// player loads; without one nothing is added, and a broken one fails the
    /// build instead of shipping the engine's shader in its place.
    #[test]
    fn a_custom_forward_shader_is_packaged_for_the_player() {
        let project = temp_project("forward");
        let yaml = "name: forward\nversion: '1'\n";
        fs::write(project.join("naive.yaml"), yaml).unwrap();
        let config: NaiveConfig = serde_norway::from_str(yaml).unwrap();
        let player = project.join("player");
        fs::write(&player, "player").unwrap();
        let artifact =
            crate::shader::precompiled_wgsl_path(Path::new(crate::shader::FORWARD_SHADER));

        let bundle = bundle_with_player(&config, &project, "linux", &player).unwrap();
        let game = bundle.join("game");
        assert!(!game.join(&artifact).exists());
        assert_eq!(
            crate::shader::load_project_forward_shader(&game),
            crate::shader::get_mesh_forward_wgsl()
        );

        // An artifact-only project packages the same way with or without Slang.
        let custom = format!(
            "{}\n// custom forward\n",
            crate::shader::get_mesh_forward_wgsl()
        );
        fs::create_dir_all(project.join("shaders/passes")).unwrap();
        fs::write(project.join(&artifact), &custom).unwrap();
        let bundle = bundle_with_player(&config, &project, "linux", &player).unwrap();
        let game = bundle.join("game");
        assert_eq!(fs::read_to_string(game.join(&artifact)).unwrap(), custom);
        assert_eq!(crate::shader::load_project_forward_shader(&game), custom);

        fs::write(project.join(&artifact), "fn helper() {}").unwrap();
        let error = bundle_with_player(&config, &project, "linux", &player).unwrap_err();
        assert!(
            error.contains("Forward shader 'shaders/passes/mesh_forward.slang'")
                && error.contains("vs_main"),
            "{error}"
        );
        fs::remove_dir_all(project).unwrap();
    }

    /// The usual case: only the Slang source exists, and the build compiles
    /// it (with its imports) for the player.
    #[cfg(feature = "slang")]
    #[test]
    fn a_forward_shader_source_is_compiled_for_the_player() {
        let project = temp_project("forward-source");
        let yaml = "name: forward\nversion: '1'\n";
        fs::write(project.join("naive.yaml"), yaml).unwrap();
        let config: NaiveConfig = serde_norway::from_str(yaml).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/release-smoke-fixture/assets/effects");
        let passes = project.join("shaders/passes");
        fs::create_dir_all(&passes).unwrap();
        fs::copy(
            fixture.join("release_marker.slang"),
            passes.join("mesh_forward.slang"),
        )
        .unwrap();
        fs::copy(
            fixture.join("release_palette.slang"),
            passes.join("release_palette.slang"),
        )
        .unwrap();
        let prepared = prepare_shaders(&config, &project).unwrap();
        let wgsl = &prepared.artifacts[Path::new("shaders/passes/mesh_forward.slang.wgsl")].wgsl;
        naga::front::wgsl::parse_str(wgsl).unwrap();
        assert_eq!(&crate::shader::load_project_forward_shader(&project), wgsl);

        fs::write(passes.join("mesh_forward.slang"), "broken source").unwrap();
        let error = prepare_shaders(&config, &project).err().unwrap();
        assert!(error.contains("Forward shader"), "{error}");
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn every_packaged_pipeline_is_checked_even_when_not_the_default() {
        let project = temp_project("other-pipelines");
        let config: NaiveConfig = serde_norway::from_str("name: demo\nversion: '1'\n").unwrap();
        for directory in ["pipelines", "assets/effects"] {
            fs::create_dir_all(project.join(directory)).unwrap();
            let pipeline = project.join(directory).join("unused.yml");
            fs::write(&pipeline, "version: 2\npasses:\n - {name: bloom_custom, type: fullscreen, shader: effects/missing.slang}\n").unwrap();
            let error = prepare_shaders(&config, &project).err().unwrap();
            assert!(error.contains("bloom_custom"), "{error}");
            fs::remove_file(pipeline).unwrap();
        }
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn a_project_from_the_0_1_22_init_packages_with_engine_shaders() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/init-0.1.22");
        let config: NaiveConfig =
            serde_norway::from_str(&fs::read_to_string(fixture.join("naive.yaml")).unwrap())
                .unwrap();
        let package = prepare_shaders(&config, &fixture).unwrap();
        // Every pass maps to an engine shader, so nothing needs compiling.
        assert!(package.artifacts.is_empty());
        assert_eq!(package.pipelines.len(), 1);
    }

    #[test]
    fn current_demo_and_new_projects_explicitly_use_engine_shaders() {
        let demo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../project")
            .canonicalize()
            .unwrap();
        let config = crate::project_config::load_config(&demo.join("naive.yaml")).unwrap();
        let prepared = prepare_shaders(&config, &demo).unwrap();
        assert_eq!(prepared.pipelines.len(), 1);
        assert!(prepared.artifacts.is_empty());

        let project = temp_project("init");
        let generated = project.join("generated");
        crate::init::create_project(generated.to_str().unwrap()).unwrap();
        let config = crate::project_config::load_config(&generated.join("naive.yaml")).unwrap();
        let prepared = prepare_shaders(&config, &generated).unwrap();
        assert_eq!(prepared.pipelines.len(), 1);
        assert!(prepared.artifacts.is_empty());
        fs::remove_dir_all(project).unwrap();
    }

    #[cfg(feature = "slang")]
    #[test]
    fn referenced_entrypoints_compile_imports_and_unreferenced_modules_are_not_passes() {
        let project = temp_project("slang-imports");
        let config = shader_config(&project, "effects/custom.slang");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/release-smoke-fixture/assets/effects");
        fs::copy(
            fixture.join("release_marker.slang"),
            project.join("effects/custom.slang"),
        )
        .unwrap();
        fs::copy(
            fixture.join("release_palette.slang"),
            project.join("effects/release_palette.slang"),
        )
        .unwrap();
        fs::create_dir(project.join("shaders")).unwrap();
        fs::write(
            project.join("shaders/unreferenced.slang"),
            "not a valid standalone shader",
        )
        .unwrap();
        let prepared = prepare_shaders(&config, &project).unwrap();
        assert_eq!(prepared.artifacts.len(), 1);
        let wgsl = &prepared.artifacts[Path::new("effects/custom.slang.wgsl")].wgsl;
        naga::front::wgsl::parse_str(wgsl).unwrap();

        // The same import-only module becomes an error if a pass references it.
        shader_config(&project, "effects/release_palette.slang");
        let error = prepare_shaders(&config, &project).err().unwrap();
        assert!(
            error.contains("Required shader 'effects/release_palette.slang'"),
            "{error}"
        );
        assert!(error.contains("entry point"), "{error}");

        // A stale good artifact must not hide broken source from the compiler.
        shader_config(&project, "effects/custom.slang");
        fs::write(project.join("effects/custom.slang.wgsl"), wgsl).unwrap();
        fs::write(project.join("effects/custom.slang"), "broken source").unwrap();
        assert!(prepare_shaders(&config, &project).is_err());
        fs::remove_dir_all(project).unwrap();
    }

    #[cfg(feature = "slang")]
    #[test]
    fn direct_and_compiled_shaders_cannot_silently_overwrite_each_other() {
        let project = temp_project("artifact-collision");
        let config = shader_config(&project, "effects/custom.slang");
        fs::write(
            project.join("effects/custom.slang"),
            include_str!("../../../project/shaders/passes/triangle.slang"),
        )
        .unwrap();
        fs::write(
            project.join("effects/custom.slang.wgsl"),
            crate::shader::get_triangle_wgsl(),
        )
        .unwrap();
        let first = fs::read_to_string(project.join("effects/render.yaml")).unwrap();
        let second =
            "  - name: second_pass\n    type: fullscreen\n    shader: ./effects/custom.slang.wgsl\n";
        for pipeline in [
            format!("{first}{second}"),
            format!(
                "version: 2\npasses:\n{second}{}",
                first.split("passes:\n").nth(1).unwrap()
            ),
        ] {
            fs::write(project.join("effects/render.yaml"), pipeline).unwrap();
            let error = prepare_shaders(&config, &project).err().unwrap();
            assert!(error.contains("Conflicting required shaders"), "{error}");
        }
        fs::remove_dir_all(project).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configured_pipeline_and_shader_aliases_outside_content_directories_are_rejected() {
        let project = temp_project("shader-aliases");
        let config = shader_config(&project, "effects/custom.slang");
        fs::rename(project.join("effects"), project.join("actual-effects")).unwrap();
        std::os::unix::fs::symlink("actual-effects", project.join("effects")).unwrap();
        let error = prepare_shaders(&config, &project).err().unwrap();
        assert!(error.contains("symlink"), "{error}");
        fs::remove_file(project.join("effects")).unwrap();
        fs::rename(project.join("actual-effects"), project.join("effects")).unwrap();
        fs::write(project.join("source.slang"), "source").unwrap();
        std::os::unix::fs::symlink("../source.slang", project.join("effects/custom.slang"))
            .unwrap();
        let error = prepare_shaders(&config, &project).err().unwrap();
        assert!(error.contains("symlink"), "{error}");
        fs::remove_file(project.join("effects/custom.slang")).unwrap();
        std::os::unix::fs::symlink("../source.slang", project.join("effects/custom.slang.wgsl"))
            .unwrap();
        let error = prepare_shaders(&config, &project).err().unwrap();
        assert!(error.contains("symlink"), "{error}");
        fs::remove_dir_all(project).unwrap();
    }
}
