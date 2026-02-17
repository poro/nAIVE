//! `naive demo` / `naive demos` — browse and run built-in engine demos.
//!
//! All demo content (scenes, Lua scripts, materials) is embedded in the binary
//! via `include_str!`. Running a demo extracts files to a temp directory and
//! launches the engine.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::{CliArgs, OutputMode};

// ---------------------------------------------------------------------------
// Shared infrastructure (embedded)
// ---------------------------------------------------------------------------

const PIPELINE_YAML: &str = include_str!("../../../project/pipelines/render.yaml");
const INPUT_BINDINGS: &str = include_str!("../../../project/input/bindings.yaml");

// ---------------------------------------------------------------------------
// Material pool — every referenced material, keyed by filename
// ---------------------------------------------------------------------------

static MATERIALS: &[(&str, &str)] = &[
    ("default.yaml", include_str!("../../../project/assets/materials/default.yaml")),
    ("blue.yaml", include_str!("../../../project/assets/materials/blue.yaml")),
    ("red.yaml", include_str!("../../../project/assets/materials/red.yaml")),
    ("gold.yaml", include_str!("../../../project/assets/materials/gold.yaml")),
    ("stone.yaml", include_str!("../../../project/assets/materials/stone.yaml")),
    ("wood.yaml", include_str!("../../../project/assets/materials/wood.yaml")),
    ("chrome.yaml", include_str!("../../../project/assets/materials/chrome.yaml")),
    ("obsidian.yaml", include_str!("../../../project/assets/materials/obsidian.yaml")),
    ("dark_floor.yaml", include_str!("../../../project/assets/materials/dark_floor.yaml")),
    ("dark_mirror.yaml", include_str!("../../../project/assets/materials/dark_mirror.yaml")),
    ("enemy.yaml", include_str!("../../../project/assets/materials/enemy.yaml")),
    ("spike.yaml", include_str!("../../../project/assets/materials/spike.yaml")),
    ("goblin.yaml", include_str!("../../../project/assets/materials/goblin.yaml")),
    ("bullet.yaml", include_str!("../../../project/assets/materials/bullet.yaml")),
    ("copper_ring.yaml", include_str!("../../../project/assets/materials/copper_ring.yaml")),
    ("steel_ring.yaml", include_str!("../../../project/assets/materials/steel_ring.yaml")),
    ("genesis_core.yaml", include_str!("../../../project/assets/materials/genesis_core.yaml")),
    // Neon
    ("neon_amber.yaml", include_str!("../../../project/assets/materials/neon_amber.yaml")),
    ("neon_blue.yaml", include_str!("../../../project/assets/materials/neon_blue.yaml")),
    ("neon_cyan.yaml", include_str!("../../../project/assets/materials/neon_cyan.yaml")),
    ("neon_gold.yaml", include_str!("../../../project/assets/materials/neon_gold.yaml")),
    ("neon_green.yaml", include_str!("../../../project/assets/materials/neon_green.yaml")),
    ("neon_pink.yaml", include_str!("../../../project/assets/materials/neon_pink.yaml")),
    ("neon_purple.yaml", include_str!("../../../project/assets/materials/neon_purple.yaml")),
    ("neon_white.yaml", include_str!("../../../project/assets/materials/neon_white.yaml")),
    // Inferno
    ("inferno_floor.yaml", include_str!("../../../project/assets/materials/inferno_floor.yaml")),
    ("inferno_wall.yaml", include_str!("../../../project/assets/materials/inferno_wall.yaml")),
    ("inferno_lava.yaml", include_str!("../../../project/assets/materials/inferno_lava.yaml")),
    ("inferno_iron.yaml", include_str!("../../../project/assets/materials/inferno_iron.yaml")),
    ("inferno_demon.yaml", include_str!("../../../project/assets/materials/inferno_demon.yaml")),
    ("inferno_health.yaml", include_str!("../../../project/assets/materials/inferno_health.yaml")),
    // Titan
    ("titan_floor.yaml", include_str!("../../../project/assets/materials/titan_floor.yaml")),
    ("titan_gold.yaml", include_str!("../../../project/assets/materials/titan_gold.yaml")),
    ("titan_pillar.yaml", include_str!("../../../project/assets/materials/titan_pillar.yaml")),
    ("titan_platinum.yaml", include_str!("../../../project/assets/materials/titan_platinum.yaml")),
    ("titan_ruby.yaml", include_str!("../../../project/assets/materials/titan_ruby.yaml")),
    ("titan_sapphire.yaml", include_str!("../../../project/assets/materials/titan_sapphire.yaml")),
    ("titan_emerald.yaml", include_str!("../../../project/assets/materials/titan_emerald.yaml")),
    ("titan_ice.yaml", include_str!("../../../project/assets/materials/titan_ice.yaml")),
    ("titan_ember.yaml", include_str!("../../../project/assets/materials/titan_ember.yaml")),
    ("titan_void.yaml", include_str!("../../../project/assets/materials/titan_void.yaml")),
    // PBR gallery
    ("pbr_M0_R0.yaml", include_str!("../../../project/assets/materials/pbr_M0_R0.yaml")),
    ("pbr_M0_R2.yaml", include_str!("../../../project/assets/materials/pbr_M0_R2.yaml")),
    ("pbr_M0_R4.yaml", include_str!("../../../project/assets/materials/pbr_M0_R4.yaml")),
    ("pbr_M1_R1.yaml", include_str!("../../../project/assets/materials/pbr_M1_R1.yaml")),
    ("pbr_M1_R3.yaml", include_str!("../../../project/assets/materials/pbr_M1_R3.yaml")),
    ("pbr_M1_R4.yaml", include_str!("../../../project/assets/materials/pbr_M1_R4.yaml")),
    ("pbr_M2_R0.yaml", include_str!("../../../project/assets/materials/pbr_M2_R0.yaml")),
    ("pbr_M2_R2.yaml", include_str!("../../../project/assets/materials/pbr_M2_R2.yaml")),
    ("pbr_M2_R4.yaml", include_str!("../../../project/assets/materials/pbr_M2_R4.yaml")),
    ("pbr_M3_R1.yaml", include_str!("../../../project/assets/materials/pbr_M3_R1.yaml")),
    ("pbr_M3_R2.yaml", include_str!("../../../project/assets/materials/pbr_M3_R2.yaml")),
    ("pbr_M3_R3.yaml", include_str!("../../../project/assets/materials/pbr_M3_R3.yaml")),
    ("pbr_M4_R0.yaml", include_str!("../../../project/assets/materials/pbr_M4_R0.yaml")),
    ("pbr_M4_R2.yaml", include_str!("../../../project/assets/materials/pbr_M4_R2.yaml")),
    ("pbr_M4_R4.yaml", include_str!("../../../project/assets/materials/pbr_M4_R4.yaml")),
];

// ---------------------------------------------------------------------------
// Demo registry
// ---------------------------------------------------------------------------

struct DemoEntry {
    number: usize,
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    scene_filename: &'static str,
    scene_content: &'static str,
    scripts: &'static [(&'static str, &'static str)],
}

static DEMOS: &[DemoEntry] = &[
    // -- Physics --
    DemoEntry {
        number: 1,
        slug: "impulse",
        name: "Impulse & Force Demo",
        description: "Launch spheres with impulse and force physics",
        category: "Physics",
        scene_filename: "tier25_impulse_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier25_impulse_demo.yaml"),
        scripts: &[
            ("tier25_impulse_demo.lua", include_str!("../../../project/logic/tier25_impulse_demo.lua")),
        ],
    },
    DemoEntry {
        number: 2,
        slug: "ccd",
        name: "Continuous Collision Detection",
        description: "Fast projectiles vs thin walls",
        category: "Physics",
        scene_filename: "tier25_ccd_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier25_ccd_demo.yaml"),
        scripts: &[
            ("tier25_ccd_demo.lua", include_str!("../../../project/logic/tier25_ccd_demo.lua")),
        ],
    },
    DemoEntry {
        number: 3,
        slug: "materials",
        name: "Material Properties Demo",
        description: "Restitution & friction hierarchy",
        category: "Physics",
        scene_filename: "tier25_materials_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier25_materials_demo.yaml"),
        scripts: &[
            ("tier25_materials_demo.lua", include_str!("../../../project/logic/tier25_materials_demo.lua")),
        ],
    },
    // -- Combat --
    DemoEntry {
        number: 4,
        slug: "combat",
        name: "Combat Demo",
        description: "Health, hitscan, projectiles, collision damage",
        category: "Combat",
        scene_filename: "combat_demo.yaml",
        scene_content: include_str!("../../../project/scenes/combat_demo.yaml"),
        scripts: &[
            ("combat_demo.lua", include_str!("../../../project/logic/combat_demo.lua")),
            ("combat_enemy.lua", include_str!("../../../project/logic/combat_enemy.lua")),
        ],
    },
    DemoEntry {
        number: 5,
        slug: "third-person",
        name: "Third-Person Camera",
        description: "Orbit camera with wall collision",
        category: "Combat",
        scene_filename: "third_person_demo.yaml",
        scene_content: include_str!("../../../project/scenes/third_person_demo.yaml"),
        scripts: &[
            ("third_person_demo.lua", include_str!("../../../project/logic/third_person_demo.lua")),
            ("combat_enemy.lua", include_str!("../../../project/logic/combat_enemy.lua")),
        ],
    },
    // -- Visual --
    DemoEntry {
        number: 6,
        slug: "particles",
        name: "Particle System Showcase",
        description: "5 emitter types with burst control",
        category: "Visual",
        scene_filename: "tier2_particle_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier2_particle_demo.yaml"),
        scripts: &[
            ("tier2_particle_camera.lua", include_str!("../../../project/logic/tier2_particle_camera.lua")),
        ],
    },
    DemoEntry {
        number: 7,
        slug: "pbr",
        name: "PBR Material Gallery",
        description: "Metallic/roughness grid with physics",
        category: "Visual",
        scene_filename: "pbr_gallery.yaml",
        scene_content: include_str!("../../../project/scenes/pbr_gallery.yaml"),
        scripts: &[
            ("ball_collision.lua", include_str!("../../../project/logic/ball_collision.lua")),
        ],
    },
    DemoEntry {
        number: 8,
        slug: "shake",
        name: "Camera Shake Demo",
        description: "Impact and explosion screen shake",
        category: "Visual",
        scene_filename: "tier25_shake_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier25_shake_demo.yaml"),
        scripts: &[
            ("tier25_shake_demo.lua", include_str!("../../../project/logic/tier25_shake_demo.lua")),
        ],
    },
    // -- UI --
    DemoEntry {
        number: 9,
        slug: "ui",
        name: "UI Overlay System",
        description: "Text, rects, flash, progress bars",
        category: "UI",
        scene_filename: "ui_demo.yaml",
        scene_content: include_str!("../../../project/scenes/ui_demo.yaml"),
        scripts: &[
            ("ui_demo.lua", include_str!("../../../project/logic/ui_demo.lua")),
            ("ui_demo_camera.lua", include_str!("../../../project/logic/ui_demo_camera.lua")),
        ],
    },
    // -- Worlds --
    DemoEntry {
        number: 10,
        slug: "genesis",
        name: "The Birth of a World",
        description: "Procedural creation with orbital rings",
        category: "Worlds",
        scene_filename: "genesis.yaml",
        scene_content: include_str!("../../../project/scenes/genesis.yaml"),
        scripts: &[
            ("genesis_camera.lua", include_str!("../../../project/logic/genesis_camera.lua")),
            ("genesis_spark.lua", include_str!("../../../project/logic/genesis_spark.lua")),
            ("genesis_orbit.lua", include_str!("../../../project/logic/genesis_orbit.lua")),
            ("genesis_pillar_rise.lua", include_str!("../../../project/logic/genesis_pillar_rise.lua")),
            ("genesis_roughness_sweep.lua", include_str!("../../../project/logic/genesis_roughness_sweep.lua")),
            ("genesis_emissive_cycle.lua", include_str!("../../../project/logic/genesis_emissive_cycle.lua")),
            ("neon_pulse.lua", include_str!("../../../project/logic/neon_pulse.lua")),
            ("genesis_sunrise.lua", include_str!("../../../project/logic/genesis_sunrise.lua")),
        ],
    },
    DemoEntry {
        number: 11,
        slug: "inferno",
        name: "Combat Arena",
        description: "Fiery battleground with enemies",
        category: "Worlds",
        scene_filename: "inferno.yaml",
        scene_content: include_str!("../../../project/scenes/inferno.yaml"),
        scripts: &[
            ("inferno_player.lua", include_str!("../../../project/logic/inferno_player.lua")),
            ("inferno_lava_glow.lua", include_str!("../../../project/logic/inferno_lava_glow.lua")),
            ("inferno_enemy.lua", include_str!("../../../project/logic/inferno_enemy.lua")),
            ("float_bob.lua", include_str!("../../../project/logic/float_bob.lua")),
            ("torch_flicker.lua", include_str!("../../../project/logic/torch_flicker.lua")),
            ("neon_pulse.lua", include_str!("../../../project/logic/neon_pulse.lua")),
        ],
    },
    DemoEntry {
        number: 12,
        slug: "titan",
        name: "Colosseum of Light",
        description: "Architectural showcase with dynamic lighting",
        category: "Worlds",
        scene_filename: "titan.yaml",
        scene_content: include_str!("../../../project/scenes/titan.yaml"),
        scripts: &[
            ("titan_camera.lua", include_str!("../../../project/logic/titan_camera.lua")),
            ("titan_levitate.lua", include_str!("../../../project/logic/titan_levitate.lua")),
            ("titan_ring_orbit.lua", include_str!("../../../project/logic/titan_ring_orbit.lua")),
            ("titan_emission_wave.lua", include_str!("../../../project/logic/titan_emission_wave.lua")),
            ("titan_pulse_light.lua", include_str!("../../../project/logic/titan_pulse_light.lua")),
            ("genesis_orbit.lua", include_str!("../../../project/logic/genesis_orbit.lua")),
            ("neon_pulse.lua", include_str!("../../../project/logic/neon_pulse.lua")),
        ],
    },
    // -- Stress Test --
    DemoEntry {
        number: 13,
        slug: "stress",
        name: "Stress Test",
        description: "300+ dynamic entities",
        category: "Stress Test",
        scene_filename: "tier2_stress_test.yaml",
        scene_content: include_str!("../../../project/scenes/tier2_stress_test.yaml"),
        scripts: &[
            ("tier2_stress_camera.lua", include_str!("../../../project/logic/tier2_stress_camera.lua")),
            ("tier2_stress_spawner.lua", include_str!("../../../project/logic/tier2_stress_spawner.lua")),
        ],
    },
    DemoEntry {
        number: 14,
        slug: "lifecycle",
        name: "Entity Lifecycle",
        description: "Spawn, destroy, pool, query, events",
        category: "Stress Test",
        scene_filename: "tier2_lifecycle_demo.yaml",
        scene_content: include_str!("../../../project/scenes/tier2_lifecycle_demo.yaml"),
        scripts: &[
            ("tier2_lifecycle_player.lua", include_str!("../../../project/logic/tier2_lifecycle_player.lua")),
            ("tier2_pickup_bob.lua", include_str!("../../../project/logic/tier2_pickup_bob.lua")),
        ],
    },
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run a demo by selector (number or name), or show interactive menu if None.
/// Returns Some(CliArgs) to launch the engine, or None if the user quit.
pub fn run_demo(selector: Option<&str>) -> Option<CliArgs> {
    let demo = match selector {
        Some(s) => find_demo(s)?,
        None => show_interactive_menu()?,
    };

    let project_root = extract_demo(demo);

    println!("  Launching: {} — {}", demo.name, demo.description);
    println!();

    Some(CliArgs {
        command: None,
        scene: Some(format!("scenes/{}", demo.scene_filename)),
        pipeline: Some("pipelines/render.yaml".to_string()),
        output: OutputMode::Window,
        project: project_root.to_string_lossy().to_string(),
        socket: "/tmp/naive-runtime.sock".to_string(),
        hud: false,
    })
}

// ---------------------------------------------------------------------------
// Demo lookup
// ---------------------------------------------------------------------------

fn find_demo(selector: &str) -> Option<&'static DemoEntry> {
    // Try as number first
    if let Ok(num) = selector.parse::<usize>() {
        let demo = DEMOS.iter().find(|d| d.number == num);
        if demo.is_none() {
            eprintln!("No demo #{num}. Use 1-{}.", DEMOS.len());
        }
        return demo;
    }

    // Exact slug match
    if let Some(demo) = DEMOS.iter().find(|d| d.slug.eq_ignore_ascii_case(selector)) {
        return Some(demo);
    }

    // Substring match on slug or name
    let lower = selector.to_ascii_lowercase();
    let matches: Vec<&DemoEntry> = DEMOS
        .iter()
        .filter(|d| {
            d.slug.to_ascii_lowercase().contains(&lower)
                || d.name.to_ascii_lowercase().contains(&lower)
        })
        .collect();

    match matches.len() {
        0 => {
            eprintln!("No demo matching \"{selector}\". Run `naive demo` to see all demos.");
            None
        }
        1 => Some(matches[0]),
        _ => {
            eprintln!("Ambiguous \"{selector}\". Matches:");
            for m in &matches {
                eprintln!("  {:>2}  {:<14} {}", m.number, m.slug, m.name);
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive menu
// ---------------------------------------------------------------------------

fn show_interactive_menu() -> Option<&'static DemoEntry> {
    let version = env!("CARGO_PKG_VERSION");

    println!();
    println!("  \x1b[1mnAIVE Engine Demos\x1b[0m (v{})", version);

    let mut current_category = "";
    for demo in DEMOS.iter() {
        if demo.category != current_category {
            current_category = demo.category;
            println!();
            println!("  \x1b[1;36m{}\x1b[0m", current_category);
        }
        println!(
            "    \x1b[1;33m{:>2}\x1b[0m  \x1b[1m{:<14}\x1b[0m {}",
            demo.number, demo.slug, demo.description
        );
    }

    println!();
    print!("  Enter number or name (\x1b[2mq to quit\x1b[0m): ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return None;
    }

    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("q") || trimmed.eq_ignore_ascii_case("quit") {
        return None;
    }

    find_demo(trimmed)
}

// ---------------------------------------------------------------------------
// Extract demo to temp directory
// ---------------------------------------------------------------------------

fn extract_demo(demo: &DemoEntry) -> PathBuf {
    let temp_root = std::env::temp_dir().join("naive-demo");

    // Clean and recreate
    let _ = std::fs::remove_dir_all(&temp_root);
    create_dirs(&temp_root);

    // naive.yaml
    write_file(
        &temp_root.join("naive.yaml"),
        &format!(
            "name: \"nAIVE Demo — {}\"\nversion: \"0.0.0\"\nengine: \"naive-runtime\"\ndefault_scene: \"scenes/{}\"\ndefault_pipeline: \"pipelines/render.yaml\"\n",
            demo.name, demo.scene_filename
        ),
    );

    // Shared infrastructure
    write_file(&temp_root.join("pipelines/render.yaml"), PIPELINE_YAML);
    write_file(&temp_root.join("input/bindings.yaml"), INPUT_BINDINGS);

    // All materials
    for (filename, content) in MATERIALS {
        write_file(
            &temp_root.join(format!("assets/materials/{}", filename)),
            content,
        );
    }

    // Scene
    write_file(
        &temp_root.join(format!("scenes/{}", demo.scene_filename)),
        demo.scene_content,
    );

    // Scripts
    for (filename, content) in demo.scripts {
        write_file(&temp_root.join(format!("logic/{}", filename)), content);
    }

    temp_root
}

fn create_dirs(root: &Path) {
    let dirs = [
        "",
        "scenes",
        "logic",
        "assets/materials",
        "pipelines",
        "input",
    ];
    for dir in &dirs {
        let _ = std::fs::create_dir_all(root.join(dir));
    }
}

fn write_file(path: &Path, content: &str) {
    if let Err(e) = std::fs::write(path, content) {
        tracing::warn!("Failed to write {}: {}", path.display(), e);
    }
}
