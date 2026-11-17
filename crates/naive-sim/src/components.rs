//! GPU-free ECS components, including shared script authoring state.

use std::path::PathBuf;

pub use naive_core::components::*;

/// Script component attached to entities.
#[derive(Debug, Clone)]
pub struct Script {
    pub source: PathBuf,
    pub initialized: bool,
}
