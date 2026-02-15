use glam::{Vec3, Vec4};
use crate::components::{ParticleConfig, ParticleEmitter, Transform};
use crate::world::SceneWorld;

/// A single particle in the simulation.
struct Particle {
    position: Vec3,
    velocity: Vec3,
    color: Vec4,
    size: f32,
    lifetime: f32,
    age: f32,
}

/// A runtime emitter instance tied to an ECS entity.
struct EmitterInstance {
    owner_entity: hecs::Entity,
    config: ParticleConfig,
    particles: Vec<Particle>,
    spawn_accumulator: f32,
}

/// CPU-side particle simulation system.
pub struct ParticleSystem {
    emitters: Vec<EmitterInstance>,
    /// One-shot bursts not tied to any entity.
    orphan_particles: Vec<Particle>,
}

impl ParticleSystem {
    pub fn new() -> Self {
        Self {
            emitters: Vec::new(),
            orphan_particles: Vec::new(),
        }
    }

    /// Synchronize emitters with the ECS world. Adds new emitters, removes stale ones.
    pub fn sync_emitters(&mut self, scene_world: &SceneWorld) {
        // Collect entity IDs that currently have ParticleEmitter
        let mut active_entities: Vec<hecs::Entity> = Vec::new();
        for (entity, emitter) in scene_world.world.query::<&ParticleEmitter>().iter() {
            active_entities.push(entity);
            // Add new emitter if not already tracked
            if !self.emitters.iter().any(|e| e.owner_entity == entity) {
                self.emitters.push(EmitterInstance {
                    owner_entity: entity,
                    config: emitter.config.clone(),
                    particles: Vec::with_capacity(emitter.config.max_particles as usize),
                    spawn_accumulator: 0.0,
                });
            }
        }

        // Remove emitters for despawned entities
        self.emitters.retain(|e| active_entities.contains(&e.owner_entity));
    }

    /// Update all particles: spawn new, age existing, kill expired.
    pub fn update(&mut self, dt: f32, scene_world: &SceneWorld) {
        self.sync_emitters(scene_world);

        for emitter in &mut self.emitters {
            // Get owner position
            let owner_pos = scene_world.world.get::<&Transform>(emitter.owner_entity)
                .map(|t| t.position)
                .unwrap_or(Vec3::ZERO);

            // Check if emitter is enabled
            let enabled = scene_world.world.get::<&ParticleEmitter>(emitter.owner_entity)
                .map(|e| e.enabled)
                .unwrap_or(false);

            // Spawn new particles
            if enabled {
                emitter.spawn_accumulator += emitter.config.spawn_rate * dt;
                let to_spawn = emitter.spawn_accumulator as u32;
                emitter.spawn_accumulator -= to_spawn as f32;

                for _ in 0..to_spawn {
                    if emitter.particles.len() >= emitter.config.max_particles as usize {
                        break;
                    }
                    emitter.particles.push(spawn_particle(&emitter.config, owner_pos));
                }
            }

            // Update existing particles
            let gravity = Vec3::new(0.0, -9.81 * emitter.config.gravity_scale, 0.0);
            for particle in &mut emitter.particles {
                particle.velocity += gravity * dt;
                particle.position += particle.velocity * dt;
                particle.age += dt;

                // Interpolate color and size
                let t = (particle.age / particle.lifetime).clamp(0.0, 1.0);
                let start = Vec4::from(emitter.config.color_start);
                let end = Vec4::from(emitter.config.color_end);
                particle.color = start.lerp(end, t);
                let [size_start, size_end] = emitter.config.size;
                particle.size = size_start + (size_end - size_start) * t;
            }

            // Remove expired
            emitter.particles.retain(|p| p.age < p.lifetime);
        }

        // Update orphan particles
        for particle in &mut self.orphan_particles {
            particle.velocity += Vec3::new(0.0, -9.81, 0.0) * dt;
            particle.position += particle.velocity * dt;
            particle.age += dt;
            let t = (particle.age / particle.lifetime).clamp(0.0, 1.0);
            particle.color.w = 1.0 - t; // fade out
        }
        self.orphan_particles.retain(|p| p.age < p.lifetime);
    }

    /// Spawn a burst of particles at a world position (no entity).
    pub fn spawn_burst(&mut self, position: Vec3, count: u32, config: &ParticleConfig) {
        for _ in 0..count {
            self.orphan_particles.push(spawn_particle(config, position));
        }
    }

    /// Spawn a burst on an existing emitter entity.
    pub fn burst_on_entity(&mut self, entity: hecs::Entity, count: u32, scene_world: &SceneWorld) {
        let emitter = match self.emitters.iter_mut().find(|e| e.owner_entity == entity) {
            Some(e) => e,
            None => return,
        };
        let owner_pos = scene_world.world.get::<&Transform>(entity)
            .map(|t| t.position)
            .unwrap_or(Vec3::ZERO);

        for _ in 0..count {
            if emitter.particles.len() >= emitter.config.max_particles as usize {
                break;
            }
            emitter.particles.push(spawn_particle(&emitter.config, owner_pos));
        }
    }

    /// Collect all live particle data for rendering (position, size, color).
    /// Returns a flat Vec of billboard vertex data.
    pub fn collect_billboard_data(&self, camera_right: Vec3, camera_up: Vec3) -> Vec<ParticleBillboardVertex> {
        let mut vertices = Vec::new();

        let all_particles = self.emitters.iter()
            .flat_map(|e| e.particles.iter())
            .chain(self.orphan_particles.iter());

        for particle in all_particles {
            let half = particle.size * 0.5;
            let right = camera_right * half;
            let up = camera_up * half;

            let p = particle.position;
            let color = [particle.color.x, particle.color.y, particle.color.z, particle.color.w];

            // Quad: two triangles
            vertices.push(ParticleBillboardVertex { position: (p - right - up).to_array(), color });
            vertices.push(ParticleBillboardVertex { position: (p + right - up).to_array(), color });
            vertices.push(ParticleBillboardVertex { position: (p + right + up).to_array(), color });

            vertices.push(ParticleBillboardVertex { position: (p - right - up).to_array(), color });
            vertices.push(ParticleBillboardVertex { position: (p + right + up).to_array(), color });
            vertices.push(ParticleBillboardVertex { position: (p - right + up).to_array(), color });
        }

        vertices
    }

    /// Get total live particle count (for diagnostics).
    pub fn particle_count(&self) -> usize {
        self.emitters.iter().map(|e| e.particles.len()).sum::<usize>()
            + self.orphan_particles.len()
    }
}

/// Vertex data for a particle billboard quad.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleBillboardVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
}

impl ParticleBillboardVertex {
    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBS: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ParticleBillboardVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBS,
        }
    }
}

/// Spawn a single particle using the emitter config and random variation.
fn spawn_particle(config: &ParticleConfig, origin: Vec3) -> Particle {
    // Simple deterministic-ish variation using a basic hash
    let hash = (origin.x * 1000.0 + origin.y * 100.0 + origin.z * 10.0) as u32;
    let rand01 = || -> f32 {
        // Use a simple pseudo-random based on incremented state
        static SEED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let s = SEED.fetch_add(1, std::sync::atomic::Ordering::Relaxed).wrapping_add(hash);
        let s = s.wrapping_mul(2654435761); // Knuth's multiplicative hash
        (s as f32) / (u32::MAX as f32)
    };

    let lifetime = config.lifetime[0] + (config.lifetime[1] - config.lifetime[0]) * rand01();
    let speed = config.initial_speed[0] + (config.initial_speed[1] - config.initial_speed[0]) * rand01();

    // Random direction within cone
    let spread_rad = config.spread.to_radians();
    let theta = rand01() * std::f32::consts::TAU;
    let phi = rand01() * spread_rad;
    let sin_phi = phi.sin();

    let local_dir = Vec3::new(
        sin_phi * theta.cos(),
        phi.cos(),
        sin_phi * theta.sin(),
    ).normalize();

    // Rotate local_dir to align with config.direction
    let up = Vec3::Y;
    let target = config.direction.normalize_or_zero();
    let velocity = if (target - up).length() < 0.001 {
        local_dir * speed
    } else if (target + up).length() < 0.001 {
        Vec3::new(local_dir.x, -local_dir.y, local_dir.z) * speed
    } else {
        let rot = glam::Quat::from_rotation_arc(up, target);
        rot * local_dir * speed
    };

    Particle {
        position: origin,
        velocity,
        color: Vec4::from(config.color_start),
        size: config.size[0],
        lifetime,
        age: 0.0,
    }
}
