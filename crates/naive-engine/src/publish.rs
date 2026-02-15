//! `naive publish` — publish to the nAIVE world server (stub).

use crate::project_config::NaiveConfig;

pub fn publish_project(config: &NaiveConfig) -> Result<(), String> {
    println!();
    println!("  nAIVE World Server — Coming Soon");
    println!();
    println!("  Your game \"{}\" v{} is ready to publish.", config.name, config.version);
    println!();
    println!("  When the world server launches, your game will be accessible");
    println!("  at a unique four-word address:");
    println!();
    println!("    bright.crystal.forest.realm");
    println!();
    println!("  Players will connect directly — no downloads, no installs.");
    println!("  The nAIVE runtime streams your world to any device.");
    println!();
    println!("  Follow progress: https://github.com/anthropics/naive");
    println!();

    Err("World server publishing is not yet available".to_string())
}
