//! `naive-player`: the `naive` program without the Slang shader compiler.
//!
//! `naive build` ships this binary in game bundles. It loads the WGSL that
//! `naive build` compiled from the game's Slang shaders, so it needs no SDK
//! libraries, and with no arguments it runs the game packaged beside it.

fn main() {
    naive_runtime::run();
}
