//! Native-compatible import paths for the shared CPU scripting runtime.
//!
//! Lua APIs and script lifecycle live in `naive-sim`; the native audio system
//! implements its renderer-independent audio backend contract.

pub use naive_sim::scripting::*;

/// Shared reference to the native audio backend used by engine integrations.
pub type SharedAudioSystem = std::rc::Rc<std::cell::RefCell<crate::audio::AudioSystem>>;

/// Shared reference to the native particle adapter, including its GPU vertices.
pub type SharedParticleSystem = std::rc::Rc<std::cell::RefCell<crate::particles::ParticleSystem>>;
