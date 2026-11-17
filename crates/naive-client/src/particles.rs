//! GPU adapter for the shared CPU particle simulation.
use glam::Vec3;

#[derive(Default)]
pub struct ParticleSystem(naive_sim::particles::ParticleSystem);
impl ParticleSystem {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn collect_billboard_data(&self, right: Vec3, up: Vec3) -> Vec<ParticleBillboardVertex> {
        self.0
            .collect_billboard_data(right, up)
            .into_iter()
            .map(|v| ParticleBillboardVertex {
                position: v.position,
                color: v.color,
            })
            .collect()
    }
}
impl std::ops::Deref for ParticleSystem {
    type Target = naive_sim::particles::ParticleSystem;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ParticleSystem {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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
