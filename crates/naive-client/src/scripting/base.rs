//! Lua base API registration.

use super::ScriptRuntime;
use mlua::prelude::*;

impl ScriptRuntime {
    /// Register the engine API functions into a Lua environment.
    pub fn register_api(&self) -> Result<(), String> {
        let globals = self.lua.globals();

        // Log function
        let log_fn = self
            .lua
            .create_function(|_, msg: String| {
                tracing::info!("[Lua] {}", msg);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        globals.set("log", log_fn).map_err(|e| e.to_string())?;

        // Print override
        let print_fn = self
            .lua
            .create_function(|_, args: LuaMultiValue| {
                let strs: Vec<String> = args.iter().map(|v| format!("{:?}", v)).collect();
                tracing::info!("[Lua] {}", strs.join("\t"));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        globals.set("print", print_fn).map_err(|e| e.to_string())?;

        // math.lerp(a, b, t) -> number
        let math_table: LuaTable = globals.get("math").map_err(|e| e.to_string())?;
        let lerp_fn = self
            .lua
            .create_function(|_, (a, b, t): (f64, f64, f64)| Ok(a + (b - a) * t))
            .map_err(|e| e.to_string())?;
        math_table.set("lerp", lerp_fn).map_err(|e| e.to_string())?;

        // math.clamp(value, min, max) -> number
        let clamp_fn = self
            .lua
            .create_function(|_, (value, min, max): (f64, f64, f64)| Ok(value.max(min).min(max)))
            .map_err(|e| e.to_string())?;
        math_table
            .set("clamp", clamp_fn)
            .map_err(|e| e.to_string())?;

        // Shared game state table (accessible from all script environments via globals metatable)
        let game_table = self.lua.create_table().map_err(|e| e.to_string())?;
        game_table
            .set("player_health", 100)
            .map_err(|e| e.to_string())?;
        game_table
            .set("game_over", false)
            .map_err(|e| e.to_string())?;
        game_table
            .set("level_complete", false)
            .map_err(|e| e.to_string())?;
        globals.set("game", game_table).map_err(|e| e.to_string())?;

        Ok(())
    }
}
