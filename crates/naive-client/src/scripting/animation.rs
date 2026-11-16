//! Lua animation API registration.

use super::{ScriptRuntime, SharedSceneWorld};

impl ScriptRuntime {
    /// Register animation API (play, stop, set_state, get_state, set_speed).
    pub fn register_animation_api(&self, scene_world: SharedSceneWorld) -> Result<(), String> {
        let globals = self.lua.globals();
        let anim_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // animation.play(entity_id, state_name)
        // state_name: "idle", "walk", "run", "attack", or custom string
        let sw = scene_world.clone();
        let play_fn = self
            .lua
            .create_function(move |_, (id, state): (String, String)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut animator) =
                        sw.world.get::<&mut crate::components::Animator>(entity)
                    {
                        let anim_state = match state.as_str() {
                            "idle" => naive_core::animation::AnimState::Idle,
                            "walk" => naive_core::animation::AnimState::Walk,
                            "run" => naive_core::animation::AnimState::Run,
                            "attack" => naive_core::animation::AnimState::Attack,
                            other => naive_core::animation::AnimState::Custom(other.to_string()),
                        };
                        animator.controller.play(anim_state);
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        anim_table.set("play", play_fn).map_err(|e| e.to_string())?;

        // animation.stop(entity_id)
        let sw = scene_world.clone();
        let stop_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut animator) =
                        sw.world.get::<&mut crate::components::Animator>(entity)
                    {
                        animator.controller.stop();
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        anim_table.set("stop", stop_fn).map_err(|e| e.to_string())?;

        // animation.set_speed(entity_id, speed)
        let sw = scene_world.clone();
        let speed_fn = self
            .lua
            .create_function(move |_, (id, speed): (String, f32)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut animator) =
                        sw.world.get::<&mut crate::components::Animator>(entity)
                    {
                        animator.controller.speed = speed;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        anim_table
            .set("set_speed", speed_fn)
            .map_err(|e| e.to_string())?;

        // animation.set_looping(entity_id, looping)
        let sw = scene_world.clone();
        let loop_fn = self
            .lua
            .create_function(move |_, (id, looping): (String, bool)| {
                let sw = sw.borrow_mut();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(mut animator) =
                        sw.world.get::<&mut crate::components::Animator>(entity)
                    {
                        animator.controller.looping = looping;
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        anim_table
            .set("set_looping", loop_fn)
            .map_err(|e| e.to_string())?;

        // animation.get_state(entity_id) -> string
        let sw = scene_world.clone();
        let get_state_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(animator) = sw.world.get::<&crate::components::Animator>(entity) {
                        return Ok(animator.controller.current_state.clip_name().to_string());
                    }
                }
                Ok("none".to_string())
            })
            .map_err(|e| e.to_string())?;
        anim_table
            .set("get_state", get_state_fn)
            .map_err(|e| e.to_string())?;

        // animation.get_time(entity_id) -> number
        let sw = scene_world.clone();
        let get_time_fn = self
            .lua
            .create_function(move |_, id: String| {
                let sw = sw.borrow();
                if let Some(&entity) = sw.entity_registry.get(&id) {
                    if let Ok(animator) = sw.world.get::<&crate::components::Animator>(entity) {
                        return Ok(animator.controller.current_time);
                    }
                }
                Ok(0.0f32)
            })
            .map_err(|e| e.to_string())?;
        anim_table
            .set("get_time", get_time_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("animation", anim_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
