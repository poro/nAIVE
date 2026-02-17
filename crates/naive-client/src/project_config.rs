//! naive.yaml project configuration parsing.
//!
//! Reads a game project's `naive.yaml` and converts it to `CliArgs`
//! via the bridge pattern — zero changes to engine.rs required.

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::cli::{CliArgs, OutputMode};

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

#[derive(Debug, Deserialize)]
pub struct DevLogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub submit_on_complete: bool,
}

impl Default for DevLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            submit_on_complete: false,
        }
    }
}

fn default_engine() -> String {
    "naive-runtime".to_string()
}

#[derive(Debug)]
pub enum ConfigError {
    NotFound,
    Io(std::io::Error),
    Parse(serde_yaml::Error),
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
    let config: NaiveConfig = serde_yaml::from_str(&contents).map_err(ConfigError::Parse)?;
    Ok(config)
}

/// Convert a NaiveConfig into CliArgs for the engine (bridge pattern).
pub fn to_cli_args(config: &NaiveConfig, project_root: &Path) -> CliArgs {
    CliArgs {
        command: None,
        scene: config.default_scene.clone(),
        pipeline: config.default_pipeline.clone(),
        output: OutputMode::Window,
        project: project_root.to_string_lossy().to_string(),
        socket: "/tmp/naive-runtime.sock".to_string(),
        hud: false,
    }
}

/// Discover test files from config or by scanning the tests/ directory.
pub fn discover_test_files(config: &NaiveConfig, project_root: &Path) -> Vec<PathBuf> {
    // Explicit file list takes priority
    if let Some(files) = &config.test.files {
        return files
            .iter()
            .map(|f| project_root.join(f))
            .filter(|p| p.exists())
            .collect();
    }

    // Scan directory (default: "tests")
    let test_dir = project_root.join(
        config
            .test
            .directory
            .as_deref()
            .unwrap_or("tests"),
    );

    if !test_dir.is_dir() {
        return Vec::new();
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&test_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map_or(false, |ext| ext == "lua")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("test_"))
        })
        .collect();

    files.sort();
    files
}
