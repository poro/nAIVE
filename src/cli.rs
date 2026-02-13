use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "naive-runtime", version, about = "nAIVE - The AI-Native Game Engine")]
pub struct CliArgs {
    /// Subcommand (test, etc.)
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to the scene YAML file
    #[arg(long)]
    pub scene: Option<String>,

    /// Path to the render pipeline YAML file
    #[arg(long)]
    pub pipeline: Option<String>,

    /// Output mode: window or headless
    #[arg(long, default_value = "window")]
    pub output: OutputMode,

    /// Path to the game project root directory
    #[arg(long, default_value = "project")]
    pub project: String,

    /// Path to the command socket for external control
    #[arg(long, default_value = "/tmp/naive-runtime.sock")]
    pub socket: String,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run automated Lua test scripts
    Test {
        /// Path to the test Lua file (relative to project root)
        test_file: String,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum OutputMode {
    Window,
    Headless,
}
