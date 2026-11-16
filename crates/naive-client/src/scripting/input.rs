//! Lua input API registration.

use super::{ScriptRuntime, SharedInputState};

impl ScriptRuntime {
    /// Register input API functions that read from the input state.
    pub fn register_input_api(&self, input: SharedInputState) -> Result<(), String> {
        let globals = self.lua.globals();

        // input.pressed(action) -> bool
        let input_table = self.lua.create_table().map_err(|e| e.to_string())?;

        let input_rc = input.clone();
        let pressed_fn = self
            .lua
            .create_function(move |_, action: String| Ok(input_rc.borrow().pressed(&action)))
            .map_err(|e| e.to_string())?;
        input_table
            .set("pressed", pressed_fn)
            .map_err(|e| e.to_string())?;

        let input_rc = input.clone();
        let just_pressed_fn = self
            .lua
            .create_function(move |_, action: String| Ok(input_rc.borrow().just_pressed(&action)))
            .map_err(|e| e.to_string())?;
        input_table
            .set("just_pressed", just_pressed_fn)
            .map_err(|e| e.to_string())?;

        let input_rc = input.clone();
        let mouse_delta_fn = self
            .lua
            .create_function(move |_, ()| {
                let delta = input_rc.borrow().mouse_delta();
                Ok((delta.x, delta.y))
            })
            .map_err(|e| e.to_string())?;
        input_table
            .set("mouse_delta", mouse_delta_fn)
            .map_err(|e| e.to_string())?;

        // input.scroll_delta() -> (dx, dy) — scroll wheel delta this frame
        let input_rc = input.clone();
        let scroll_delta_fn = self
            .lua
            .create_function(move |_, ()| {
                let delta = input_rc.borrow().scroll_delta();
                Ok((delta.x, delta.y))
            })
            .map_err(|e| e.to_string())?;
        input_table
            .set("scroll_delta", scroll_delta_fn)
            .map_err(|e| e.to_string())?;

        // input.any_just_pressed() -> bool
        let input_rc = input.clone();
        let any_pressed_fn = self
            .lua
            .create_function(move |_, ()| Ok(input_rc.borrow().any_just_pressed()))
            .map_err(|e| e.to_string())?;
        input_table
            .set("any_just_pressed", any_pressed_fn)
            .map_err(|e| e.to_string())?;

        // input.mouse_position() -> (sx, sy) — cursor position in screen pixels
        let input_rc = input.clone();
        let mouse_pos_fn = self
            .lua
            .create_function(move |_, ()| {
                let pos = input_rc.borrow().cursor_position();
                Ok((pos.x, pos.y))
            })
            .map_err(|e| e.to_string())?;
        input_table
            .set("mouse_position", mouse_pos_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("input", input_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
