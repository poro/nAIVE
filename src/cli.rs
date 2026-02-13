use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "naive-runtime", version, about = "nAIVE - The AI-Native Game Engine")]
pub struct CliArgs {
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
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum OutputMode {
    Window,
    Headless,
}
