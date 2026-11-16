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

    let shaders = precompile_shaders(&project_root.join("shaders"), &game_dir.join("shaders"))?;
    if shaders.found > 0 {
        if cfg!(feature = "slang") {
            println!(
                "  Slang shaders compiled to WGSL: {} ({} imported modules skipped)",
                shaders.compiled, shaders.modules
            );
        } else {
            println!(
                "  Note: this naive was built without Slang, so {} Slang shader(s) were not \
                 compiled; the bundle uses the engine's built-in WGSL for those passes.",
                shaders.found
            );
        }
        for failure in &shaders.failed {
            println!("  Warning: {failure} (the bundle will use built-in WGSL for it)");
        }
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

#[derive(Default)]
struct ShaderReport {
    found: usize,
    compiled: usize,
    modules: usize,
    failed: Vec<String>,
}

/// Compile every Slang shader under `source` and write the WGSL into `dest`
/// at the same relative path, as `<name>.slang.wgsl`. Files with no entry
/// points are imported modules: they are compiled as part of the shaders that
/// import them.
fn precompile_shaders(source: &Path, dest: &Path) -> Result<ShaderReport, String> {
    let mut report = ShaderReport::default();
    if !source.is_dir() {
        return Ok(report);
    }
    let mut slang_files = Vec::new();
    collect_files(source, "slang", &mut slang_files)?;
    slang_files.sort();
    for slang in slang_files {
        report.found += 1;
        if !cfg!(feature = "slang") {
            continue;
        }
        let relative = slang.strip_prefix(source).map_err(|e| e.to_string())?;
        match crate::shader::compile_slang_to_wgsl_public(&slang) {
            Ok(wgsl) => {
                let out = crate::shader::precompiled_wgsl_path(&dest.join(relative));
                fs::write(&out, wgsl)
                    .map_err(|e| format!("Failed to write {}: {}", out.display(), e))?;
                report.compiled += 1;
            }
            Err(e) if e.to_string().contains("entry point") => report.modules += 1,
            Err(e) => report.failed.push(format!("{}: {}", relative.display(), e)),
        }
    }
    Ok(report)
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
        let config: NaiveConfig = serde_yaml::from_str("name: demo\nversion: '1'\n").unwrap();
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
        let config: NaiveConfig = serde_yaml::from_str("name: demo\nversion: \"0.1.0\"\n").unwrap();
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
            serde_yaml::from_str(&fs::read_to_string(project.join("naive.yaml")).unwrap()).unwrap();
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

    #[cfg(feature = "slang")]
    #[test]
    fn slang_shaders_are_compiled_to_wgsl_in_the_bundle() {
        let repo_shaders = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../project/shaders");
        let project = temp_project("shaders");
        let source = project.join("shaders");
        fs::create_dir_all(source.join("passes")).unwrap();
        fs::create_dir_all(source.join("modules")).unwrap();
        fs::copy(
            repo_shaders.join("passes/tonemap.slang"),
            source.join("passes/tonemap.slang"),
        )
        .unwrap();
        fs::copy(
            repo_shaders.join("modules/camera.slang"),
            source.join("modules/camera.slang"),
        )
        .unwrap();
        let dest = project.join("out");
        copy_dir_recursive(&source, &dest).unwrap();
        let report = precompile_shaders(&source, &dest).unwrap();
        assert_eq!(
            (report.found, report.compiled),
            (2, 1),
            "{:?}",
            report.failed
        );
        assert_eq!(report.modules, 1);
        let wgsl = fs::read_to_string(dest.join("passes/tonemap.slang.wgsl")).unwrap();
        naga::front::wgsl::parse_str(&wgsl).expect("precompiled WGSL parses");
        fs::remove_dir_all(project).unwrap();
    }
}
