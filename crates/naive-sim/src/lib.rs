//! CPU simulation, asset decoding and presentation contracts shared by the
//! native engine and headless tests, without a GPU, a window or audio device.
//!
//! Simulation modules move here from `naive-client` as their native adapters
//! are separated (`docs/sim-split-plan.md`). Client re-exports preserve existing
//! component, scene-state, physics, input and project-path imports.
//! Enable `scripting` for the sandboxed Lua runtime, shared lifecycle steps and
//! headless test runner. The default CPU library does not link a Lua VM.

pub use naive_core::{events, scene, transform, tween};

pub mod animation;
pub mod assets;
pub mod components;
pub mod input;
pub mod particles;
pub mod physics;
pub mod presentation;
pub mod project_path;
#[cfg(feature = "scripting")]
pub mod sandbox;
pub mod scene_snapshot;
#[cfg(feature = "scripting")]
pub mod scripting;
#[cfg(feature = "scripting")]
pub mod sim;
pub mod swarm;
pub mod swarm_api;
#[cfg(feature = "scripting")]
pub mod test_runner;
pub mod world;
