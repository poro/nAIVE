//! CPU simulation, asset decoding and presentation contracts shared by the
//! native engine and headless tests, without a GPU, a window or audio.
//!
//! Simulation modules move here from `naive-client` as their native adapters
//! are separated (`docs/sim-split-plan.md`). Client re-exports preserve existing
//! component, scene-state, physics, input and project-path imports.

pub use naive_core::{events, scene, transform};

pub mod animation;
pub mod assets;
pub mod components;
pub mod input;
pub mod particles;
pub mod physics;
pub mod presentation;
pub mod project_path;
pub mod scene_snapshot;
pub mod world;
