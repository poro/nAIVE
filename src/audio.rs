use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use kira::manager::{AudioManager, AudioManagerSettings, DefaultBackend};
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::sound::PlaybackState;
use kira::tween::Tween;

/// Audio component for entities that emit spatial sound.
#[derive(Debug, Clone)]
pub struct AudioSource {
    pub sound_path: PathBuf,
    pub volume: f32,
    pub looping: bool,
    pub spatial: bool,
    pub max_distance: f32,
}

/// Central audio system wrapping Kira.
pub struct AudioSystem {
    manager: Option<AudioManager>,
    /// Active sound handles keyed by a string identifier.
    sounds: HashMap<String, StaticSoundHandle>,
    /// Music track handle.
    music: Option<StaticSoundHandle>,
    /// Listener position for spatial audio.
    listener_pos: Vec3,
    /// Master volume.
    master_volume: f32,
}

impl AudioSystem {
    pub fn new() -> Self {
        let settings = AudioManagerSettings::<DefaultBackend> {
            capacities: kira::manager::Capacities {
                sound_capacity: 512,
                command_capacity: 256,
                ..Default::default()
            },
            ..Default::default()
        };
        let manager = AudioManager::<DefaultBackend>::new(settings)
            .map_err(|e| {
                tracing::warn!("Failed to initialize audio: {}. Audio disabled.", e);
                e
            })
            .ok();

        if manager.is_some() {
            tracing::info!("Audio system initialized (Kira)");
        }

        Self {
            manager,
            sounds: HashMap::new(),
            music: None,
            listener_pos: Vec3::ZERO,
            master_volume: 1.0,
        }
    }

    /// Update the listener position (typically the camera/player position).
    pub fn set_listener_position(&mut self, pos: Vec3) {
        self.listener_pos = pos;
    }

    /// Play a one-shot sound effect.
    pub fn play_sfx(
        &mut self,
        id: &str,
        project_root: &Path,
        path: &str,
        volume: f32,
    ) -> Result<(), String> {
        let manager = match &mut self.manager {
            Some(m) => m,
            None => return Ok(()), // Audio disabled
        };

        let full_path = project_root.join(path);
        let sound_data = StaticSoundData::from_file(&full_path)
            .map_err(|e| format!("Failed to load sound {:?}: {}", full_path, e))?;

        let handle = manager
            .play(sound_data.volume(volume as f64 * self.master_volume as f64))
            .map_err(|e| format!("Failed to play sound: {}", e))?;

        self.sounds.insert(id.to_string(), handle);
        Ok(())
    }

    /// Play music (replaces any currently playing music).
    pub fn play_music(
        &mut self,
        project_root: &Path,
        path: &str,
        volume: f32,
        fade_in_secs: f32,
    ) -> Result<(), String> {
        let manager = match &mut self.manager {
            Some(m) => m,
            None => return Ok(()),
        };

        // Stop current music
        if let Some(mut music) = self.music.take() {
            music.stop(Tween {
                duration: std::time::Duration::from_secs_f32(0.5),
                ..Default::default()
            });
        }

        let full_path = project_root.join(path);
        let sound_data = StaticSoundData::from_file(&full_path)
            .map_err(|e| format!("Failed to load music {:?}: {}", full_path, e))?;

        let handle = manager
            .play(sound_data.volume(0.0).loop_region(..))
            .map_err(|e| format!("Failed to play music: {}", e))?;

        // Fade in
        let mut handle = handle;
        handle.set_volume(
            volume as f64 * self.master_volume as f64,
            Tween {
                duration: std::time::Duration::from_secs_f32(fade_in_secs),
                ..Default::default()
            },
        );

        self.music = Some(handle);
        Ok(())
    }

    /// Stop a specific sound.
    pub fn stop_sound(&mut self, id: &str, fade_out_secs: f32) {
        if let Some(mut handle) = self.sounds.remove(id) {
            handle.stop(Tween {
                duration: std::time::Duration::from_secs_f32(fade_out_secs),
                ..Default::default()
            });
        }
    }

    /// Stop music.
    pub fn stop_music(&mut self, fade_out_secs: f32) {
        if let Some(mut music) = self.music.take() {
            music.stop(Tween {
                duration: std::time::Duration::from_secs_f32(fade_out_secs),
                ..Default::default()
            });
        }
    }

    /// Set master volume (0.0 to 1.0).
    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
    }

    /// Calculate spatial volume based on distance.
    pub fn spatial_volume(&self, source_pos: Vec3, max_distance: f32, base_volume: f32) -> f32 {
        let dist = self.listener_pos.distance(source_pos);
        if dist >= max_distance {
            0.0
        } else {
            let attenuation = 1.0 - (dist / max_distance);
            base_volume * attenuation * attenuation * self.master_volume
        }
    }

    /// Clean up finished sounds.
    pub fn cleanup(&mut self) {
        self.sounds.retain(|_, handle| {
            handle.state() != PlaybackState::Stopped
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_system_creation() {
        // Audio might fail in test environment (no audio device), that's OK
        let audio = AudioSystem::new();
        assert_eq!(audio.master_volume, 1.0);
    }

    #[test]
    fn test_spatial_volume() {
        let audio = AudioSystem::new();
        // At position, full volume
        let vol = audio.spatial_volume(Vec3::ZERO, 10.0, 1.0);
        assert!((vol - 1.0).abs() < 0.01);

        // At max distance, zero volume
        let vol = audio.spatial_volume(Vec3::new(10.0, 0.0, 0.0), 10.0, 1.0);
        assert!((vol - 0.0).abs() < 0.01);

        // Half distance
        let vol = audio.spatial_volume(Vec3::new(5.0, 0.0, 0.0), 10.0, 1.0);
        assert!(vol > 0.0 && vol < 1.0);
    }
}
