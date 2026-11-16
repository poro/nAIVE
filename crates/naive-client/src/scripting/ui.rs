//! Lua ui API registration.

use super::{ScriptRuntime, SharedBitmapFont, SharedUiRenderer};
use std::cell::RefCell;
use std::rc::Rc;

impl ScriptRuntime {
    /// Register UI overlay API (text, rect, flash, screen dimensions).
    pub fn register_ui_api(
        &self,
        ui_renderer: SharedUiRenderer,
        bitmap_font: SharedBitmapFont,
        surface_config: Rc<RefCell<wgpu::SurfaceConfiguration>>,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let ui_table = self.lua.create_table().map_err(|e| e.to_string())?;

        // ui.text(x, y, text, size, r, g, b, a)
        let ui = ui_renderer.clone();
        let font = bitmap_font.clone();
        let text_fn = self
            .lua
            .create_function(
                move |_,
                      (x, y, text, size, r, g, b, a): (
                    f32,
                    f32,
                    String,
                    f32,
                    f32,
                    f32,
                    f32,
                    f32,
                )| {
                    let mut ui = ui.borrow_mut();
                    let font = font.borrow();
                    ui.draw_text(x, y, &text, size, [r, g, b, a], &font);
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        ui_table.set("text", text_fn).map_err(|e| e.to_string())?;

        // ui.rect(x, y, w, h, r, g, b, a)
        let ui = ui_renderer.clone();
        let rect_fn = self
            .lua
            .create_function(
                move |_, (x, y, w, h, r, g, b, a): (f32, f32, f32, f32, f32, f32, f32, f32)| {
                    let mut ui = ui.borrow_mut();
                    ui.draw_rect(x, y, w, h, [r, g, b, a]);
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())?;
        ui_table.set("rect", rect_fn).map_err(|e| e.to_string())?;

        // ui.flash(r, g, b, a, duration)
        let ui = ui_renderer.clone();
        let flash_fn = self
            .lua
            .create_function(move |_, (r, g, b, a, dur): (f32, f32, f32, f32, f32)| {
                let mut ui = ui.borrow_mut();
                ui.set_flash([r, g, b, a], dur);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        ui_table.set("flash", flash_fn).map_err(|e| e.to_string())?;

        // ui.text_width(text, font_size) -> pixels
        let font = bitmap_font.clone();
        let text_width_fn = self
            .lua
            .create_function(move |_, (text, font_size): (String, f32)| {
                let font = font.borrow();
                let scale = font_size / font.glyph_h;
                let width = text.len() as f32 * font.glyph_w * scale;
                Ok(width)
            })
            .map_err(|e| e.to_string())?;
        ui_table
            .set("text_width", text_width_fn)
            .map_err(|e| e.to_string())?;

        // ui.screen_width() -> number
        let config = surface_config.clone();
        let width_fn = self
            .lua
            .create_function(move |_, ()| {
                let config = config.borrow();
                Ok(config.width as f32)
            })
            .map_err(|e| e.to_string())?;
        ui_table
            .set("screen_width", width_fn)
            .map_err(|e| e.to_string())?;

        // ui.screen_height() -> number
        let config = surface_config.clone();
        let height_fn = self
            .lua
            .create_function(move |_, ()| {
                let config = config.borrow();
                Ok(config.height as f32)
            })
            .map_err(|e| e.to_string())?;
        ui_table
            .set("screen_height", height_fn)
            .map_err(|e| e.to_string())?;

        globals.set("ui", ui_table).map_err(|e| e.to_string())?;
        Ok(())
    }
}
