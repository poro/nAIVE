use std::collections::HashMap;

use crate::camera::CameraState;
use crate::components::{DirectionalLight, GaussianSplat, Hidden, MaterialOverride, MeshRenderer, PointLight, Transform};
use crate::material::MaterialCache;
use crate::mesh::MeshCache;
use crate::renderer::{DrawUniformPool, DrawUniforms, GpuState, DRAW_UNIFORM_SIZE};
use crate::splat::SplatCache;
use crate::world::SceneWorld;

use super::resource::{LightingUniforms, PointLightUniform, ShadowUniforms, MAX_LIGHTS, PassType};
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
) {
    let output = match gpu.surface.get_current_texture() {
        Ok(t) => t,
        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
            gpu.surface.configure(&gpu.device, &gpu.config);
            return;
        }
        Err(e) => {
            tracing::error!("Surface error: {:?}", e);
            return;
        }
    };

    let swapchain_view = output
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let encoder = execute_pipeline_to_view(
        gpu, compiled, scene_world, camera_state, draw_pool,
        mesh_cache, material_cache, splat_cache, &swapchain_view, debug,
        texture_resources, bone_palettes,
    );

    gpu.queue.submit(std::iter::once(encoder.finish()));
    output.present();
}

/// Execute the compiled multi-pass pipeline, returning the encoder for further passes.
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
) -> wgpu::CommandEncoder {

    // Upload per-entity draw uniforms (skip hidden entities before incrementing draw_index)
    let mut draw_index = 0u32;
    for (entity, (transform, mesh_renderer)) in
        scene_world.world.query::<(&Transform, &MeshRenderer)>().iter()
    {
        if scene_world.world.get::<&Hidden>(entity).is_ok() {
            continue;
        }
        let material = material_cache.get(mesh_renderer.material_handle);
        let model_matrix = transform.world_matrix;
        let normal_matrix = model_matrix.inverse().transpose();

        // Apply runtime material overrides from Lua scripts
        let mat_override = scene_world.world.get::<&MaterialOverride>(entity).ok();
        let roughness = mat_override
            .as_ref()
            .and_then(|o| o.roughness)
            .unwrap_or(material.uniform.roughness);
        let metallic = mat_override
            .as_ref()
            .and_then(|o| o.metallic)
            .unwrap_or(material.uniform.metallic);
        let emission = if debug.emission_enabled {
            mat_override
                .as_ref()
                .and_then(|o| o.emission)
                .map(|e| [e[0], e[1], e[2], 0.0])
                .unwrap_or(material.uniform.emission)
        } else {
            [0.0; 4]
        };

        let base_color = mat_override
            .as_ref()
            .and_then(|o| o.base_color)
            .map(|c| [c[0], c[1], c[2], material.uniform.base_color[3]])
            .unwrap_or(material.uniform.base_color);

        let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
        let has_texture = if gpu_mesh.texture_bind_group.is_some() { 1.0f32 } else { 0.0f32 };
        // Check if entity has skeletal animation
        let entity_has_skin = scene_world.world
            .get::<&crate::components::Animator>(entity)
            .is_ok();

        let draw_uniform = DrawUniforms {
            model_matrix: model_matrix.to_cols_array_2d(),
            normal_matrix: normal_matrix.to_cols_array_2d(),
            base_color,
            roughness,
            metallic,
            has_texture,
            has_skin: if entity_has_skin { 1.0 } else { 0.0 },
            emission,
            _padding: [0.0; 20],
        };

        gpu.queue.write_buffer(
            &draw_pool.buffer,
            draw_index as u64 * DRAW_UNIFORM_SIZE,
            bytemuck::cast_slice(&[draw_uniform]),
        );
        draw_index += 1;
    }

    // Upload light uniforms (point lights + directional light)
    let mut light_data = LightingUniforms::default();
    if debug.point_lights_enabled {
        for (_entity, (transform, light)) in
            scene_world.world.query::<(&Transform, &PointLight)>().iter()
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
    for (_entity, dir_light) in
        scene_world.world.query::<&DirectionalLight>().iter()
    {
        light_data.has_directional = 1;
        light_data.dir_light_direction = dir_light.direction.to_array();
        light_data.dir_light_intensity = dir_light.intensity;
        light_data.dir_light_color = dir_light.color.to_array();

        // Compute orthographic VP from light direction
        let extent = dir_light.shadow_extent;
        let light_pos = -dir_light.direction.normalize() * 30.0;
        let light_view = glam::Mat4::look_at_rh(light_pos, glam::Vec3::ZERO, glam::Vec3::Y);
        let light_proj = glam::Mat4::orthographic_rh(
            -extent, extent, -extent, extent, 0.1, 60.0,
        );
        light_vp = light_proj * light_view;
        light_data.light_vp = light_vp.to_cols_array_2d();
        break; // Only one directional light supported
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
        gpu.queue.write_buffer(
            shadow_buf,
            0,
            bytemuck::cast_slice(&[shadow_data]),
        );
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
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                // Drop immediately ends the pass
            }
            continue;
        }
        match pass.pass_type {
            PassType::Rasterize => {
                execute_rasterize_pass(
                    &mut encoder,
                    gpu,
                    pass,
                    compiled,
                    scene_world,
                    camera_state,
                    draw_pool,
                    mesh_cache,
                    texture_resources,
                    bone_palettes,
                );
            }
            PassType::Fullscreen => {
                execute_fullscreen_pass(
                    &mut encoder,
                    pass,
                    compiled,
                    camera_state,
                    &swapchain_view,
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
                );
            }
            PassType::Shadow => {
                execute_shadow_pass(
                    &mut encoder,
                    gpu,
                    pass,
                    compiled,
                    scene_world,
                    draw_pool,
                    mesh_cache,
                    bone_palettes,
                );
            }
            PassType::Compute => {
                // Not implemented yet
            }
        }
    }

    encoder
}

/// Execute a shadow depth pass (renders all geometry from light's perspective).
fn execute_shadow_pass(
    encoder: &mut wgpu::CommandEncoder,
    gpu: &GpuState,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    bone_palettes: &HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,
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
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);

        // Group 0: shadow uniforms (light VP matrix)
        if let Some(bg) = &compiled.shadow_bind_group {
            render_pass.set_bind_group(0, bg, &[]);
        }

        // Draw all mesh entities (skip hidden before incrementing draw_index)
        let mut draw_index = 0u32;
        for (entity, (_, mesh_renderer)) in
            scene_world.world.query::<(&Transform, &MeshRenderer)>().iter()
        {
            if scene_world.world.get::<&Hidden>(entity).is_ok() {
                continue;
            }
            let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
            let dynamic_offset = draw_index * DRAW_UNIFORM_SIZE as u32;

            render_pass.set_bind_group(1, &draw_pool.bind_group, &[dynamic_offset]);

            // Upload bone matrices for skinned entities (group 2 in shadow shader)
            if let (Some(skin_buffer), Some(skin_bg)) = (&compiled.skin_buffer, &compiled.skin_bind_group) {
                if let Some(palette) = bone_palettes.get(&entity) {
                    gpu.queue.write_buffer(skin_buffer, 0, bytemuck::cast_slice(&[*palette]));
                } else {
                    let identity = crate::anim_system::BoneMatrixPalette::default();
                    gpu.queue.write_buffer(skin_buffer, 0, bytemuck::cast_slice(&[identity]));
                }
                render_pass.set_bind_group(2, skin_bg, &[]);
            }

            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                gpu_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
            draw_index += 1;
        }
    }
}

/// Execute a rasterize pass (G-buffer geometry pass).
fn execute_rasterize_pass(
    encoder: &mut wgpu::CommandEncoder,
    gpu: &GpuState,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    draw_pool: &DrawUniformPool,
    mesh_cache: &MeshCache,
    texture_resources: Option<&crate::mesh::TextureResources>,
    bone_palettes: &HashMap<hecs::Entity, crate::anim_system::BoneMatrixPalette>,
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
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        let mut draw_index = 0u32;
        for (entity, (_, mesh_renderer)) in
            scene_world.world.query::<(&Transform, &MeshRenderer)>().iter()
        {
            if scene_world.world.get::<&Hidden>(entity).is_ok() {
                continue;
            }
            let gpu_mesh = mesh_cache.get(mesh_renderer.mesh_handle);
            let dynamic_offset = draw_index * DRAW_UNIFORM_SIZE as u32;

            render_pass.set_bind_group(1, &draw_pool.bind_group, &[dynamic_offset]);

            // Bind texture at group(2): use mesh's texture or fallback to white
            if let Some(tex_res) = texture_resources {
                let tex_bg = gpu_mesh.texture_bind_group.as_ref()
                    .unwrap_or(&tex_res.default_bind_group);
                render_pass.set_bind_group(2, tex_bg, &[]);
            }

            // Upload bone matrices for skinned entities (group 3)
            if let (Some(skin_buffer), Some(skin_bg)) = (&compiled.skin_buffer, &compiled.skin_bind_group) {
                if let Some(palette) = bone_palettes.get(&entity) {
                    gpu.queue.write_buffer(skin_buffer, 0, bytemuck::cast_slice(&[*palette]));
                } else {
                    // Non-skinned: upload identity palette (has_skin=0)
                    let identity = crate::anim_system::BoneMatrixPalette::default();
                    gpu.queue.write_buffer(skin_buffer, 0, bytemuck::cast_slice(&[identity]));
                }
                render_pass.set_bind_group(3, skin_bg, &[]);
            }

            render_pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                gpu_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
            draw_index += 1;
        }
        let draw_count = draw_index;
        if draw_count == 0 {
            tracing::warn!("Rasterize pass '{}': ZERO entities drawn!", pass.name);
        } else {
            tracing::debug!("Rasterize pass '{}': {} entities drawn", pass.name, draw_count);
        }
    }
}

/// Execute a Gaussian splat rendering pass.
fn execute_splat_pass(
    encoder: &mut wgpu::CommandEncoder,
    pass: &CompiledPass,
    compiled: &CompiledPipeline,
    device: &wgpu::Device,
    scene_world: &SceneWorld,
    camera_state: &CameraState,
    splat_cache: &SplatCache,
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
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&pass.pipeline);
        render_pass.set_bind_group(0, &camera_state.bind_group, &[]);

        // For each entity with a GaussianSplat component, create a bind group and draw
        for (_entity, splat) in scene_world.world.query::<&GaussianSplat>().iter() {
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
) {
    let is_tonemap = pass.name.contains("tonemap");
    let is_bloom = pass.name.contains("bloom");
    let is_fxaa = pass.name.contains("fxaa");
    let writes_to_swapchain = pass
        .color_targets
        .iter()
        .any(|t| t == "swapchain");

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
            timestamp_writes: None,
            occlusion_query_set: None,
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
pub fn rebuild_bind_groups(
    device: &wgpu::Device,
    compiled: &mut CompiledPipeline,
) {
    // Rebuild G-buffer bind group
    if let Some(layout) = &compiled.gbuffer_bind_group_layout {
        let albedo_view = compiled
            .resources
            .get("gbuffer_albedo")
            .map(|r| &r.view);
        let normal_view = compiled
            .resources
            .get("gbuffer_normal")
            .map(|r| &r.view);
        let depth_view = compiled
            .resources
            .get("gbuffer_depth")
            .map(|r| &r.view);
        let emission_view = compiled
            .resources
            .get("gbuffer_emission")
            .map(|r| &r.view);

        if let (Some(albedo), Some(normal), Some(depth)) = (albedo_view, normal_view, depth_view) {
            let emission = emission_view.unwrap_or(albedo);
            compiled.gbuffer_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
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
                },
            ));
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
            compiled.bloom_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
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
                },
            ));
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

            let bloom_view = compiled.resources.get("bloom_buffer")
                .map(|r| &r.view)
                .unwrap_or(hdr_view);

            compiled.tonemap_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
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
                },
            ));
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
            compiled.fxaa_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
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
                },
            ));
        }
    }

    // Rebuild splat composite bind group
    if let Some(layout) = &compiled.splat_composite_bind_group_layout {
        let splat_color = compiled.resources.get("splat_color").map(|r| &r.view);
        let splat_depth = compiled.resources.get("splat_depth").map(|r| &r.view);

        if let (Some(color_view), Some(depth_view)) = (splat_color, splat_depth) {
            compiled.splat_composite_bind_group = Some(device.create_bind_group(
                &wgpu::BindGroupDescriptor {
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
                },
            ));
        }
    }

    // Rebuild lighting bind group (shadow map may have been resized)
    if let Some(sampler) = &compiled.shadow_sampler {
        // Create dummy shadow map fallback
        let shadow_dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Dummy Shadow Map (resized)"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_dummy_view = shadow_dummy_tex.create_view(&Default::default());
        let shadow_map_view = compiled.resources.get("shadow_map")
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
