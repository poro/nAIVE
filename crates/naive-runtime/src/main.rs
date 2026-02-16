use clap::Parser;
use naive_client::cli::CliArgs;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = CliArgs::parse();
    tracing::info!("nAIVE runtime v{}", env!("CARGO_PKG_VERSION"));

    match &args.command {
        // naive init <name>
        Some(naive_client::cli::Command::Init { name }) => {
            if let Err(e) = naive_client::init::create_project(name) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
            return;
        }

        // naive run [--scene X]
        Some(naive_client::cli::Command::Run { scene }) => {
            let hud = args.hud;
            let cwd = std::env::current_dir().expect("Failed to get current directory");
            let args = match naive_client::project_config::find_config(&cwd) {
                Some(config_path) => {
                    let project_root = config_path.parent().unwrap();
                    let config = match naive_client::project_config::load_config(&config_path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            std::process::exit(1);
                        }
                    };
                    tracing::info!("Loaded project: {} v{}", config.name, config.version);
                    let mut cli_args = naive_client::project_config::to_cli_args(&config, project_root);
                    // CLI scene override takes priority
                    if scene.is_some() {
                        cli_args.scene = scene.clone();
                    }
                    cli_args.hud = hud;
                    cli_args
                }
                None => {
                    eprintln!("Error: No naive.yaml found in current directory or parents.");
                    eprintln!("  Run `naive init <name>` to create a new project,");
                    eprintln!("  or use `naive-runtime --project <path> --scene <scene>` for legacy mode.");
                    std::process::exit(1);
                }
            };
            run_engine(args);
            return;
        }

        // naive test [test_file]
        Some(naive_client::cli::Command::Test { test_file }) => {
            match test_file {
                Some(file) => {
                    let cwd = std::env::current_dir().expect("Failed to get current directory");
                    let project_root = match naive_client::project_config::find_config(&cwd) {
                        Some(config_path) => config_path.parent().unwrap().to_path_buf(),
                        None => std::path::PathBuf::from(&args.project),
                    };
                    let test_path = project_root.join(file);
                    run_single_test(&project_root, &test_path);
                }
                None => {
                    let cwd = std::env::current_dir().expect("Failed to get current directory");
                    let config_path = match naive_client::project_config::find_config(&cwd) {
                        Some(p) => p,
                        None => {
                            eprintln!("Error: No naive.yaml found. Specify a test file or run from a project directory.");
                            std::process::exit(1);
                        }
                    };
                    let project_root = config_path.parent().unwrap();
                    let config = match naive_client::project_config::load_config(&config_path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            std::process::exit(1);
                        }
                    };
                    let test_files = naive_client::project_config::discover_test_files(&config, project_root);
                    if test_files.is_empty() {
                        println!("No test files found.");
                        return;
                    }
                    println!("Running {} test file(s)...\n", test_files.len());
                    let mut total_passed = 0;
                    let mut total_failed = 0;
                    for test_path in &test_files {
                        println!("--- {} ---", test_path.display());
                        let results = naive_client::test_runner::run_test_file(project_root, test_path);
                        let passed = results.iter().filter(|r| r.passed).count();
                        let failed = results.len() - passed;
                        total_passed += passed;
                        total_failed += failed;
                        println!();
                    }
                    println!("{} passed, {} failed across {} files.",
                        total_passed, total_failed, test_files.len());
                    if total_failed > 0 {
                        std::process::exit(1);
                    }
                }
            }
            return;
        }

        // naive build [--target X]
        Some(naive_client::cli::Command::Build { target }) => {
            let cwd = std::env::current_dir().expect("Failed to get current directory");
            let config_path = match naive_client::project_config::find_config(&cwd) {
                Some(p) => p,
                None => {
                    eprintln!("Error: No naive.yaml found. Run from a project directory.");
                    std::process::exit(1);
                }
            };
            let project_root = config_path.parent().unwrap();
            let config = match naive_client::project_config::load_config(&config_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };
            if let Err(e) = naive_client::build::bundle_project(&config, project_root, target.as_deref()) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
            return;
        }

        // naive publish
        Some(naive_client::cli::Command::Publish) => {
            let cwd = std::env::current_dir().expect("Failed to get current directory");
            let config_path = match naive_client::project_config::find_config(&cwd) {
                Some(p) => p,
                None => {
                    eprintln!("Error: No naive.yaml found. Run from a project directory.");
                    std::process::exit(1);
                }
            };
            let config = match naive_client::project_config::load_config(&config_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };
            if let Err(e) = naive_client::publish::publish_project(&config) {
                eprintln!("Note: {}", e);
                std::process::exit(1);
            }
            return;
        }

        // No subcommand: auto-detect or legacy mode
        None => {
            let cwd = std::env::current_dir().expect("Failed to get current directory");

            if let Some(config_path) = naive_client::project_config::find_config(&cwd) {
                if args.scene.is_none() && args.project == "project" {
                    let project_root = config_path.parent().unwrap();
                    let config = match naive_client::project_config::load_config(&config_path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("Warning: Found naive.yaml but failed to parse: {}", e);
                            eprintln!("Falling back to legacy mode.");
                            tracing::info!("Project root: {}", args.project);
                            run_engine(args);
                            return;
                        }
                    };
                    tracing::info!("Auto-detected project: {} v{}", config.name, config.version);
                    let mut cli_args = naive_client::project_config::to_cli_args(&config, project_root);
                    cli_args.hud = args.hud;
                    run_engine(cli_args);
                    return;
                }
            }

            tracing::info!("Project root: {}", args.project);
            run_engine(args);
        }
    }
}

fn run_single_test(project_root: &std::path::Path, test_path: &std::path::Path) {
    if !test_path.exists() {
        eprintln!("Test file not found: {}", test_path.display());
        std::process::exit(1);
    }

    let results = naive_client::test_runner::run_test_file(project_root, test_path);

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

fn run_engine(args: CliArgs) {
    let event_loop =
        winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut engine = naive_client::engine::Engine::new(args);

    event_loop
        .run_app(&mut engine)
        .expect("Event loop error");
}
