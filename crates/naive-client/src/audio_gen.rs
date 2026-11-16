use std::path::Path;

/// Generate default sound files if they don't already exist.
pub fn generate_default_sounds(project_root: &Path) {
    for (relative, generate) in [
        (
            "assets/audio/collision.wav",
            generate_collision_wav as fn() -> Result<Vec<u8>, String>,
        ),
        ("assets/audio/cosmic_ambient.wav", generate_ambient_wav),
    ] {
        if let Err(error) = generate_sound(project_root, relative, generate) {
            tracing::error!("Failed to generate {relative}: {error}");
        }
    }
}

fn generate_sound(
    project_root: &Path,
    relative: &str,
    generate: fn() -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    use std::io::Write;
    let path = crate::project_path::resolve(project_root, relative)?;
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = generate()?;
    // Exclusive creation also refuses a final symlink if one appears after
    // validation, and never overwrites a user's existing audio file.
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    file.write_all(&data).map_err(|e| e.to_string())?;
    tracing::info!("Generated {relative}");
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn default_sounds_cannot_write_through_directory_or_dangling_file_symlinks() {
        let parent = std::env::temp_dir().join(format!("naive-audio-paths-{}", std::process::id()));
        let project = parent.join("project");
        let outside = parent.join("outside");
        std::fs::create_dir_all(project.join("assets")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("assets/audio")).unwrap();
        super::generate_default_sounds(&project);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        std::fs::remove_file(project.join("assets/audio")).unwrap();
        std::fs::create_dir(project.join("assets/audio")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("collision.wav"),
            project.join("assets/audio/collision.wav"),
        )
        .unwrap();
        super::generate_default_sounds(&project);
        assert!(!outside.join("collision.wav").exists());
        assert!(project.join("assets/audio/cosmic_ambient.wav").exists());
        std::fs::remove_dir_all(parent).unwrap();
    }
}

/// Generate a short impact thud WAV (mono, 44100 Hz, 16-bit PCM).
/// ~0.15s: low-frequency sine burst (200 Hz) with fast exponential decay + noise.
fn generate_collision_wav() -> Result<Vec<u8>, String> {
    let sample_rate = 44100u32;
    let duration_secs = 0.15f32;
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        // Fast exponential decay
        let envelope = (-t * 30.0).exp();
        // Low sine burst at 200 Hz
        let sine = (2.0 * std::f32::consts::PI * 200.0 * t).sin();
        // Add some noise for texture
        let noise = simple_noise(i as u32) * 0.3;
        let sample = (sine * 0.7 + noise) * envelope;
        let clamped = sample.clamp(-1.0, 1.0);
        samples.push((clamped * 32767.0) as i16);
    }

    Ok(write_wav_mono(sample_rate, &samples))
}

/// Generate a loopable ambient pad WAV (mono, 44100 Hz, 16-bit PCM).
/// ~30s: layered sine waves with slow modulation (drone).
fn generate_ambient_wav() -> Result<Vec<u8>, String> {
    let sample_rate = 44100u32;
    let duration_secs = 30.0f32;
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    // Fade-in and fade-out duration for seamless looping
    let fade_samples = (sample_rate as f32 * 0.5) as usize;

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;

        // Layered drones at different frequencies
        let f1 = 55.0; // A1
        let f2 = 82.5; // E2
        let f3 = 110.0; // A2
        let f4 = 146.8; // D3

        // Slow modulation
        let mod1 = 1.0 + 0.3 * (0.1 * t * 2.0 * std::f32::consts::PI).sin();
        let mod2 = 1.0 + 0.2 * (0.07 * t * 2.0 * std::f32::consts::PI).sin();

        let s1 = (2.0 * std::f32::consts::PI * f1 * t).sin() * 0.3 * mod1;
        let s2 = (2.0 * std::f32::consts::PI * f2 * t).sin() * 0.25 * mod2;
        let s3 = (2.0 * std::f32::consts::PI * f3 * t).sin() * 0.2;
        let s4 = (2.0 * std::f32::consts::PI * f4 * t).sin() * 0.15;

        // Gentle noise bed
        let noise = simple_noise(i as u32) * 0.05;

        let mut sample = s1 + s2 + s3 + s4 + noise;

        // Fade envelope for seamless loop
        if i < fade_samples {
            sample *= i as f32 / fade_samples as f32;
        } else if i > num_samples - fade_samples {
            sample *= (num_samples - i) as f32 / fade_samples as f32;
        }

        let clamped = sample.clamp(-1.0, 1.0);
        samples.push((clamped * 32767.0) as i16);
    }

    Ok(write_wav_mono(sample_rate, &samples))
}

/// Simple deterministic pseudo-random noise in [-1, 1].
fn simple_noise(seed: u32) -> f32 {
    let x = seed.wrapping_mul(1103515245).wrapping_add(12345);
    let bits = (x >> 16) & 0x7FFF;
    (bits as f32 / 16383.5) - 1.0
}

/// Write mono 16-bit PCM WAV data.
fn write_wav_mono(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let num_channels = 1u16;
    let bits_per_sample = 16u16;
    let byte_rate = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
    let block_align = num_channels * bits_per_sample / 8;
    let data_size = (samples.len() * 2) as u32;
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(file_size as usize + 8);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt sub-chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&num_channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data sub-chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &sample in samples {
        buf.extend_from_slice(&sample.to_le_bytes());
    }

    buf
}
