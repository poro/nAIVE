use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "naive", version, about = "nAIVE - The AI-Native Game Engine")]
pub struct CliArgs {
    /// Subcommand (init, run, test, build, publish)
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

    /// Show the render debug HUD on startup
    #[arg(long, global = true)]
    pub hud: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new nAIVE game project
    Init {
        /// Project name (becomes directory name)
        name: String,
    },
    /// Run the game (reads naive.yaml in current directory)
    Run {
        /// Override the scene to load
        #[arg(long)]
        scene: Option<String>,
    },
    /// Run automated Lua test scripts
    Test {
        /// Specific test file (or runs all from naive.yaml)
        test_file: Option<String>,
    },
    /// Bundle game for standalone distribution
    Build {
        /// Target platform (macos, windows, linux)
        #[arg(long)]
        target: Option<String>,
    },
    /// Publish to nAIVE world server
    Publish,
    /// Submit dev.log as a GitHub issue for engine feedback
    SubmitLog,
    /// Run a built-in engine demo
    Demo {
        /// Demo number or name (omit for interactive selection)
        selector: Option<String>,
    },
    /// List and run built-in engine demos
    Demos {
        /// Demo number or name (omit for interactive selection)
        selector: Option<String>,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum OutputMode {
    Window,
    Headless,
}
