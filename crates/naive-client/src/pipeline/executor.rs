use std::collections::HashMap;

use crate::camera::CameraState;
use crate::components::{
    Animator, DirectionalLight, GaussianSplat, Hidden, MaterialOverride, MeshHandle, MeshRenderer,
    PointLight, Transform,
};
use crate::gpu_timer::GpuTimer;
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::renderer::{DrawUniformPool, DrawUniforms, GpuState, DRAW_UNIFORM_SIZE};
use crate::splat::SplatCache;
use crate::swarm::SwarmDraw;
use crate::texture_cache::TextureHandle;
use crate::world::SceneWorld;

use super::resource::{LightingUniforms, PassType, PointLightUniform, ShadowUniforms, MAX_LIGHTS};
use super::{CompiledPass, CompiledPipeline, RenderDebugState};

// ---------------------------------------------------------------------------
// Pipeline executor
// ---------------------------------------------------------------------------

/// Execute the compiled pipeline for one frame.
#[allow(clippy::too_many_arguments)]
pub fn execute_pipeline(
    gpu: &GpuState,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    splat_cache: &SplatCache,
    debug: &RenderDebugState,
    texture_resources: Option<&crate::mesh::TextureResources>,
    bone_palettes: &HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,
    texture_cache: Option<&crate::texture_cache::TextureCache>,
    swarms: &[SwarmDraw],
) {
    let output = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
            gpu.surface.configure(&gpu.device, &gpu.config);
            return;
        }
        // Minimized, hidden or slow to present: skip this frame.
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
        wgpu::CurrentSurfaceTexture::Validation => {
            tracing::error!("Surface validation error while acquiring a frame");
            return;
        }
    };

    let swapchain_view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let encoder = execute_pipeline_to_view(
        gpu,
        compiled,
        scene_world,
        camera_state,
        draw_pool,
        mesh_cache,
        material_cache,
        splat_cache,
        &swapchain_view,
        debug,
        texture_resources,
        bone_palettes,
        texture_cache,
        swarms,
        None,
    );

    gpu.queue.submit(std::iter::once(encoder.finish()));
    gpu.queue.present(output);
}

/// Execute the compiled multi-pass pipeline, returning the encoder for further passes.
#[allow(clippy::too_many_arguments)]
pub fn execute_pipeline_to_view(
    gpu: &GpuState,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    splat_cache: &SplatCache,
    swapchain_view: &wgpu::TextureView,
    debug: &RenderDebugState,
    texture_resources: Option<&crate::mesh::TextureResources>,
    bone_palettes: &HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,
    texture_cache: Option<&crate::texture_cache::TextureCache>,
    swarms: &[SwarmDraw],
    timer: Option<&GpuTimer>,
) -> wgpu::CommandEncoder {
    // Per-entity draw data for both mesh passes, uploaded once in batch order.
    let frustum = debug.frustum_culling.then(|| {
        crate::culling::Frustum::from_view_projection(&glam::Mat4::from_cols_array_2d(
            &camera_state.uniform.view_projection,
        ))
    });
    let draws = collect_draws(
        scene_world,
        mesh_cache,
        material_cache,
        debug,
        bone_palettes,
        draw_pool.capacity,
        frustum.as_ref(),
    );
    upload_draws(gpu, draw_pool, &draws);
    upload_skin_palette(gpu, compiled, draws.skin_palette);

    // Upload light uniforms (point lights + directional light)
    let mut light_data = LightingUniforms::default();
    if debug.point_lights_enabled {
        for (_entity, (transform, light)) in scene_world
            .world
            .query::<(hecs::Entity, (&Transform, &PointLight))>()
            .iter()
        {
            if (light_data.light_count as usize) < MAX_LIGHTS {
                let idx = light_data.light_count as usize;
                let base_intensity = if debug.torch_flicker_enabled {
                    light.intensity
                } else {
                    1.5 // base_intensity, ignoring flicker script
                };
                light_data.lights[idx] = PointLightUniform {
                    position: transform.position.to_array(),
                    range: light.range,
                    color: light.color.to_array(),
                    intensity: base_intensity * debug.light_intensity_mult,
                };
                light_data.light_count += 1;
            }
        }
    }

    // Query directional light and compute shadow VP matrix
    let mut light_vp = glam::Mat4::IDENTITY;
    // Only one directional light is supported, so take the first.
    if let Some((_entity, dir_light)) = scene_world
        .world
        .query::<(hecs::Entity, &DirectionalLight)>()
        .iter()
        .next()
    {
        light_data.has_directional = 1;
        light_data.dir_light_direction = dir_light.direction.to_array();
        light_data.dir_light_intensity = dir_light.intensity;
        light_data.dir_light_color = dir_light.color.to_array();

        light_vp = directional_shadow_view_projection(dir_light.direction, dir_light.shadow_extent);
        light_data.light_vp = light_vp.to_cols_array_2d();
    }

    // Debug: inject a directional light for ambient override
    if debug.ambient_override > 0.0 && light_data.has_directional == 0 {
        light_data.has_directional = 1;
        light_data.dir_light_direction = [0.0, -1.0, 0.0]; // straight down
        light_data.dir_light_intensity = debug.ambient_override;
        light_data.dir_light_color = [1.0, 1.0, 1.0];
    }

    gpu.queue.write_buffer(
        &compiled.light_buffer,
        0,
        bytemuck::cast_slice(&[light_data]),
    );

    // Upload shadow uniform buffer (light VP matrix for shadow pass)
    if let Some(shadow_buf) = &compiled.shadow_uniform_buffer {
        let shadow_data = ShadowUniforms {
            light_view_projection: light_vp.to_cols_array_2d(),
        };
        gpu.queue
            .write_buffer(shadow_buf, 0, bytemuck::cast_slice(&[shadow_data]));
    }

    // Create command encoder
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Pipeline Render Encoder"),
        });

    // Execute passes in topological order (skip passes disabled by debug state)
    for &pass_idx in &compiled.pass_order {
        let pass = &compiled.passes[pass_idx];

        // Skip passes disabled via debug toggles
        if !debug.bloom_enabled && pass.name == "bloom_pass" {
            // Clear bloom buffer to black so tonemap reads zero bloom
            if let Some(resource) = compiled.resources.get("bloom_buffer") {
                let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("bloom_clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &resource.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: timer.and_then(|t| t.render_pass(&pass.name)),
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                // Drop immediately ends the pass
            }
            continue;
        }
        match pass.pass_type {
            PassType::Rasterize => {
                execute_rasterize_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    camera_state,
                    draw_pool,
                    &draws,
                    swarms,
                    mesh_cache,
                    texture_resources,
                    texture_cache,
                    timer,
                );
            }
            PassType::Fullscreen => {
                execute_fullscreen_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    camera_state,
                    swapchain_view,
                    timer,
                );
            }
            PassType::Splat => {
                execute_splat_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    &gpu.device,
                    scene_world,
                    camera_state,
                    splat_cache,
                    timer,
                );
            }
            PassType::Shadow => {
                execute_shadow_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    draw_pool,
                    &draws,
                    swarms,
                    mesh_cache,
                    timer,
                );
            }
            PassType::Compute => {
                // Not implemented yet
            }
        }
    }

    encoder
}

/// The directional light's shadow camera: an orthographic box around the world
/// origin that reaches `extent` from it in every direction, sideways and along
/// the light. The light sits `extent` back from the origin and the far plane
/// is `2 * extent` away, so the depth range grows with the scene instead of
/// cutting it off at a fixed distance.
fn directional_shadow_view_projection(direction: glam::Vec3, extent: f32) -> glam::Mat4 {
    let dir = direction.try_normalize().unwrap_or(glam::Vec3::NEG_Y);
    let extent = extent.max(0.01);
    // look_at needs an up vector that is not parallel to the view direction.
    let up = if dir.y.abs() > 0.99 {
        glam::Vec3::Z
    } else {
        glam::Vec3::Y
    };
    let light_view = glam::camera::rh::view::look_at_mat4(-dir * extent, glam::Vec3::ZERO, up);
    let light_proj = glam::camera::rh::proj::directx::orthographic(
        -extent,
        extent,
        -extent,
        extent,
        0.0,
        2.0 * extent,
    );
    light_proj * light_view
}

/// Entities that share a mesh and an albedo texture. Their draw data is
/// contiguous in the draw buffer, so one instanced draw covers them all.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    mesh: usize,
    albedo_texture: Option<usize>,
}

struct DrawBatch {
    key: BatchKey,
    /// Index of the batch's first entry in the draw buffer.
    first: u32,
    count: u32,
    /// How many of the batch's entries, from `first`, the camera can see.
    /// Those come first, so the rest of the batch is outside the view.
    visible: u32,
}

impl DrawBatch {
    fn mesh(&self) -> MeshHandle {
        MeshHandle(self.key.mesh)
    }

    /// Every entity in the batch: what the shadow pass draws, since an
    /// entity out of view can cast a shadow into it.
    fn instances(&self) -> std::ops::Range<u32> {
        self.first..self.first + self.count
    }

    /// The entities the camera can see: what the camera passes draw.
    fn visible_instances(&self) -> std::ops::Range<u32> {
        self.first..self.first + self.visible
    }
}

/// Everything the mesh passes draw in one frame.
struct FrameDraws<'a> {
    /// Draw data for each visible entity, in the order the world listed them.
    uniforms: Vec<DrawUniforms>,
    /// Indices into `uniforms` in draw-buffer order: grouped into batches, and
    /// in world order within a batch.
    order: Vec<u32>,
    batches: Vec<DrawBatch>,
    /// Entities outside the camera's view this frame.
    culled: usize,
    /// The bone palette every skinned entity is drawn with this frame.
    skin_palette: Option<&'a crate::anim_system::BoneMatrixPalette>,
}

/// Gather the draw data for every visible mesh entity and group the entities
/// into batches that share a mesh and an albedo texture.
fn collect_draws<'a>(
    scene_world: &SceneWorld,
    mesh_cache: &MeshCache,
    material_cache: &MaterialCache,
    debug: &RenderDebugState,
    bone_palettes: &'a HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,
    capacity: u32,
    frustum: Option<&crate::culling::Frustum>,
) -> FrameDraws<'a> {
    let mut uniforms = Vec::new();
    let mut keys = Vec::new();
    // Per entity: outside the camera's view.
    let mut outside = Vec::new();
    let mut skin_palette = None;

    let mut query = scene_world.world.query::<(
        hecs::Entity,
        (
            &Transform,
            &MeshRenderer,
            Option<&MaterialOverride>,
            Option<&Animator>,
            Option<&Hidden>,
        ),
    )>();
    for (entity, (transform, mesh_renderer, mat_override, animator, hidden)) in query.iter() {
        if hidden.is_some() {
            continue;
        }
        let material = material_cache.get(mesh_renderer.material_handle);
        let model_matrix = transform.world_matrix;
        let normal_matrix = model_matrix.inverse().transpose();

        // Apply runtime material overrides from Lua scripts
        let roughness = mat_override
            .and_then(|o| o.roughness)
            .unwrap_or(material.uniform.roughness);
        let metallic = mat_override
            .and_then(|o| o.metallic)
            .unwrap_or(material.uniform.metallic);
        let emission = if debug.emission_enabled {
            mat_override
                .and_then(|o| o.emission)
                .map(|e| [e[0], e[1], e[2], 0.0])
                .unwrap_or(material.uniform.emission)
        } else {
            [0.0; 4]
        };
        let base_color = mat_override
            .and_then(|o| o.base_color)
            .map(|c| [c[0], c[1], c[2], material.uniform.base_color[3]])
            .unwrap_or(material.uniform.base_color);

        let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
        let has_texture =
            material.albedo_texture.is_some() || gpu_mesh.texture_bind_group.is_some();

        // Entities with skeletal animation. All of them read the one skin
        // buffer, so they share the palette of the last one found.
        let has_skin = animator.is_some();
        if has_skin {
            skin_palette = bone_palettes.get(&entity);
        }

        uniforms.push(DrawUniforms {
            model_matrix: model_matrix.to_cols_array_2d(),
            normal_matrix: normal_matrix.to_cols_array_2d(),
            base_color,
            roughness,
            metallic,
            has_texture: if has_texture { 1.0 } else { 0.0 },
            has_skin: if has_skin { 1.0 } else { 0.0 },
            emission,
            _padding: [0.0; 20],
        });
        keys.push(BatchKey {
            mesh: mesh_renderer.mesh_handle.0,
            albedo_texture: material.albedo_texture.map(|t| t.0),
        });
        // A skinned pose can reach outside the mesh's rest-pose bounds, so
        // skinned entities are always drawn.
        outside
            .push(!has_skin && frustum.is_some_and(|f| !f.sees(&gpu_mesh.bounds, &model_matrix)));
    }

    // A stable sort keeps world order inside each batch, so overlapping
    // surfaces resolve the same way every frame. Within a batch, entities in
    // view come first, so the camera passes can draw just those.
    let mut order: Vec<u32> = (0..uniforms.len() as u32).collect();
    order.sort_by_key(|&i| (keys[i as usize], outside[i as usize]));

    // The engine grows the draw buffer before rendering. If entities appeared
    // since then (a capture taken in the same frame as a spawn), draw what fits.
    if order.len() > capacity as usize {
        tracing::warn!(
            "{} mesh entities but room for {}; drawing {} this frame",
            order.len(),
            capacity,
            capacity
        );
        order.truncate(capacity as usize);
    }

    let mut batches: Vec<DrawBatch> = Vec::new();
    let mut culled = 0;
    for (slot, &i) in order.iter().enumerate() {
        let key = keys[i as usize];
        let visible = u32::from(!outside[i as usize]);
        culled += 1 - visible as usize;
        match batches.last_mut() {
            Some(batch) if batch.key == key => {
                batch.count += 1;
                batch.visible += visible;
            }
            _ => batches.push(DrawBatch {
                key,
                first: slot as u32,
                count: 1,
                visible,
            }),
        }
    }

    FrameDraws {
        uniforms,
        order,
        batches,
        culled,
        skin_palette,
    }
}

/// Write the frame's draw data into the draw buffer in batch order, in one
/// upload.
fn upload_draws(gpu: &GpuState, draw_pool: &DrawUniformPool, draws: &FrameDraws) {
    let Some(size) = wgpu::BufferSize::new(draws.order.len() as u64 * DRAW_UNIFORM_SIZE) else {
        return;
    };
    let Some(mut staging) = gpu.queue.write_buffer_with(&draw_pool.buffer, 0, size) else {
        return;
    };
    let stride = DRAW_UNIFORM_SIZE as usize;
    for (slot, &i) in draws.order.iter().enumerate() {
        staging
            .slice(slot * stride..(slot + 1) * stride)
            .copy_from_slice(bytemuck::bytes_of(&draws.uniforms[i as usize]));
    }
}

/// Execute a shadow depth pass (renders all geometry from light's perspective).
#[allow(clippy::too_many_arguments)]
fn execute_shadow_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    draw_pool: &DrawUniformPool,
    draws: &FrameDraws,
    swarms: &[SwarmDraw],
    mesh_cache: &MeshCache,
    timer: Option<&GpuTimer>,
) {
    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view)
        .expect("Shadow pass has no depth target");

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timer.and_then(|t| t.render_pass(&pass.name)),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        render_pass.set_pipeline(&pass.pipeline);

        // Group 0: shadow uniforms (light VP matrix)
        if let Some(bg) = &compiled.shadow_bind_group {
            render_pass.set_bind_group(0, bg, &[]);
        }

        // Group 1: every entity's draw data; group 2: bone matrices.
        render_pass.set_bind_group(1, &draw_pool.storage_bind_group, &[]);
        if let Some(skin_bg) = &compiled.skin_bind_group {
            render_pass.set_bind_group(2, skin_bg, &[]);
        }

        for batch in &draws.batches {
            let gpu_mesh = mesh_cache.get(batch.mesh());
            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, batch.instances());
        }

        // Swarms: the same shader, reading each swarm's own draw data.
        for swarm in swarms {
            let gpu_mesh = mesh_cache.get(swarm.mesh);
            render_pass.set_bind_group(1, swarm.draws, &[]);
            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..swarm.count);
        }
    }
}

/// The albedo texture to bind for a draw: the material's texture, else the
/// mesh's own, else plain white.
fn albedo_bind_group<'a>(
    texture_resources: &'a crate::mesh::TextureResources,
    texture_cache: Option<&'a crate::texture_cache::TextureCache>,
    albedo_texture: Option<TextureHandle>,
    gpu_mesh: &'a crate::mesh::GpuMesh,
) -> &'a wgpu::BindGroup {
    if let (Some(albedo), Some(tc)) = (albedo_texture, texture_cache) {
        tc.get(albedo)
    } else if let Some(mesh_tex) = gpu_mesh.texture_bind_group.as_ref() {
        mesh_tex
    } else {
        &texture_resources.default_bind_group
    }
}

/// Upload the bone matrices for the frame, once.
///
/// Every draw reads the same skin buffer, and buffer writes all land before
/// the passes run, so there is one palette per frame: the one `collect_draws`
/// picked. The shaders only apply it to entities with an animator. With no
/// palette, upload the identity one, which turns skinning off.
fn upload_skin_palette(
    gpu: &GpuState,
    compiled: &CompiledPipeline,
    palette: Option<&crate::anim_system::BoneMatrixPalette>,
) {
    let Some(skin_buffer) = &compiled.skin_buffer else {
        return;
    };
    match palette {
        Some(palette) => gpu
            .queue
            .write_buffer(skin_buffer, 0, bytemuck::bytes_of(palette)),
        None => {
            let identity = crate::anim_system::BoneMatrixPalette::default();
            gpu.queue
                .write_buffer(skin_buffer, 0, bytemuck::bytes_of(&identity));
        }
    }
}

/// Execute a rasterize pass (G-buffer geometry pass).
#[allow(clippy::too_many_arguments)]
fn execute_rasterize_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    draws: &FrameDraws,
    swarms: &[SwarmDraw],
    mesh_cache: &MeshCache,
    texture_resources: Option<&crate::mesh::TextureResources>,
    texture_cache: Option<&crate::texture_cache::TextureCache>,
    timer: Option<&GpuTimer>,
) {
    // Build color attachments from pass targets
    let color_views: Vec<&wgpu::TextureView> = pass
        .color_targets
        .iter()
        .filter_map(|name| compiled.resources.get(name).map(|r| &r.view))
        .collect();

    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
        .iter()
        .map(|view| {
            Some(wgpu::RenderPassColorAttachment {
                depth_slice: None,
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();

    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view);

    let depth_attachment = depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    });

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: timer.and_then(|t| t.render_pass(&pass.name)),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);
        // Group 1: every entity's draw data; group 3: bone matrices.
        render_pass.set_bind_group(1, &draw_pool.storage_bind_group, &[]);
        if let Some(skin_bg) = &compiled.skin_bind_group {
            render_pass.set_bind_group(3, skin_bg, &[]);
        }

        for batch in draws.batches.iter().filter(|batch| batch.visible > 0) {
            let gpu_mesh = mesh_cache.get(batch.mesh());

            // Bind texture at group(2): material texture > mesh texture > white fallback
            if let Some(tex_res) = texture_resources {
                let albedo = batch.key.albedo_texture.map(TextureHandle);
                let tex_bg = albedo_bind_group(tex_res, texture_cache, albedo, gpu_mesh);
                render_pass.set_bind_group(2, tex_bg, &[]);
            }

            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, batch.visible_instances());
        }

        // Swarms: the same shader, reading each swarm's own draw data.
        for swarm in swarms {
            let gpu_mesh = mesh_cache.get(swarm.mesh);
            render_pass.set_bind_group(1, swarm.draws, &[]);
            if let Some(tex_res) = texture_resources {
                let tex_bg =
                    albedo_bind_group(tex_res, texture_cache, swarm.albedo_texture, gpu_mesh);
                render_pass.set_bind_group(2, tex_bg, &[]);
            }
            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass
                .set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..swarm.count);
        }

        if draws.order.is_empty() && swarms.is_empty() {
            tracing::warn!("Rasterize pass '{}': ZERO entities drawn!", pass.name);
        } else {
            tracing::debug!(
                "Rasterize pass '{}': {} entities ({} outside the view) in {} draws, {} swarms",
                pass.name,
                draws.order.len(),
                draws.culled,
                draws.batches.len(),
                swarms.len()
            );
        }
    }
}

/// Execute a Gaussian splat rendering pass.
#[allow(clippy::too_many_arguments)]
fn execute_splat_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    device: &wgpu::Device,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    splat_cache: &SplatCache,
    timer: Option<&GpuTimer>,
) {
    // Build color attachments
    let color_views: Vec<&wgpu::TextureView> = pass
        .color_targets
        .iter()
        .filter_map(|name| compiled.resources.get(name).map(|r| &r.view))
        .collect();

    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
        .iter()
        .map(|view| {
            Some(wgpu::RenderPassColorAttachment {
                depth_slice: None,
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();

    let depth_view = pass
        .depth_target
        .as_ref()
        .and_then(|name| compiled.resources.get(name))
        .map(|r| &r.view);

    let depth_attachment = depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    });

    // Get the splat data bind group layout
    let splat_layout = match &compiled.splat_data_bind_group_layout {
        Some(layout) => layout,
        None => return,
    };

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: timer.and_then(|t| t.render_pass(&pass.name)),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        // For each entity with a GaussianSplat component, create a bind group and draw
        for (_entity, splat) in scene_world
            .world
            .query::<(hecs::Entity, &GaussianSplat)>()
            .iter()
        {
            let gpu_splat = splat_cache.get(splat.splat_handle);
            if gpu_splat.splat_count == 0 {
                continue;
            }

            // Create bind group for this splat's data
            let splat_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Splat Data Bind Group"),
                layout: splat_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: gpu_splat.splat_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: gpu_splat.sorted_index_buffer.as_entire_binding(),
                    },
                ],
            });

            render_pass.set_bind_group(1, &splat_bind_group, &[]);
            // 6 vertices per quad, N instances (one per splat)
            render_pass.draw(0..6, 0..gpu_splat.splat_count);
        }
    }
}

/// Execute a fullscreen pass (lighting or tonemap).
fn execute_fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    camera_state: &CameraState,
    swapchain_view: &wgpu::TextureView,
    timer: Option<&GpuTimer>,
) {
    let is_tonemap = pass.name.contains("tonemap");
    let is_bloom = pass.name.contains("bloom");
    let is_fxaa = pass.name.contains("fxaa");
    let writes_to_swapchain = pass.color_targets.iter().any(|t| t == "swapchain");

    // Determine the output view
    let output_view = if writes_to_swapchain {
        swapchain_view
    } else {
        pass.color_targets
            .first()
            .and_then(|name| compiled.resources.get(name))
            .map(|r| &r.view)
            .expect("Fullscreen pass has no output target")
    };

    {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&pass.name),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timer.and_then(|t| t.render_pass(&pass.name)),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        render_pass.set_pipeline(&pass.pipeline);

        if is_fxaa {
            // FXAA: group 0 = LDR texture + sampler
            if let Some(bg) = &compiled.fxaa_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else if is_tonemap {
            // Tonemap: group 0 = HDR texture + sampler + bloom texture
            if let Some(bg) = &compiled.tonemap_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else if is_bloom {
            // Bloom: group 0 = HDR texture + sampler
            if let Some(bg) = &compiled.bloom_bind_group {
                render_pass.set_bind_group(0, bg, &[]);
            }
        } else {
            // Lighting: group 0 = camera, group 1 = G-buffer textures, group 2 = lights
            render_pass.set_bind_group(0, &camera_state.bind_group, &[]);
            if let Some(bg) = &compiled.gbuffer_bind_group {
                render_pass.set_bind_group(1, bg, &[]);
            }
            render_pass.set_bind_group(2, &compiled.light_bind_group, &[]);
            // Group 3: splat composite textures (if available)
            if let Some(bg) = &compiled.splat_composite_bind_group {
                render_pass.set_bind_group(3, bg, &[]);
            }
        }

        // Draw fullscreen triangle (3 vertices, no vertex buffer)
        render_pass.draw(0..3, 0..1);
    }
}

/// Rebuild bind groups after resources are resized.
/// Call this after `resize_resources()` to update texture view references.
pub fn rebuild_bind_groups(device: &wgpu::Device, compiled: &mut CompiledPipeline) {
    // Rebuild G-buffer bind group
    if let Some(layout) = &compiled.gbuffer_bind_group_layout {
        let albedo_view = compiled.resources.get("gbuffer_albedo").map(|r| &r.view);
        let normal_view = compiled.resources.get("gbuffer_normal").map(|r| &r.view);
        let depth_view = compiled.resources.get("gbuffer_depth").map(|r| &r.view);
        let emission_view = compiled.resources.get("gbuffer_emission").map(|r| &r.view);

        if let (Some(albedo), Some(normal), Some(depth)) = (albedo_view, normal_view, depth_view) {
            let emission = emission_view.unwrap_or(albedo);
            compiled.gbuffer_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("GBuffer Input Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(albedo),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(normal),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(depth),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&compiled.gbuffer_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(emission),
                        },
                    ],
                }));
        }
    }

    // Rebuild bloom bind group
    if let Some(layout) = &compiled.bloom_bind_group_layout {
        if let Some(hdr_view) = compiled.resources.get("hdr_buffer").map(|r| &r.view) {
            let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Bloom HDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            compiled.bloom_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Bloom Input Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(hdr_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&hdr_sampler),
                        },
                    ],
                }));
        }
    }

    // Rebuild tonemap bind group (HDR + bloom)
    if let Some(layout) = &compiled.tonemap_bind_group_layout {
        if let Some(hdr_view) = compiled.resources.get("hdr_buffer").map(|r| &r.view) {
            let hdr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("HDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            let bloom_view = compiled
                .resources
                .get("bloom_buffer")
                .map(|r| &r.view)
                .unwrap_or(hdr_view);

            compiled.tonemap_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Tonemap Input Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(hdr_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&hdr_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(bloom_view),
                        },
                    ],
                }));
        }
    }

    // Rebuild FXAA bind group
    if let Some(layout) = &compiled.fxaa_bind_group_layout {
        if let Some(ldr_view) = compiled.resources.get("ldr_buffer").map(|r| &r.view) {
            let ldr_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("FXAA LDR Sampler (resized)"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            compiled.fxaa_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("FXAA Input Bind Group (resized)"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(ldr_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&ldr_sampler),
                    },
                ],
            }));
        }
    }

    // Rebuild splat composite bind group
    if let Some(layout) = &compiled.splat_composite_bind_group_layout {
        let splat_color = compiled.resources.get("splat_color").map(|r| &r.view);
        let splat_depth = compiled.resources.get("splat_depth").map(|r| &r.view);

        if let (Some(color_view), Some(depth_view)) = (splat_color, splat_depth) {
            compiled.splat_composite_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Splat Composite Bind Group (resized)"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(color_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(depth_view),
                        },
                    ],
                }));
        }
    }

    // Rebuild lighting bind group (shadow map may have been resized)
    if let Some(sampler) = &compiled.shadow_sampler {
        // Create dummy shadow map fallback
        let shadow_dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Dummy Shadow Map (resized)"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_dummy_view = shadow_dummy_tex.create_view(&Default::default());
        let shadow_map_view = compiled
            .resources
            .get("shadow_map")
            .map(|r| &r.view)
            .unwrap_or(&shadow_dummy_view);

        compiled.light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lighting Bind Group (resized)"),
            layout: &compiled.light_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: compiled.light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(shadow_map_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec3, Vec3Swizzles, Vec4Swizzles};

    /// Where a world point lands in the shadow map: xy in [-1, 1], depth in [0, 1].
    fn to_shadow_ndc(vp: glam::Mat4, p: Vec3) -> Vec3 {
        let clip = vp * p.extend(1.0);
        clip.xyz() / clip.w
    }

    #[test]
    fn shadow_depth_range_covers_the_extent_along_the_light() {
        let dir = Vec3::new(0.35, -1.0, 0.25);
        let extent = 140.0;
        let vp = directional_shadow_view_projection(dir, extent);
        let n = dir.normalize();

        let origin = to_shadow_ndc(vp, Vec3::ZERO);
        assert!(origin.xy().length() < 1e-4);
        assert!((origin.z - 0.5).abs() < 1e-4);

        // Points `extent` toward and away from the light sit on the near and
        // far planes, so nothing inside the box is cut off.
        assert!(to_shadow_ndc(vp, -n * extent).z.abs() < 1e-4);
        assert!((to_shadow_ndc(vp, n * extent).z - 1.0).abs() < 1e-4);

        // A floor corner well inside the box: inside the map on every axis.
        let corner = to_shadow_ndc(vp, Vec3::new(90.0, 0.0, 90.0));
        assert!(corner.x.abs() < 1.0 && corner.y.abs() < 1.0);
        assert!(corner.z > 0.0 && corner.z < 1.0);
    }

    #[test]
    fn shadow_camera_handles_a_light_pointing_straight_down() {
        let vp = directional_shadow_view_projection(Vec3::NEG_Y, 20.0);
        assert!(vp.is_finite());
        let below = to_shadow_ndc(vp, Vec3::new(0.0, -10.0, 0.0));
        assert!((below.z - 0.75).abs() < 1e-4);
    }
}
