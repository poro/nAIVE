//! Lua camera API registration.

use super::{ScriptRuntime, SharedCameraShakeState};
use mlua::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

impl ScriptRuntime {
    /// Register camera API (world_to_screen projection).
    pub fn register_camera_api(
        &self,
        camera_state: Rc<RefCell<crate::camera::CameraState>>,
        surface_config: Rc<RefCell<wgpu::SurfaceConfiguration>>,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let camera_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // camera.world_to_screen(x, y, z) -> sx, sy, visible
        let cs = camera_state.clone();
        let config = surface_config.clone();
        let w2s_fn = self
            .lua
            .create_function(move |_, (x, y, z): (f32, f32, f32)| {
                let cs = cs.borrow();
                let config = config.borrow();
                let vp = glam::Mat4::from_cols_array_2d(&cs.uniform.view_projection);
                let clip = vp * glam::Vec4::new(x, y, z, 1.0);
                if clip.w <= 0.0 {
                    return Ok((0.0f32, 0.0f32, false));
                }
                let ndc_x = clip.x / clip.w;
                let ndc_y = clip.y / clip.w;
                let ndc_z = clip.z / clip.w;
                let visible = (-1.0..=1.0).contains(&ndc_x)
                    && (-1.0..=1.0).contains(&ndc_y)
                    && (0.0..=1.0).contains(&ndc_z);
                let sx = (ndc_x * 0.5 + 0.5) * config.width as f32;
                let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * config.height as f32;
                Ok((sx, sy, visible))
            })
            .map_err(|e| e.to_string())?;
        camera_table
            .set("world_to_screen", w2s_fn)
            .map_err(|e| e.to_string())?;

        // camera.screen_to_ray(sx, sy) -> (ox, oy, oz, dx, dy, dz) — unproject screen pixel to world ray
        let cs = camera_state.clone();
        let config = surface_config.clone();
        let s2r_fn = self
            .lua
            .create_function(move |_, (sx, sy): (f32, f32)| {
                let cs = cs.borrow();
                let config = config.borrow();
                let width = config.width as f32;
                let height = config.height as f32;
                let ndc_x = (sx / width) * 2.0 - 1.0;
                let ndc_y = 1.0 - (sy / height) * 2.0;
                let inv_vp = glam::Mat4::from_cols_array_2d(&cs.uniform.inv_view_projection);
                let near_clip = inv_vp * glam::Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
                let far_clip = inv_vp * glam::Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
                if near_clip.w.abs() < 1e-6 || far_clip.w.abs() < 1e-6 {
                    return Ok((0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, -1.0f32));
                }
                let origin = near_clip.truncate() / near_clip.w;
                let far_pt = far_clip.truncate() / far_clip.w;
                let direction = (far_pt - origin).normalize();
                Ok((
                    origin.x,
                    origin.y,
                    origin.z,
                    direction.x,
                    direction.y,
                    direction.z,
                ))
            })
            .map_err(|e| e.to_string())?;
        camera_table
            .set("screen_to_ray", s2r_fn)
            .map_err(|e| e.to_string())?;

        globals
            .set("camera", camera_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register camera shake API (camera.shake).
    pub fn register_camera_shake_api(
        &self,
        camera_shake: SharedCameraShakeState,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let camera_table: LuaTable = globals.get("camera").map_err(|e| e.to_string())?;

        // camera.shake(intensity, duration)
        let shake = camera_shake.clone();
        let shake_fn = self
            .lua
            .create_function(move |_, (intensity, duration): (f32, f32)| {
                let mut shake = shake.borrow_mut();
                shake.intensity = intensity;
                shake.duration = duration;
                shake.timer = duration;
                shake.seed = shake.seed.wrapping_add(1);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        camera_table
            .set("shake", shake_fn)
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}
