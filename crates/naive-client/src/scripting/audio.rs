//! Lua audio API registration.

use super::{ScriptRuntime, SharedAudioSystem};
use std::path::PathBuf;

impl ScriptRuntime {
    /// Register audio API functions that control the audio system from Lua.
    pub fn register_audio_api(
        &self,
        audio_system: SharedAudioSystem,
        project_root: PathBuf,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let audio_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // audio.play_sfx(id, path, volume)
        let root1 = project_root.clone();
        let audio = audio_system.clone();
        let play_sfx_fn = self
            .lua
            .create_function(move |_, (id, path, volume): (String, String, f32)| {
                let mut audio = audio.borrow_mut();
                if let Err(e) = audio.play_sfx(&id, &root1, &path, volume) {
                    tracing::error!("[Lua] audio.play_sfx error: {}", e);
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        audio_table
            .set("play_sfx", play_sfx_fn)
            .map_err(|e| e.to_string())?;

        // audio.play_music(path, volume, fade_in_secs)
        let root2 = project_root.clone();
        let audio = audio_system.clone();
        let play_music_fn = self
            .lua
            .create_function(move |_, (path, volume, fade_in): (String, f32, f32)| {
                let mut audio = audio.borrow_mut();
                if let Err(e) = audio.play_music(&root2, &path, volume, fade_in) {
                    tracing::error!("[Lua] audio.play_music error: {}", e);
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        audio_table
            .set("play_music", play_music_fn)
            .map_err(|e| e.to_string())?;

        // audio.stop_sound(id, fade_out_secs)
        let audio = audio_system.clone();
        let stop_sound_fn = self
            .lua
            .create_function(move |_, (id, fade_out): (String, f32)| {
                let mut audio = audio.borrow_mut();
                audio.stop_sound(&id, fade_out);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        audio_table
            .set("stop_sound", stop_sound_fn)
            .map_err(|e| e.to_string())?;

        // audio.stop_music(fade_out_secs)
        let audio = audio_system.clone();
        let stop_music_fn = self
            .lua
            .create_function(move |_, fade_out: f32| {
                let mut audio = audio.borrow_mut();
                audio.stop_music(fade_out);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        audio_table
            .set("stop_music", stop_music_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("audio", audio_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
