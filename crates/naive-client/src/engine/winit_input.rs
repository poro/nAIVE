//! The edge between winit and nAIVE's input types: window and device events
//! become [`InputEvent`]s for [`crate::input::InputState`], which lives in
//! the GPU-free `naive-sim` crate and knows nothing about winit.

use glam::Vec2;
use winit::event::{DeviceEvent, ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::input::{InputEvent, Key, MouseButton};

/// Pixels per scroll line, for trackpads that report scrolling in pixels.
const PIXELS_PER_SCROLL_LINE: f32 = 120.0;

/// The input event in a window event, if it is one nAIVE tracks.
pub(super) fn window_input_event(event: &WindowEvent) -> Option<InputEvent> {
    match event {
        WindowEvent::KeyboardInput { event, .. } => {
            let PhysicalKey::Code(code) = event.physical_key else {
                return None;
            };
            Some(InputEvent::Key {
                key: key_from_winit(code)?,
                pressed: event.state == ElementState::Pressed,
            })
        }
        WindowEvent::MouseInput { state, button, .. } => Some(InputEvent::MouseButton {
            button: mouse_button_from_winit(*button)?,
            pressed: *state == ElementState::Pressed,
        }),
        WindowEvent::CursorMoved { position, .. } => Some(InputEvent::CursorMoved(Vec2::new(
            position.x as f32,
            position.y as f32,
        ))),
        WindowEvent::MouseWheel { delta, .. } => Some(InputEvent::Scroll(match delta {
            MouseScrollDelta::LineDelta(x, y) => Vec2::new(*x, *y),
            MouseScrollDelta::PixelDelta(pos) => {
                Vec2::new(pos.x as f32, pos.y as f32) / PIXELS_PER_SCROLL_LINE
            }
        })),
        _ => None,
    }
}

/// The input event in a device event: raw mouse motion.
pub(super) fn device_input_event(event: &DeviceEvent) -> Option<InputEvent> {
    match event {
        DeviceEvent::MouseMotion { delta } => Some(InputEvent::RawMouseMotion(Vec2::new(
            delta.0 as f32,
            delta.1 as f32,
        ))),
        _ => None,
    }
}

/// The nAIVE key for a winit key code. Keys nAIVE has no name for are
/// dropped: nothing can bind or query them.
fn key_from_winit(code: KeyCode) -> Option<Key> {
    Key::ALL
        .iter()
        .copied()
        .find(|&key| winit_key_code(key) == code)
}

/// The winit key code for each nAIVE key. The match is exhaustive, so a new
/// [`Key`] does not compile until it is mapped here.
fn winit_key_code(key: Key) -> KeyCode {
    match key {
        Key::A => KeyCode::KeyA,
        Key::B => KeyCode::KeyB,
        Key::C => KeyCode::KeyC,
        Key::D => KeyCode::KeyD,
        Key::E => KeyCode::KeyE,
        Key::F => KeyCode::KeyF,
        Key::G => KeyCode::KeyG,
        Key::H => KeyCode::KeyH,
        Key::I => KeyCode::KeyI,
        Key::J => KeyCode::KeyJ,
        Key::K => KeyCode::KeyK,
        Key::L => KeyCode::KeyL,
        Key::M => KeyCode::KeyM,
        Key::N => KeyCode::KeyN,
        Key::O => KeyCode::KeyO,
        Key::P => KeyCode::KeyP,
        Key::Q => KeyCode::KeyQ,
        Key::R => KeyCode::KeyR,
        Key::S => KeyCode::KeyS,
        Key::T => KeyCode::KeyT,
        Key::U => KeyCode::KeyU,
        Key::V => KeyCode::KeyV,
        Key::W => KeyCode::KeyW,
        Key::X => KeyCode::KeyX,
        Key::Y => KeyCode::KeyY,
        Key::Z => KeyCode::KeyZ,
        Key::Digit0 => KeyCode::Digit0,
        Key::Digit1 => KeyCode::Digit1,
        Key::Digit2 => KeyCode::Digit2,
        Key::Digit3 => KeyCode::Digit3,
        Key::Digit4 => KeyCode::Digit4,
        Key::Digit5 => KeyCode::Digit5,
        Key::Digit6 => KeyCode::Digit6,
        Key::Digit7 => KeyCode::Digit7,
        Key::Digit8 => KeyCode::Digit8,
        Key::Digit9 => KeyCode::Digit9,
        Key::Space => KeyCode::Space,
        Key::ShiftLeft => KeyCode::ShiftLeft,
        Key::ShiftRight => KeyCode::ShiftRight,
        Key::ControlLeft => KeyCode::ControlLeft,
        Key::ControlRight => KeyCode::ControlRight,
        Key::Escape => KeyCode::Escape,
        Key::Enter => KeyCode::Enter,
        Key::Tab => KeyCode::Tab,
        Key::ArrowUp => KeyCode::ArrowUp,
        Key::ArrowDown => KeyCode::ArrowDown,
        Key::ArrowLeft => KeyCode::ArrowLeft,
        Key::ArrowRight => KeyCode::ArrowRight,
    }
}

/// The nAIVE mouse button for a winit one; back, forward and other buttons
/// are dropped.
fn mouse_button_from_winit(button: winit::event::MouseButton) -> Option<MouseButton> {
    match button {
        winit::event::MouseButton::Left => Some(MouseButton::Left),
        winit::event::MouseButton::Right => Some(MouseButton::Right),
        winit::event::MouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_maps_to_its_own_winit_code_and_back() {
        for &key in Key::ALL {
            assert_eq!(key_from_winit(winit_key_code(key)), Some(key), "{key:?}");
        }
    }

    #[test]
    fn winit_names_match_key_names() {
        // nAIVE's key names are winit's KeyCode names, without the "Key"
        // prefix on letters, so bindings files written against winit's names
        // keep working.
        for &key in Key::ALL {
            let winit_name = format!("{:?}", winit_key_code(key));
            assert_eq!(
                winit_name.strip_prefix("Key").unwrap_or(&winit_name),
                key.name()
            );
        }
    }

    #[test]
    fn unnamed_keys_and_buttons_are_dropped() {
        assert_eq!(key_from_winit(KeyCode::F1), None);
        assert_eq!(
            mouse_button_from_winit(winit::event::MouseButton::Back),
            None
        );
    }

    #[test]
    fn raw_mouse_motion_passes_through() {
        let motion = device_input_event(&DeviceEvent::MouseMotion { delta: (2.0, -3.0) });
        assert_eq!(
            motion,
            Some(InputEvent::RawMouseMotion(Vec2::new(2.0, -3.0)))
        );
    }
}
