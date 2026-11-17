//! Lua audio API registration.

use super::ScriptRuntime;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Audio operations used by Lua, independent of an audio device or library.
pub trait AudioBackend {
    fn play_sfx(
        &mut self,
        id: &str,
        project_root: &Path,
        path: &str,
        volume: f32,
    ) -> Result<(), String>;
    fn play_music(
        &mut self,
        project_root: &Path,
        path: &str,
        volume: f32,
        fade_in_secs: f32,
    ) -> Result<(), String>;
    fn stop_sound(&mut self, id: &str, fade_out_secs: f32);
    fn stop_music(&mut self, fade_out_secs: f32);
}

/// Headless audio preserves the Lua interface without device or filesystem IO.
#[derive(Debug, Default)]
pub struct SilentAudio;

impl AudioBackend for SilentAudio {
    fn play_sfx(
        &mut self,
        _id: &str,
        _project_root: &Path,
        _path: &str,
        _volume: f32,
    ) -> Result<(), String> {
        Ok(())
    }

    fn play_music(
        &mut self,
        _project_root: &Path,
        _path: &str,
        _volume: f32,
        _fade_in_secs: f32,
    ) -> Result<(), String> {
        Ok(())
    }

    fn stop_sound(&mut self, _id: &str, _fade_out_secs: f32) {}

    fn stop_music(&mut self, _fade_out_secs: f32) {}
}

impl ScriptRuntime {
    /// Register audio API functions that control the audio system from Lua.
    pub fn register_audio_api<A: AudioBackend + 'static>(
        &self,
        audio_system: Rc<RefCell<A>>,
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
