//! naive.yaml project configuration parsing.
//!
//! Reads a game project's `naive.yaml` and converts it to `CliArgs`
//! via the bridge pattern — zero changes to engine.rs required.

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::cli::CliArgs;

#[derive(Debug, Deserialize)]
pub struct NaiveConfig {
    pub name: String,
    pub version: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    pub default_scene: Option<String>,
    pub default_pipeline: Option<String>,
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub dev_log: DevLogConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct TestConfig {
    pub files: Option<Vec<String>>,
    pub directory: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct BuildConfig {
    pub targets: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DevLogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub submit_on_complete: bool,
}

fn default_engine() -> String {
    "naive-runtime".to_string()
}

#[derive(Debug)]
pub enum ConfigError {
    NotFound,
    Io(std::io::Error),
    Parse(serde_norway::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NotFound => write!(f, "naive.yaml not found"),
            ConfigError::Io(e) => write!(f, "IO error reading naive.yaml: {}", e),
            ConfigError::Parse(e) => write!(f, "Failed to parse naive.yaml: {}", e),
        }
    }
}

/// Walk up from `start_dir` looking for `naive.yaml`.
pub fn find_config(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("naive.yaml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load and parse a `naive.yaml` file.
pub fn load_config(path: &Path) -> Result<NaiveConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    let config: NaiveConfig = serde_norway::from_str(&contents).map_err(ConfigError::Parse)?;
    Ok(config)
}

/// Convert a NaiveConfig into CliArgs for the engine (bridge pattern).
/// `socket` is the command socket path from the command line.
pub fn to_cli_args(config: &NaiveConfig, project_root: &Path, socket: &str) -> CliArgs {
    CliArgs {
        command: None,
        scene: config.default_scene.clone(),
        pipeline: config.default_pipeline.clone(),
        project: project_root.to_string_lossy().to_string(),
        socket: socket.to_string(),
        hud: false,
        editor_mode: false,
    }
}

/// Discover test files from config or by scanning the tests/ directory.
pub fn discover_test_files(
    config: &NaiveConfig,
    project_root: &Path,
) -> Result<Vec<PathBuf>, String> {
    let canonical_root = project_root.canonicalize().map_err(|e| e.to_string())?;
    // Explicit file list takes priority
    if let Some(files) = &config.test.files {
        if files.is_empty() {
            return Err("Configured test file list is empty".into());
        }
        return files
            .iter()
            .map(|f| {
                let path = crate::project_path::resolve(project_root, f)?;
                if !path.is_file() {
                    return Err(format!(
                        "Configured test file not found: {}",
                        path.display()
                    ));
                }
                Ok(path)
            })
            .collect();
    }

    // Scan directory (default: "tests")
    let test_dir = crate::project_path::resolve(
        project_root,
        config.test.directory.as_deref().unwrap_or("tests"),
    )?;

    if !test_dir.is_dir() {
        return Err(format!("Test directory not found: {}", test_dir.display()));
    }

    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(&test_dir).map_err(|e| format!("Cannot read test directory: {e}"))?
    {
        let path = entry
            .map_err(|e| format!("Cannot inspect test directory: {e}"))?
            .path();
        if path.is_file() && {
            let p = &path;
            p.extension().is_some_and(|ext| ext == "lua")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("test_"))
        } {
            let relative = path
                .strip_prefix(&canonical_root)
                .map_err(|e| e.to_string())?;
            files.push(crate::project_path::resolve(project_root, relative)?);
        }
    }

    files.sort();
    if files.is_empty() {
        return Err(format!(
            "No test_*.lua files found in {}",
            test_dir.display()
        ));
    }
    Ok(files)
}
