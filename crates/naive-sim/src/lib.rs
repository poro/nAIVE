//! CPU simulation, asset decoding and presentation contracts shared by the
//! native engine and headless tests, without a GPU, a window or audio.
//!
//! This is the first slice of moving the simulation out of `naive-client`
//! (`docs/sim-split-plan.md`); `naive-client` re-exports these modules, so
//! code there keeps using `crate::physics`, `crate::input` and
//! `crate::project_path`.

pub use naive_core::{components, events, scene, transform};

pub mod animation;
pub mod assets;
pub mod input;
pub mod particles;
pub mod physics;
pub mod presentation;
pub mod project_path;
