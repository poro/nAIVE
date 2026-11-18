//! The frame schedule: the one ordered list of per-frame steps that the
//! native engine (`Engine::tick_simulation`) and the headless test runner
//! ([`crate::test_runner::TestRunner::step_frame`]) both run.
//!
//! Each used to list its steps by hand, the two orders drifted, and headless
//! tests could disagree with the game. The order now lives only in
//! [`FRAME_PHASES`]. GPU-free phases run here on the [`FrameState`] the host
//! lends for each phase; work only one side can do (GPU uploads and swarms,
//! audio playback, HUD notifications, the test runner's input and failures)
//! plugs in through the [`FrameHost`] hooks. Every intentional difference
//! between the engine and the runner lives in a hook implementation, never in
//! the order.
//!
//! The schedule keeps nothing borrowed while a hook runs: each phase asks the
//! host for its state again, so a hook may replace the scene or the Lua VM.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::particles::ParticleSystem;
use crate::presentation::{CameraShakeState, UiFrame};
use crate::scripting::{
    ScriptRuntime, SharedEntityCommandQueue, SharedEventBus, SharedInputState, SharedPhysicsWorld,
    SharedSceneWorld,
};
use crate::swarm_api::SharedSwarmBus;
use crate::tween::TweenSystem;

/// One step of a frame. [`FRAME_PHASES`] lists them in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FramePhase {
    /// [`FrameHost::begin_input`]: make this frame's input current.
    Input,
    /// Mouse look, movement and jumping for player characters, while the
    /// cursor is captured and [`FrameHost::player_control_allowed`].
    PlayerController,
    /// Physics in fixed steps, `fixed_update` before each one.
    FixedSteps,
    /// Collision and trigger hooks from those steps.
    PhysicsEvents,
    /// Contact damage and projectile hits.
    CollisionDamage,
    /// Projectile ages; expired ones are queued for destruction.
    Projectiles,
    /// Swarm attacks damage their targets; swarm events go out.
    SwarmReports,
    /// Deaths and `on_death` hooks.
    Health,
    /// `update(dt)` on every scripted entity.
    Scripts,
    /// [`FrameHost::advance_animations`]: skeletal animation controllers.
    Animations,
    /// The UI flash and camera-shake timers.
    PresentationTimers,
    /// Deferred entity commands: queued destroys first, then
    /// [`FrameHost::apply_entity_commands`] for everything else.
    EntityCommands,
    /// [`FrameHost::load_requested_scene`], when a script called `scene.load`.
    SceneLoad,
    /// The event bus advances and Lua `events.on` listeners hear this
    /// frame's events.
    LuaEvents,
    /// [`FrameHost::report_limits`] for scripts and listeners stopped this
    /// frame for exceeding a limit.
    LimitReports,
    /// Tweens advance.
    Tweens,
    /// CPU particles advance.
    Particles,
    /// [`FrameHost::update_audio`].
    Audio,
    /// World transforms from the transform hierarchy.
    Transforms,
    /// [`FrameHost::step_swarms`].
    Swarms,
}

/// Every phase, in the order a frame runs them.
///
/// Animations come straight after the scripts that drive them and before
/// the UI flash and camera-shake timers. The three touch disjoint state
/// (animator components, the UI frame, the shake timer) and run no Lua, so
/// neither order changes a result; this one keeps the world-state steps
/// together ahead of the presentation timers, and is the order the headless
/// tests were written against.
pub const FRAME_PHASES: [FramePhase; 20] = [
    FramePhase::Input,
    FramePhase::PlayerController,
    FramePhase::FixedSteps,
    FramePhase::PhysicsEvents,
    FramePhase::CollisionDamage,
    FramePhase::Projectiles,
    FramePhase::SwarmReports,
    FramePhase::Health,
    FramePhase::Scripts,
    FramePhase::Animations,
    FramePhase::PresentationTimers,
    FramePhase::EntityCommands,
    FramePhase::SceneLoad,
    FramePhase::LuaEvents,
    FramePhase::LimitReports,
    FramePhase::Tweens,
    FramePhase::Particles,
    FramePhase::Audio,
    FramePhase::Transforms,
    FramePhase::Swarms,
];

impl FramePhase {
    /// Whether the phase also runs while the engine is paused, with a zero
    /// time step: edits made while paused still need world transforms, and
    /// GPU swarms rewrite their draw data without moving.
    pub const fn runs_while_paused(self) -> bool {
        matches!(self, FramePhase::Transforms | FramePhase::Swarms)
    }
}

/// Whether simulated time passes this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameMode {
    Running,
    /// Only the phases that [`FramePhase::runs_while_paused`] run, with a
    /// zero time step. The headless runner never pauses.
    Paused,
}

/// The shared state a host lends to the GPU-free phases. Hosts build it
/// from their own fields; it lives for one phase at a time.
pub struct FrameState<'a> {
    pub scene_world: &'a SharedSceneWorld,
    pub physics_world: &'a SharedPhysicsWorld,
    pub input: &'a SharedInputState,
    pub script_runtime: &'a mut ScriptRuntime,
    pub event_bus: &'a SharedEventBus,
    pub lua_event_listeners: &'a RefCell<HashMap<String, Vec<mlua::RegistryKey>>>,
    pub entity_commands: &'a SharedEntityCommandQueue,
    pub swarm_bus: &'a SharedSwarmBus,
    pub ui_frame: &'a RefCell<UiFrame>,
    pub camera_shake: &'a RefCell<CameraShakeState>,
    /// The engine wraps the CPU particles for its GPU adapter.
    pub particle_system: &'a RefCell<dyn AsMut<ParticleSystem>>,
    pub tween_system: &'a mut TweenSystem,
}

/// What the native engine and the headless runner each supply to the shared
/// schedule. Every method is one documented difference between them.
pub trait FrameHost {
    /// The state the GPU-free phases run on, or `None` before a scene has
    /// loaded (those phases are then skipped).
    fn frame_state(&mut self) -> Option<FrameState<'_>>;

    /// [`FramePhase::Input`]. Headless: apply the input a test injected and
    /// capture the cursor, so tests drive the player as if the mouse were
    /// captured. Native: nothing, because winit delivers input between frames
    /// and the engine clears it after rendering, once the editor camera and
    /// debug keys have read it.
    fn begin_input(&mut self);

    /// Whether the player controller may run when the cursor is captured.
    /// Native: not in editor mode, where the free camera has the mouse.
    fn player_control_allowed(&self) -> bool;

    /// [`FramePhase::Animations`]. Native also builds the bone palettes it
    /// draws with; headless only advances the controllers.
    fn advance_animations(&mut self, dt: f32);

    /// [`FramePhase::EntityCommands`], after the queued destroys: runtime
    /// meshes, spawns, pools, scale and visibility, then `init` for new
    /// scripts. Native uploads meshes and textures to the GPU; headless
    /// builds entities on the CPU and fails the test on GPU-only commands.
    fn apply_entity_commands(&mut self);

    /// [`FramePhase::SceneLoad`]: replace the scene with `path`, the
    /// project-relative scene a script passed to `scene.load` this frame.
    fn load_requested_scene(&mut self, path: String);

    /// [`FramePhase::LimitReports`], with at least one message. Native shows
    /// them on the HUD. The runtime has already recorded each as an error,
    /// which is what fails a headless test.
    fn report_limits(&mut self, messages: Vec<String>);

    /// [`FramePhase::Audio`]. Native drops finished sounds and moves the
    /// listener to the player; headless audio is silent and has neither.
    fn update_audio(&mut self);

    /// [`FramePhase::Swarms`]. Native steps swarms on the GPU; headless runs
    /// the CPU reference simulation with the same maths.
    fn step_swarms(&mut self, dt: f32);
}

/// Run one frame of `dt` seconds through the shared schedule.
pub fn run_frame<H: FrameHost + ?Sized>(host: &mut H, dt: f32, mode: FrameMode) {
    for phase in FRAME_PHASES {
        let dt = match mode {
            FrameMode::Running => dt,
            FrameMode::Paused if phase.runs_while_paused() => 0.0,
            FrameMode::Paused => continue,
        };
        if run_phase(host, phase, dt) {
            trace::note(phase);
        }
    }
}

/// Run `phase`. False when it was skipped: no scene is loaded, or the player
/// controller is off.
fn run_phase<H: FrameHost + ?Sized>(host: &mut H, phase: FramePhase, dt: f32) -> bool {
    match phase {
        // Host hooks alone; they run even before a scene has loaded.
        FramePhase::Input => host.begin_input(),
        FramePhase::Animations => host.advance_animations(dt),
        FramePhase::Audio => host.update_audio(),
        FramePhase::Swarms => host.step_swarms(dt),
        FramePhase::PlayerController if !host.player_control_allowed() => return false,
        _ => {
            let Some(state) = host.frame_state() else {
                return false;
            };
            // The shared step consumes the state, so nothing is still
            // borrowed when the host's hook runs after it.
            match run_shared_step(state, phase, dt) {
                Then::Done => {}
                Then::Skip => return false,
                Then::ApplyEntityCommands => host.apply_entity_commands(),
                Then::LoadScene(path) => host.load_requested_scene(path),
                Then::ReportLimits(messages) => host.report_limits(messages),
            }
        }
    }
    true
}

/// What is left of a phase once its shared step has released the state.
enum Then {
    Done,
    /// The phase did not run: the cursor is not captured.
    Skip,
    ApplyEntityCommands,
    LoadScene(String),
    ReportLimits(Vec<String>),
}

/// The GPU-free step of `phase`, on the state its host lent.
fn run_shared_step(state: FrameState<'_>, phase: FramePhase, dt: f32) -> Then {
    let FrameState {
        scene_world,
        physics_world,
        input,
        script_runtime,
        event_bus,
        lua_event_listeners,
        entity_commands,
        swarm_bus,
        ui_frame,
        camera_shake,
        particle_system,
        tween_system,
    } = state;
    match phase {
        FramePhase::PlayerController => {
            if !input.borrow().cursor_captured {
                return Then::Skip;
            }
            super::update_fps_controller(input, scene_world, physics_world, dt);
        }
        FramePhase::FixedSteps => {
            super::run_fixed_steps(physics_world, scene_world, Some(script_runtime), dt);
        }
        FramePhase::PhysicsEvents => {
            super::dispatch_physics_events(scene_world, physics_world, script_runtime)
        }
        FramePhase::CollisionDamage => super::process_collision_damage(
            scene_world,
            physics_world,
            script_runtime,
            entity_commands,
        ),
        FramePhase::Projectiles => super::update_projectiles(scene_world, entity_commands, dt),
        FramePhase::SwarmReports => {
            super::process_swarm_reports(swarm_bus, scene_world, script_runtime, event_bus)
        }
        FramePhase::Health => super::process_health(scene_world, script_runtime),
        FramePhase::Scripts => super::update_scripts(scene_world, script_runtime, dt),
        FramePhase::PresentationTimers => {
            ui_frame.borrow_mut().advance(dt);
            camera_shake.borrow_mut().advance(dt);
        }
        FramePhase::EntityCommands => {
            // Destroys go first, so a destroy and a spawn of the same id in
            // one frame leave the new entity.
            super::process_destroys(
                scene_world,
                Some(physics_world),
                Some(script_runtime),
                entity_commands,
            );
            return Then::ApplyEntityCommands;
        }
        FramePhase::SceneLoad => {
            let requested = entity_commands.borrow_mut().pending_scene_load.take();
            if let Some(path) = requested {
                return Then::LoadScene(path);
            }
        }
        FramePhase::LuaEvents => {
            super::dispatch_lua_events(event_bus, lua_event_listeners, Some(script_runtime), dt)
        }
        FramePhase::LimitReports => {
            let messages = script_runtime.take_limit_reports();
            if !messages.is_empty() {
                return Then::ReportLimits(messages);
            }
        }
        FramePhase::Tweens => {
            let _completed = tween_system.update(dt);
        }
        FramePhase::Particles => particle_system
            .borrow_mut()
            .as_mut()
            .update(dt, &scene_world.borrow().world),
        FramePhase::Transforms => {
            crate::transform::update_transforms(&mut scene_world.borrow_mut().world)
        }
        FramePhase::Input | FramePhase::Animations | FramePhase::Audio | FramePhase::Swarms => {
            unreachable!("{phase:?} is a host hook, which run_phase runs itself")
        }
    }
    Then::Done
}

/// Records which phases ran, so tests can check that the engine and the
/// headless runner go through the same schedule.
#[cfg(any(test, feature = "test-support"))]
pub mod trace {
    use super::FramePhase;
    use std::cell::RefCell;

    thread_local! {
        static PHASES: RefCell<Option<Vec<FramePhase>>> = const { RefCell::new(None) };
    }

    pub(super) fn note(phase: FramePhase) {
        PHASES.with(|phases| {
            if let Some(phases) = phases.borrow_mut().as_mut() {
                phases.push(phase);
            }
        });
    }

    /// Run `f` and return the phases that ran on this thread meanwhile.
    pub fn record_phases(f: impl FnOnce()) -> Vec<FramePhase> {
        /// Puts the outer recording back even if `f` panics.
        struct Restore(Option<Option<Vec<FramePhase>>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(outer) = self.0.take() {
                    PHASES.with(|phases| phases.replace(outer));
                }
            }
        }
        let mut restore = Restore(Some(PHASES.with(|phases| phases.replace(Some(Vec::new())))));
        f();
        let outer = restore.0.take().unwrap_or_default();
        PHASES
            .with(|phases| phases.replace(outer))
            .unwrap_or_default()
    }
}

#[cfg(not(any(test, feature = "test-support")))]
mod trace {
    #[inline(always)]
    pub(super) fn note(_phase: super::FramePhase) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that only records which hooks ran, around the shared phases.
    struct Recorder {
        hooks: Vec<&'static str>,
        player_control: bool,
    }

    impl FrameHost for Recorder {
        fn frame_state(&mut self) -> Option<FrameState<'_>> {
            None
        }
        fn begin_input(&mut self) {
            self.hooks.push("input");
        }
        fn player_control_allowed(&self) -> bool {
            self.player_control
        }
        fn advance_animations(&mut self, _dt: f32) {
            self.hooks.push("animations");
        }
        fn apply_entity_commands(&mut self) {
            self.hooks.push("entity_commands");
        }
        fn load_requested_scene(&mut self, _path: String) {
            self.hooks.push("scene_load");
        }
        fn report_limits(&mut self, _messages: Vec<String>) {
            self.hooks.push("limits");
        }
        fn update_audio(&mut self) {
            self.hooks.push("audio");
        }
        fn step_swarms(&mut self, dt: f32) {
            self.hooks
                .push(if dt == 0.0 { "swarms(0)" } else { "swarms" });
        }
    }

    #[test]
    fn every_phase_appears_once() {
        for phase in FRAME_PHASES {
            assert_eq!(
                FRAME_PHASES.iter().filter(|p| **p == phase).count(),
                1,
                "{phase:?}"
            );
        }
    }

    #[test]
    fn without_a_scene_only_host_hooks_run_and_paused_frames_keep_swarm_draws() {
        let mut host = Recorder {
            hooks: Vec::new(),
            player_control: true,
        };
        let phases = trace::record_phases(|| run_frame(&mut host, 0.5, FrameMode::Running));
        assert_eq!(
            phases,
            [
                FramePhase::Input,
                FramePhase::Animations,
                FramePhase::Audio,
                FramePhase::Swarms
            ]
        );
        assert_eq!(host.hooks, ["input", "animations", "audio", "swarms"]);

        host.hooks.clear();
        let phases = trace::record_phases(|| run_frame(&mut host, 0.5, FrameMode::Paused));
        assert_eq!(phases, [FramePhase::Swarms]);
        assert_eq!(host.hooks, ["swarms(0)"]);
    }
}
