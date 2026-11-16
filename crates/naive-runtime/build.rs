//! Adds the SLANG SDK's library directory to the runtime search path of the
//! `naive`, `naive-runtime` and `naive_mcp` binaries, so they can load libslang
//! when run from the build directory.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SLANG_DIR");

    if std::env::var_os("CARGO_FEATURE_SLANG").is_none() {
        return;
    }

    // A packager that passes its own -rpath, as the Homebrew formula does,
    // decides where the libraries live.
    let rustflags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    if rustflags.contains("-rpath") {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" && target_os != "linux" {
        return;
    }

    let Some(slang_dir) = std::env::var_os("SLANG_DIR") else {
        return;
    };
    let lib_dir = Path::new(&slang_dir).join("lib");
    // The search path must be absolute; .cargo/config.toml already makes
    // SLANG_DIR absolute, but it can also be set by hand.
    let lib_dir = std::fs::canonicalize(&lib_dir).unwrap_or(lib_dir);
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
}
