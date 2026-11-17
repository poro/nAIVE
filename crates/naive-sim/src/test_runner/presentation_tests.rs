use super::*;
use crate::presentation::UiIntent;

struct Project(PathBuf);
impl Project {
    fn new(script: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("naive-presentation-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let project = Self(root);
        project.write("game.lua", script);
        project
    }
    fn write(&self, path: &str, source: &str) {
        std::fs::write(self.0.join(path), source).unwrap();
    }

    /// A one-joint skinned triangle and a real 0.5-second walk clip, with an
    /// external buffer so the ordinary project-path and asset loader run.
    fn animated_mesh(&self) {
        let mut bytes = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bytes.extend(value.to_le_bytes());
        }
        for _ in 0..12 {
            bytes.extend(0_u16.to_le_bytes());
        }
        for _ in 0..3 {
            for value in [1.0_f32, 0.0, 0.0, 0.0] {
                bytes.extend(value.to_le_bytes());
            }
        }
        for value in [0.0_f32, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
            bytes.extend(value.to_le_bytes());
        }
        std::fs::write(self.0.join("animated.bin"), bytes).unwrap();
        self.write("animated.gltf", &serde_json::json!({
            "asset": {"version": "2.0"}, "scene": 0,
            "scenes": [{"nodes": [0,1]}],
            "nodes": [{"mesh": 0, "skin": 0}, {"name": "root"}],
            "skins": [{"joints": [1]}],
            "meshes": [{"primitives": [{"attributes": {"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2}}]}],
            "buffers": [{"uri":"animated.bin","byteLength":140}],
            "bufferViews": [
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":24},
                {"buffer":0,"byteOffset":60,"byteLength":48},
                {"buffer":0,"byteOffset":108,"byteLength":8},
                {"buffer":0,"byteOffset":116,"byteLength":24}
            ],
            "accessors": [
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5123,"count":3,"type":"VEC4"},
                {"bufferView":2,"componentType":5126,"count":3,"type":"VEC4"},
                {"bufferView":3,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[0.5]},
                {"bufferView":4,"componentType":5126,"count":2,"type":"VEC3"}
            ],
            "animations": [{"name":"walk","samplers":[{"input":3,"output":4}],
                "channels":[{"sampler":0,"target":{"node":1,"path":"translation"}}]}]
        }).to_string());
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ordinary_game_scripts_use_presentation_and_animation_without_headless_guards() {
    let project = Project::new(
        r#"
function init()
    assert(ui.screen_width() == 800 and ui.screen_height() == 600)
    assert(ui.text_width('aé', 16) == 24)
    ui.text(10, 20, 'ready', 16, 1, 1, 1, 1)
    ui.rect(0, 0, 30, 40, 1, 0, 0, 0.5)
    ui.flash(1, 0, 0, 0.8, 0.5)
    local sx, sy, visible = camera.world_to_screen(0, 0, -5)
    assert(visible and math.abs(sx - 400) < 0.001 and math.abs(sy - 300) < 0.001)
    local ox, oy, oz, dx, dy, dz = camera.screen_to_ray(sx, sy)
    assert(math.abs(oz + 0.1) < 0.001 and math.abs(dx) < 0.001 and math.abs(dy) < 0.001 and dz < -0.999)
    camera.shake(0.1, 0.2)
    entity.set_emitter_enabled('actor', false)
    entity.set_emitter_rate('actor', 0)
    entity.burst('actor', 2)
    particles.spawn_burst(0, 0, 0, 3, { lifetime_min=0.05, lifetime_max=0.05, gravity_scale=0 })
    assert(animation.get_state('actor') == 'walk' and animation.get_time('actor') == 0)
    animation.play('actor', 'walk')
    animation.set_speed('actor', 3)
    animation.set_looping('actor', false)
end
function update(dt)
    ui.text(0, 0, 'tick', 16, 1, 1, 1, 1)
    game.width = ui.screen_width()
    game.height = ui.screen_height()
end
"#,
    );
    project.animated_mesh();
    project.write(
        "scene.yaml",
        r#"
name: Presentation parity
entities:
  - id: actor
    components:
      transform: {}
      camera: {near: 0.1, far: 100, role: main}
      mesh_renderer: {mesh: animated.gltf, material: 'procedural:default'}
      animator: {state: walk, speed: 2, looping: false}
      particle_emitter: {spawn_rate: 0, lifetime: [0.05, 0.05]}
      script: {source: game.lua}
"#,
    );
    let mut runner = TestRunner::new(&project.0);
    runner.set_viewport(800, 600);
    runner.load_scene("scene.yaml").unwrap();
    assert_eq!(runner.ui_frame.borrow().intents.len(), 2);
    assert_eq!(runner.particle_system.borrow().particle_count(), 5);
    runner.step_frames(2);
    let world = runner.scene_world.borrow();
    let actor = world.entity_registry["actor"];
    let animator = world
        .world
        .get::<&crate::components::Animator>(actor)
        .unwrap();
    assert!((animator.controller.current_time - 0.1).abs() < 1e-5);
    drop(animator);
    drop(world);
    assert_eq!(runner.ui_frame.borrow().intents.len(), 1);
    let frame = runner.ui_frame.borrow();
    let Some(UiIntent::Rect { color, .. }) = frame.flash(*runner.viewport.borrow()) else {
        panic!("missing flash")
    };
    assert!((color[3] - 0.8 * (0.5 - 2.0 / 60.0) / 0.5).abs() < 1e-5);
    drop(frame);
    runner.set_viewport(400, 900);
    runner.step_frames(32);
    let game: LuaTable = runner.script_runtime.lua.globals().get("game").unwrap();
    assert_eq!(game.get::<u32>("width").unwrap(), 400);
    assert_eq!(game.get::<u32>("height").unwrap(), 900);
    assert_eq!(runner.particle_system.borrow().particle_count(), 0);
    assert_eq!(runner.ui_frame.borrow().flash_remaining, 0.0);
    runner.script_runtime.lua.load("assert(animation.get_time('actor') == 0.5); animation.stop('actor'); assert(animation.get_time('actor') == 0)").exec().unwrap();
    assert!(runner.take_errors().is_empty());
}

#[test]
fn camera_queries_report_missing_camera_and_track_viewport_and_live_transforms() {
    let project = Project::new("");
    project.write("scene.yaml", "name: Camera\nentities:\n  - id: camera\n    components:\n      transform: {}\n      camera: {}\n");
    let mut runner = TestRunner::new(&project.0);
    runner.load_scene("scene.yaml").unwrap();
    runner.set_viewport(640, 480);
    runner
        .script_runtime
        .lua
        .load(
            r#"
local x,y,visible = camera.world_to_screen(0,0,-5)
assert(visible and math.abs(x-320)<0.001 and math.abs(y-240)<0.001)
entity.set_position('camera', 10, 0, 0)
local x,y,visible = camera.world_to_screen(10,0,-5)
assert(visible and math.abs(x-320)<0.001 and math.abs(y-240)<0.001)
"#,
        )
        .exec()
        .unwrap();
    project.write("empty.yaml", "name: Empty\nentities: []\n");
    runner.load_scene("empty.yaml").unwrap();
    for code in ["camera.world_to_screen(0,0,0)", "camera.screen_to_ray(0,0)"] {
        let error = runner.script_runtime.lua.load(code).exec().unwrap_err();
        assert!(error.to_string().contains("requires a main camera"));
    }
}
