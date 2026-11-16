//! The parts of nAIVE that run without a GPU, a window or audio: what
//! headless tests, a server and the web build can share with the engine.
//!
//! This is the first slice of moving the simulation out of `naive-client`
//! (`docs/sim-split-plan.md`); `naive-client` re-exports these modules, so
//! code there keeps using `crate::physics` and `crate::project_path`.

pub use naive_core::{components, events, scene, transform};

pub mod physics;
pub mod project_path;
