//! The engine's side of the shared frame schedule (`naive_sim::sim::frame`):
//! the GPU, audio and HUD work the GPU-free schedule cannot do itself. The
//! headless test runner runs the same schedule with its own hooks.

use naive_sim::sim::frame::{run_frame, FrameHost, FrameMode, FrameState};

use super::Engine;
use crate::components::{Player, Transform};

impl Engine {
    /// Run this frame's simulation through the shared schedule. While paused
    /// only world transforms and swarm draw data are updated.
    pub(super) fn tick_simulation(&mut self) {
        let mode = if self.paused {
            FrameMode::Paused
        } else {
            FrameMode::Running
        };
        self.tick_simulation_as(mode);
    }

    /// Run a whole simulation frame even while the editor is paused. The
    /// benchmark measures whole frames, gameplay included, and promises
    /// that the simulation advances.
    pub(super) fn tick_simulation_running(&mut self) {
        self.tick_simulation_as(FrameMode::Running);
    }

    fn tick_simulation_as(&mut self, mode: FrameMode) {
        let dt = self.delta_time;
        run_frame(&mut NativeFrame(self), dt, mode);
    }
}

/// The engine as a host of the shared frame schedule.
struct NativeFrame<'a>(&'a mut Engine);

impl FrameHost for NativeFrame<'_> {
    fn frame_state(&mut self) -> Option<FrameState<'_>> {
        let engine = &mut *self.0;
        Some(FrameState {
            scene_world: engine.scene_world.as_ref()?,
            physics_world: engine.physics_world.as_ref()?,
            input: engine.input_state.as_ref()?,
            script_runtime: engine.script_runtime.as_mut()?,
            event_bus: &engine.event_bus,
            lua_event_listeners: &engine.lua_event_listeners,
            entity_commands: &engine.entity_commands,
            swarm_bus: &engine.swarm_bus,
            ui_frame: &engine.script_ui,
            camera_shake: &engine.camera_shake,
            particle_system: &*engine.particle_system,
            tween_system: &mut engine.tween_system,
        })
    }

    fn begin_input(&mut self) {
        // winit delivers input between frames. `run_frame` clears the
        // per-frame part after rendering, once the editor camera, cursor
        // capture and debug keys have read it.
    }

    fn player_control_allowed(&self) -> bool {
        // In the editor the free camera has the mouse and keys.
        !self.0.args.editor_mode
    }

    fn advance_animations(&mut self, dt: f32) {
        self.0.tick_animations(dt);
    }

    fn apply_entity_commands(&mut self) {
        self.0.process_entity_commands();
    }

    fn load_requested_scene(&mut self, path: String) {
        self.0.load_requested_scene(&path);
    }

    fn report_limits(&mut self, messages: Vec<String>) {
        let now = std::time::Instant::now();
        for message in messages {
            self.0
                .reload_notifications
                .push((message, now, [1.0, 0.3, 0.3, 1.0]));
        }
    }

    fn update_audio(&mut self) {
        let engine = &mut *self.0;
        let mut audio = engine.audio_system.borrow_mut();
        audio.cleanup();
        // Only the first player entity drives the listener.
        if let Some(scene_world) = &engine.scene_world {
            let scene_world = scene_world.borrow();
            let listener = scene_world
                .world
                .query::<(&Transform, &Player)>()
                .iter()
                .next()
                .map(|(transform, _player)| transform.position);
            if let Some(position) = listener {
                audio.set_listener_position(position);
            }
        }
    }

    fn step_swarms(&mut self, dt: f32) {
        // Once per frame, not again for captures. A zero `dt` (paused) only
        // rewrites their draw data.
        let engine = &mut *self.0;
        if let (Some(gpu), Some(scene_world), Some(draw_pool)) =
            (&engine.gpu, &engine.scene_world, &engine.draw_pool)
        {
            let physics = engine.physics_world.as_ref().map(|p| p.borrow());
            engine.swarm_system.update(
                gpu,
                &scene_world.borrow(),
                physics.as_deref(),
                &engine.mesh_cache,
                &engine.material_cache,
                draw_pool,
                &mut engine.swarm_bus.borrow_mut(),
                engine.gpu_timer.as_ref(),
                dt,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CliArgs;
    use crate::physics::PhysicsWorld;
    use crate::scripting::ScriptRuntime;
    use crate::world::SceneWorld;
    use naive_sim::sim::frame::{trace::record_phases, FramePhase, FRAME_PHASES};
    use naive_sim::test_runner::TestRunner;
    use std::cell::RefCell;
    use std::rc::Rc;

    const SCENE: &str = r#"
name: Frame schedule
settings: {gravity: [0, -9.81, 0]}
entities:
  - id: player
    components:
      transform: {position: [0, 1, 0]}
      character_controller: {move_speed: 4.0, height: 1.8, radius: 0.3}
      script: {source: frame.lua}
  - id: floor
    components:
      transform: {position: [0, -0.5, 0]}
      collider: {shape: box, half_extents: [10, 0.5, 10]}
"#;

    const SCRIPT: &str = r#"
function init()
    events.on('ready', function() game.heard = (game.heard or 0) + 1 end)
    events.emit('ready', {})
end
function update(dt)
    game.updates = (game.updates or 0) + 1
    if game.heard_at_update == nil then game.heard_at_update = game.heard or 0 end
end
"#;

    struct Project(std::path::PathBuf);

    impl Project {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("naive-frame-schedule-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("scene.yaml"), SCENE).unwrap();
            std::fs::write(path.join("frame.lua"), SCRIPT).unwrap();
            Self(path)
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An engine with the project's scene built on the CPU, as far as an
    /// engine without a window can be.
    fn engine(project: &Project, editor_mode: bool) -> Engine {
        let mut engine = Engine::new(CliArgs {
            command: None,
            scene: None,
            pipeline: None,
            project: project.0.to_string_lossy().into_owned(),
            socket: String::new(),
            hud: false,
            editor_mode,
        });
        engine.audio_system = Rc::new(RefCell::new(crate::audio::AudioSystem::silent()));
        let scene = crate::scene::load_scene(&project.0.join("scene.yaml")).unwrap();
        let world = Rc::new(RefCell::new(SceneWorld::new()));
        let physics = Rc::new(RefCell::new(PhysicsWorld::new(glam::Vec3::from(
            scene.settings.gravity,
        ))));
        crate::world::spawn_all_entities_headless(
            &mut world.borrow_mut(),
            &scene,
            &mut physics.borrow_mut(),
        );
        let input = Rc::new(RefCell::new(crate::input::InputState::new(
            crate::input::load_bindings(&project.0),
        )));
        input.borrow_mut().cursor_captured = true;
        engine.scene_world = Some(world);
        engine.physics_world = Some(physics);
        engine.input_state = Some(input);
        let mut runtime = ScriptRuntime::new();
        engine.register_script_apis(&runtime);
        naive_sim::sim::initialize_pending_scripts(
            engine.scene_world.as_ref().unwrap(),
            &mut runtime,
            &project.0,
        )
        .unwrap();
        engine.script_runtime = Some(runtime);
        engine
    }

    fn game_value(runtime: &ScriptRuntime, key: &str) -> i64 {
        let game: mlua::Table = runtime.lua.globals().get("game").unwrap();
        game.get(key).unwrap()
    }

    /// The engine's and the headless runner's real frame entry points run
    /// the same phases in the same order, the one `FRAME_PHASES` lists.
    #[test]
    fn native_and_headless_frames_run_the_shared_schedule() {
        let project = Project::new();
        let mut native = engine(&project, false);
        let mut runner = TestRunner::new(&project.0);
        runner.load_scene("scene.yaml").unwrap();

        for frame in 1..=3 {
            let native_phases = record_phases(|| native.tick_simulation());
            let headless_phases = record_phases(|| runner.step_frame());
            assert_eq!(native_phases, FRAME_PHASES, "native frame {frame}");
            assert_eq!(headless_phases, native_phases, "headless frame {frame}");
        }
        for runtime in [
            native.script_runtime.as_ref().unwrap(),
            &runner.script_runtime,
        ] {
            // Both really ran the scripts.
            assert_eq!(game_value(runtime, "updates"), 3);
            // What `init` emitted reached its listener once, on the first
            // frame's event phase, after that frame's `update`.
            assert_eq!(game_value(runtime, "heard"), 1);
            assert_eq!(game_value(runtime, "heard_at_update"), 0);
        }
        assert!(runner.take_errors().is_empty());
        assert_eq!(
            native.script_runtime.as_ref().unwrap().take_errors(),
            Vec::<String>::new()
        );
    }

    /// The engine's own differences are in its hooks, not in the order:
    /// the editor's free camera keeps the player controller off, so does a
    /// released cursor, and a paused engine only updates transforms and
    /// swarm draw data.
    #[test]
    fn editor_released_cursor_and_paused_frames_skip_phases_in_order() {
        let project = Project::new();
        let without_player: Vec<_> = FRAME_PHASES
            .into_iter()
            .filter(|phase| *phase != FramePhase::PlayerController)
            .collect();

        let mut editor = engine(&project, true);
        assert_eq!(record_phases(|| editor.tick_simulation()), without_player);

        let mut released = engine(&project, false);
        released
            .input_state
            .as_ref()
            .unwrap()
            .borrow_mut()
            .cursor_captured = false;
        assert_eq!(record_phases(|| released.tick_simulation()), without_player);

        let mut paused = engine(&project, false);
        paused.paused = true;
        assert_eq!(
            record_phases(|| paused.tick_simulation()),
            [FramePhase::Transforms, FramePhase::Swarms]
        );
        let game: mlua::Table = paused
            .script_runtime
            .as_ref()
            .unwrap()
            .lua
            .globals()
            .get("game")
            .unwrap();
        assert_eq!(game.get::<Option<i64>>("updates").unwrap(), None);
    }

    /// The benchmark runs whole frames while the editor is paused: a paused
    /// frame would skip physics and scripts and flatter the measurement.
    #[test]
    fn benchmark_frames_simulate_while_paused() {
        let project = Project::new();
        let mut paused = engine(&project, false);
        paused.paused = true;
        for _ in 0..2 {
            assert_eq!(
                record_phases(|| paused.tick_simulation_running()),
                FRAME_PHASES
            );
        }
        assert!(paused.paused, "the pause outlasts the benchmark");
        assert_eq!(
            game_value(paused.script_runtime.as_ref().unwrap(), "updates"),
            2
        );
    }
}
