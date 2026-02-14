use std::path::Path;

/// Generate default sound files if they don't already exist.
pub fn generate_default_sounds(project_root: &Path) {
    let audio_dir = project_root.join("assets/audio");
    if !audio_dir.exists() {
        let _ = std::fs::create_dir_all(&audio_dir);
    }

    let collision_path = audio_dir.join("collision.wav");
    if !collision_path.exists() {
        match generate_collision_wav() {
            Ok(data) => {
                if let Err(e) = std::fs::write(&collision_path, &data) {
                    tracing::error!("Failed to write collision.wav: {}", e);
                } else {
                    tracing::info!("Generated collision.wav");
                }
            }
            Err(e) => tracing::error!("Failed to generate collision.wav: {}", e),
        }
    }

    let ambient_path = audio_dir.join("cosmic_ambient.wav");
    if !ambient_path.exists() {
        match generate_ambient_wav() {
            Ok(data) => {
                if let Err(e) = std::fs::write(&ambient_path, &data) {
                    tracing::error!("Failed to write cosmic_ambient.wav: {}", e);
                } else {
                    tracing::info!("Generated cosmic_ambient.wav");
                }
            }
            Err(e) => tracing::error!("Failed to generate cosmic_ambient.wav: {}", e),
        }
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
