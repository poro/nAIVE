mod audio;
mod audio_gen;
mod camera;
mod cli;
mod command;
mod components;
mod events;
mod engine;
mod font;
mod input;
mod material;
mod mesh;
mod physics;
mod pipeline;
mod reflect;
mod renderer;
mod scene;
mod scripting;
mod shader;
mod splat;
mod test_runner;
mod transform;
mod tween;
mod ui;
mod watcher;
mod world;

use clap::Parser;
use cli::CliArgs;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = CliArgs::parse();
    tracing::info!("nAIVE runtime v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Project root: {}", args.project);

    // Handle subcommands
    if let Some(cli::Command::Test { test_file }) = &args.command {
        let project_root = std::path::Path::new(&args.project);
        let test_path = project_root.join(test_file);
        if !test_path.exists() {
            eprintln!("Test file not found: {}", test_path.display());
            std::process::exit(1);
        }

        let results = test_runner::run_test_file(project_root, &test_path);

        let total = results.len();
        let passed = results.iter().filter(|r| r.passed).count();
        let failed = total - passed;

        println!();
        if failed == 0 {
            println!("All {} tests passed.", total);
            std::process::exit(0);
        } else {
            println!("{} passed, {} failed.", passed, failed);
            std::process::exit(1);
        }
    }

    // Default: run the windowed engine
    let event_loop =
        winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut engine = engine::Engine::new(args);

    event_loop
        .run_app(&mut engine)
        .expect("Event loop error");
}
