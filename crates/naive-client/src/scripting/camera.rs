//! Lua camera API registration.

use super::{ScriptRuntime, SharedCameraShakeState};
use mlua::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

impl ScriptRuntime {
    /// Register projection/picking against the shared CPU camera and viewport.
    pub fn register_camera_api(
        &self,
        scene_world: super::SharedSceneWorld,
        physics_world: super::SharedPhysicsWorld,
        viewport: Rc<RefCell<naive_sim::presentation::Viewport>>,
        camera_shake: SharedCameraShakeState,
    ) -> Result<(), String> {
        self.register_camera_api_with_override(
            scene_world,
            physics_world,
            viewport,
            camera_shake,
            None,
        )
    }

    /// The editor supplies its free-camera pose; gameplay uses the ECS camera.
    pub fn register_camera_api_with_override(
        &self,
        scene_world: super::SharedSceneWorld,
        physics_world: super::SharedPhysicsWorld,
        viewport: Rc<RefCell<naive_sim::presentation::Viewport>>,
        camera_shake: SharedCameraShakeState,
        camera_override: Option<super::SharedCameraOverride>,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let table = self.lua.create_table().map_err(|e| e.to_string())?;
        let view = move || {
            if let Some(camera_override) = &camera_override {
                if let Some(view) = camera_override.borrow().as_ref() {
                    return Ok(naive_sim::presentation::CameraView::new(
                        &view.camera,
                        &view.transform,
                        *viewport.borrow(),
                    ));
                }
            }
            naive_sim::presentation::scene_camera(
                &scene_world.borrow().world,
                Some(&physics_world.borrow()),
                *viewport.borrow(),
                camera_shake.borrow().offset(),
            )
            .ok_or_else(|| {
                LuaError::RuntimeError(
                    "camera query requires a main camera in the loaded scene".into(),
                )
            })
        };
        let world_view = view.clone();
        table
            .set(
                "world_to_screen",
                self.lua
                    .create_function(move |_, (x, y, z): (f32, f32, f32)| {
                        Ok(world_view()?.world_to_screen(glam::Vec3::new(x, y, z)))
                    })
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        table
            .set(
                "screen_to_ray",
                self.lua
                    .create_function(move |_, (x, y): (f32, f32)| {
                        let (origin, direction) = view()?.screen_to_ray(x, y).ok_or_else(|| {
                            LuaError::RuntimeError(
                                "camera screen_to_ray cannot unproject these coordinates".into(),
                            )
                        })?;
                        Ok((
                            origin.x,
                            origin.y,
                            origin.z,
                            direction.x,
                            direction.y,
                            direction.z,
                        ))
                    })
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        globals.set("camera", table).map_err(|e| e.to_string())?;
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
