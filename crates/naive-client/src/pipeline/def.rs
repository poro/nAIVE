use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Pipeline YAML serde types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PipelineFile {
    pub version: u32,
    #[serde(default)]
    pub settings: PipelineSettings,
    #[serde(default)]
    pub resources: Vec<ResourceDef>,
    pub passes: Vec<PassDef>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PipelineSettings {
    #[serde(default = "default_resolution")]
    pub resolution: [u32; 2],
    #[serde(default = "default_true")]
    pub vsync: bool,
    #[serde(default = "default_60")]
    pub max_fps: u32,
    #[serde(default)]
    pub hdr: bool,
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            resolution: default_resolution(),
            vsync: true,
            max_fps: 60,
            hdr: false,
        }
    }
}

fn default_resolution() -> [u32; 2] {
    [1280, 720]
}
fn default_true() -> bool {
    true
}
fn default_60() -> u32 {
    60
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ResourceDef {
    pub name: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    pub format: String,
    #[serde(default = "default_viewport")]
    pub size: String,
}

fn default_viewport() -> String {
    "viewport".to_string()
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PassDef {
    pub name: String,
    #[serde(rename = "type")]
    pub pass_type: String,
    pub shader: String,
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    #[serde(default)]
    pub outputs: HashMap<String, String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub cull: Option<String>,
    #[serde(default)]
    pub dispatch: Option<String>,
}

// ---------------------------------------------------------------------------
// Pipeline error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PipelineError {
    IoError(std::io::Error),
    ParseError(serde_yaml::Error),
    DagCycle(String),
    InvalidFormat(String),
    ShaderError(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "Pipeline IO error: {}", e),
            Self::ParseError(e) => write!(f, "Pipeline parse error: {}", e),
            Self::DagCycle(msg) => write!(f, "Pipeline DAG cycle: {}", msg),
            Self::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
            Self::ShaderError(msg) => write!(f, "Shader error: {}", msg),
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline YAML loading
// ---------------------------------------------------------------------------

pub fn load_pipeline(path: &Path) -> Result<PipelineFile, PipelineError> {
    let contents = std::fs::read_to_string(path).map_err(PipelineError::IoError)?;
    let pipeline: PipelineFile =
        serde_yaml::from_str(&contents).map_err(PipelineError::ParseError)?;
    tracing::info!(
        "Loaded pipeline v{} with {} passes and {} resources",
        pipeline.version,
        pipeline.passes.len(),
        pipeline.resources.len()
    );
    Ok(pipeline)
}

// ---------------------------------------------------------------------------
// DAG builder -- topological sort via Kahn's algorithm
// ---------------------------------------------------------------------------

/// Build an execution order for the passes using topological sort.
///
/// Dependencies are inferred from outputs -> inputs: if pass B lists an input
/// whose value matches the name of a resource that pass A writes to, then
/// A must execute before B. Special values "auto" and "swapchain" are not
/// considered resources produced by other passes.
pub fn build_dag(passes: &[PassDef]) -> Result<Vec<usize>, PipelineError> {
    let n = passes.len();

    // Map: resource_name -> index of the pass that produces it
    let mut producer: HashMap<&str, usize> = HashMap::new();
    for (i, pass) in passes.iter().enumerate() {
        for resource_name in pass.outputs.values() {
            if resource_name != "swapchain" {
                producer.insert(resource_name.as_str(), i);
            }
        }
    }

    // Build adjacency list and in-degree counts
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, pass) in passes.iter().enumerate() {
        for input_resource in pass.inputs.values() {
            if input_resource == "auto" {
                continue;
            }
            if let Some(&producer_idx) = producer.get(input_resource.as_str()) {
                if producer_idx != i {
                    adj[producer_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }
    }

    // Kahn's algorithm
    let mut queue: Vec<usize> = Vec::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push(i);
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(n);
    while let Some(node) = queue.pop() {
        order.push(node);
        for &neighbor in &adj[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push(neighbor);
            }
        }
    }

    if order.len() != n {
        return Err(PipelineError::DagCycle(
            "Cycle detected in render pass dependencies".to_string(),
        ));
    }

    Ok(order)
}
