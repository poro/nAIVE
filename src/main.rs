mod camera;
mod cli;
mod components;
mod engine;
mod material;
mod mesh;
mod renderer;
mod scene;
mod shader;
mod transform;
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

    let event_loop =
        winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut engine = engine::Engine::new(args);

    event_loop
        .run_app(&mut engine)
        .expect("Event loop error");
}
