use std::collections::{HashMap, HashSet};
use std::path::Path;

use glam::Vec2;
use serde::{Deserialize, Serialize};
use winit::event::{DeviceEvent, ElementState, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

/// Semantic action names mapped from physical inputs via bindings.yaml.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InputBindings {
    #[serde(default)]
    pub actions: HashMap<String, Vec<InputTrigger>>,
    #[serde(default)]
    pub axes: HashMap<String, AxisBinding>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum InputTrigger {
    Key(String),
    Mouse(String),
}

/// The physical input a trigger names.
enum Resolved {
    Key(KeyCode),
    Mouse(MouseButton),
}

/// The mouse button called `name` in bindings: "Left", "Right" or "Middle".
pub fn mouse_button_named(name: &str) -> Option<MouseButton> {
    match name {
        "Left" => Some(MouseButton::Left),
        "Right" => Some(MouseButton::Right),
        "Middle" => Some(MouseButton::Middle),
        _ => None,
    }
}

impl InputTrigger {
    /// Both variants hold a plain string, so bindings.yaml always loads as
    /// `Key`. Mouse button names therefore count as mouse buttons in either
    /// variant; no key shares their names (the arrow keys are `ArrowLeft`
    /// and so on).
    fn resolve(&self) -> Option<Resolved> {
        let (InputTrigger::Key(name) | InputTrigger::Mouse(name)) = self;
        if let Some(button) = mouse_button_named(name) {
            return Some(Resolved::Mouse(button));
        }
        match self {
            InputTrigger::Key(name) => key_name_to_code(name).map(Resolved::Key),
            InputTrigger::Mouse(_) => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AxisBinding {
    #[serde(default)]
    pub positive: Vec<String>,
    #[serde(default)]
    pub negative: Vec<String>,
    #[serde(default)]
    pub mouse: Option<String>,
}

impl Default for InputBindings {
    fn default() -> Self {
        let mut actions = HashMap::new();
        actions.insert("move_forward".into(), vec![InputTrigger::Key("W".into())]);
        actions.insert("move_backward".into(), vec![InputTrigger::Key("S".into())]);
        actions.insert("move_left".into(), vec![InputTrigger::Key("A".into())]);
        actions.insert("move_right".into(), vec![InputTrigger::Key("D".into())]);
        actions.insert("jump".into(), vec![InputTrigger::Key("Space".into())]);
        actions.insert("interact".into(), vec![InputTrigger::Key("E".into())]);
        actions.insert("sprint".into(), vec![InputTrigger::Key("ShiftLeft".into())]);
        actions.insert("attack".into(), vec![InputTrigger::Mouse("Left".into())]);

        Self {
            actions,
            axes: HashMap::new(),
        }
    }
}

/// Load input bindings from a YAML file, with defaults as fallback.
pub fn load_bindings(project_root: &Path) -> InputBindings {
    let path = project_root.join("input/bindings.yaml");
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_norway::from_str(&contents) {
                Ok(bindings) => {
                    tracing::info!("Loaded input bindings from {:?}", path);
                    return bindings;
                }
                Err(e) => tracing::warn!("Failed to parse bindings.yaml: {}", e),
            },
            Err(e) => tracing::warn!("Failed to read bindings.yaml: {}", e),
        }
    }
    tracing::info!("Using default input bindings");
    InputBindings::default()
}

/// Maps key name strings to winit KeyCode.
fn key_name_to_code(name: &str) -> Option<KeyCode> {
    match name {
        "A" => Some(KeyCode::KeyA),
        "B" => Some(KeyCode::KeyB),
        "C" => Some(KeyCode::KeyC),
        "D" => Some(KeyCode::KeyD),
        "E" => Some(KeyCode::KeyE),
        "F" => Some(KeyCode::KeyF),
        "G" => Some(KeyCode::KeyG),
        "H" => Some(KeyCode::KeyH),
        "I" => Some(KeyCode::KeyI),
        "J" => Some(KeyCode::KeyJ),
        "K" => Some(KeyCode::KeyK),
        "L" => Some(KeyCode::KeyL),
        "M" => Some(KeyCode::KeyM),
        "N" => Some(KeyCode::KeyN),
        "O" => Some(KeyCode::KeyO),
        "P" => Some(KeyCode::KeyP),
        "Q" => Some(KeyCode::KeyQ),
        "R" => Some(KeyCode::KeyR),
        "S" => Some(KeyCode::KeyS),
        "T" => Some(KeyCode::KeyT),
        "U" => Some(KeyCode::KeyU),
        "V" => Some(KeyCode::KeyV),
        "W" => Some(KeyCode::KeyW),
        "X" => Some(KeyCode::KeyX),
        "Y" => Some(KeyCode::KeyY),
        "Z" => Some(KeyCode::KeyZ),
        "Digit0" | "0" => Some(KeyCode::Digit0),
        "Digit1" | "1" => Some(KeyCode::Digit1),
        "Digit2" | "2" => Some(KeyCode::Digit2),
        "Digit3" | "3" => Some(KeyCode::Digit3),
        "Space" => Some(KeyCode::Space),
        "ShiftLeft" => Some(KeyCode::ShiftLeft),
        "ShiftRight" => Some(KeyCode::ShiftRight),
        "ControlLeft" => Some(KeyCode::ControlLeft),
        "ControlRight" => Some(KeyCode::ControlRight),
        "Escape" => Some(KeyCode::Escape),
        "Enter" => Some(KeyCode::Enter),
        "Tab" => Some(KeyCode::Tab),
        "ArrowUp" => Some(KeyCode::ArrowUp),
        "ArrowDown" => Some(KeyCode::ArrowDown),
        "ArrowLeft" => Some(KeyCode::ArrowLeft),
        "ArrowRight" => Some(KeyCode::ArrowRight),
        _ => None,
    }
}

/// Central input state, updated each frame.
pub struct InputState {
    bindings: InputBindings,
    // Raw key state
    keys_held: HashSet<KeyCode>,
    keys_just_pressed: HashSet<KeyCode>,
    keys_just_released: HashSet<KeyCode>,
    // Raw mouse state
    mouse_buttons_held: HashSet<MouseButton>,
    mouse_buttons_just_pressed: HashSet<MouseButton>,
    mouse_buttons_just_released: HashSet<MouseButton>,
    // Mouse motion accumulated this frame
    mouse_delta: Vec2,
    // Scroll wheel delta accumulated this frame (x=horizontal, y=vertical)
    scroll_delta: Vec2,
    // Cursor position
    cursor_position: Vec2,
    // Cursor position snapshot for frame-level delta computation
    frame_cursor_snapshot: Option<Vec2>,
    // Whether the cursor is captured (for FPS camera)
    pub cursor_captured: bool,
    // Synthetic input queue (for MCP/testing)
    synthetic_keys_pressed: HashSet<KeyCode>,
    synthetic_keys_released: HashSet<KeyCode>,
    synthetic_mouse_pressed: HashSet<MouseButton>,
    synthetic_mouse_released: HashSet<MouseButton>,
}

impl InputState {
    pub fn new(bindings: InputBindings) -> Self {
        Self {
            bindings,
            keys_held: HashSet::new(),
            keys_just_pressed: HashSet::new(),
            keys_just_released: HashSet::new(),
            mouse_buttons_held: HashSet::new(),
            mouse_buttons_just_pressed: HashSet::new(),
            mouse_buttons_just_released: HashSet::new(),
            mouse_delta: Vec2::ZERO,
            scroll_delta: Vec2::ZERO,
            cursor_position: Vec2::ZERO,
            frame_cursor_snapshot: None,
            cursor_captured: false,
            synthetic_keys_pressed: HashSet::new(),
            synthetic_keys_released: HashSet::new(),
            synthetic_mouse_pressed: HashSet::new(),
            synthetic_mouse_released: HashSet::new(),
        }
    }

    /// Call at the start of each frame to clear transient state.
    pub fn begin_frame(&mut self) {
        self.keys_just_pressed.clear();
        self.keys_just_released.clear();
        self.mouse_buttons_just_pressed.clear();
        self.mouse_buttons_just_released.clear();
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = Vec2::ZERO;

        // Apply synthetic inputs
        for key in self.synthetic_keys_pressed.drain() {
            self.keys_held.insert(key);
            self.keys_just_pressed.insert(key);
        }
        for key in self.synthetic_keys_released.drain() {
            self.keys_held.remove(&key);
            self.keys_just_released.insert(key);
        }
        for btn in self.synthetic_mouse_pressed.drain() {
            self.mouse_buttons_held.insert(btn);
            self.mouse_buttons_just_pressed.insert(btn);
        }
        for btn in self.synthetic_mouse_released.drain() {
            self.mouse_buttons_held.remove(&btn);
            self.mouse_buttons_just_released.insert(btn);
        }
    }

    /// Process a winit WindowEvent.
    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key_code) = event.physical_key {
                    match event.state {
                        ElementState::Pressed => {
                            if !self.keys_held.contains(&key_code) {
                                self.keys_just_pressed.insert(key_code);
                            }
                            self.keys_held.insert(key_code);
                        }
                        ElementState::Released => {
                            self.keys_held.remove(&key_code);
                            self.keys_just_released.insert(key_code);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match state {
                ElementState::Pressed => {
                    if !self.mouse_buttons_held.contains(button) {
                        self.mouse_buttons_just_pressed.insert(*button);
                    }
                    self.mouse_buttons_held.insert(*button);
                }
                ElementState::Released => {
                    self.mouse_buttons_held.remove(button);
                    self.mouse_buttons_just_released.insert(*button);
                }
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Vec2::new(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        self.scroll_delta.x += x;
                        self.scroll_delta.y += y;
                    }
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        // Normalize pixel deltas to ~line units
                        self.scroll_delta.x += pos.x as f32 / 120.0;
                        self.scroll_delta.y += pos.y as f32 / 120.0;
                    }
                }
            }
            _ => {}
        }
    }

    /// Process a winit DeviceEvent (for raw mouse motion).
    /// Only uses DeviceEvent delta when cursor is captured (locked mode),
    /// since CursorMoved won't fire when locked. When cursor is free,
    /// delta is computed from CursorMoved instead (reliable on macOS).
    pub fn handle_device_event(&mut self, event: &DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.cursor_captured {
                self.mouse_delta.x += delta.0 as f32;
                self.mouse_delta.y += delta.1 as f32;
            }
        }
    }

    /// Whether any trigger bound to `action` is in `keys` or `buttons`.
    fn action_in(
        &self,
        action: &str,
        keys: &HashSet<KeyCode>,
        buttons: &HashSet<MouseButton>,
    ) -> bool {
        self.bindings.actions.get(action).is_some_and(|triggers| {
            triggers.iter().any(|trigger| match trigger.resolve() {
                Some(Resolved::Key(code)) => keys.contains(&code),
                Some(Resolved::Mouse(button)) => buttons.contains(&button),
                None => false,
            })
        })
    }

    /// Check if a semantic action is currently held.
    pub fn pressed(&self, action: &str) -> bool {
        self.action_in(action, &self.keys_held, &self.mouse_buttons_held)
    }

    /// Check if a semantic action was just pressed this frame.
    pub fn just_pressed(&self, action: &str) -> bool {
        self.action_in(
            action,
            &self.keys_just_pressed,
            &self.mouse_buttons_just_pressed,
        )
    }

    /// Check if a semantic action was just released this frame.
    pub fn just_released(&self, action: &str) -> bool {
        self.action_in(
            action,
            &self.keys_just_released,
            &self.mouse_buttons_just_released,
        )
    }

    /// Check if any mapped action was just pressed this frame.
    pub fn any_just_pressed(&self) -> bool {
        for action in self.bindings.actions.keys() {
            if self.just_pressed(action) {
                return true;
            }
        }
        false
    }

    /// Get a 2D movement axis from WASD-style bindings.
    pub fn axis_2d(&self, forward: &str, backward: &str, left: &str, right: &str) -> Vec2 {
        let mut axis = Vec2::ZERO;
        if self.pressed(forward) {
            axis.y += 1.0;
        }
        if self.pressed(backward) {
            axis.y -= 1.0;
        }
        if self.pressed(left) {
            axis.x -= 1.0;
        }
        if self.pressed(right) {
            axis.x += 1.0;
        }
        if axis != Vec2::ZERO {
            axis = axis.normalize();
        }
        axis
    }

    /// Compute mouse delta from cursor position snapshot.
    /// Call once per frame before reading mouse_delta(), when cursor is not captured.
    /// Uses position snapshots so event ordering doesn't matter.
    pub fn compute_cursor_delta(&mut self) {
        if !self.cursor_captured {
            if let Some(prev) = self.frame_cursor_snapshot {
                self.mouse_delta = self.cursor_position - prev;
            } else {
                self.mouse_delta = Vec2::ZERO;
            }
            self.frame_cursor_snapshot = Some(self.cursor_position);
        }
    }

    /// Get raw mouse delta this frame.
    pub fn mouse_delta(&self) -> Vec2 {
        self.mouse_delta
    }

    /// Get scroll wheel delta this frame (y > 0 = scroll up).
    pub fn scroll_delta(&self) -> Vec2 {
        self.scroll_delta
    }

    /// Get cursor position.
    pub fn cursor_position(&self) -> Vec2 {
        self.cursor_position
    }

    /// Check if a raw mouse button is held.
    pub fn mouse_button_held(&self, button: MouseButton) -> bool {
        self.mouse_buttons_held.contains(&button)
    }

    /// Check if a raw key is held.
    pub fn key_held(&self, code: KeyCode) -> bool {
        self.keys_held.contains(&code)
    }

    /// Check if a raw key was just pressed this frame (by KeyCode, not action name).
    pub fn just_pressed_key(&self, code: KeyCode) -> bool {
        self.keys_just_pressed.contains(&code)
    }

    /// Inject synthetic key press (for MCP/testing).
    /// Cancels any pending release for the same key so last-write wins.
    pub fn inject_key_press(&mut self, key_name: &str) {
        if let Some(code) = key_name_to_code(key_name) {
            self.synthetic_keys_pressed.insert(code);
            self.synthetic_keys_released.remove(&code);
        }
    }

    /// Inject synthetic key release (for MCP/testing).
    /// Cancels any pending press for the same key so last-write wins.
    pub fn inject_key_release(&mut self, key_name: &str) {
        if let Some(code) = key_name_to_code(key_name) {
            self.synthetic_keys_released.insert(code);
            self.synthetic_keys_pressed.remove(&code);
        }
    }

    /// Inject synthetic mouse button press.
    pub fn inject_mouse_press(&mut self, button: MouseButton) {
        self.synthetic_mouse_pressed.insert(button);
        self.synthetic_mouse_released.remove(&button);
    }

    /// Inject synthetic mouse button release.
    pub fn inject_mouse_release(&mut self, button: MouseButton) {
        self.synthetic_mouse_released.insert(button);
        self.synthetic_mouse_pressed.remove(&button);
    }

    /// Inject synthetic mouse motion.
    pub fn inject_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.mouse_delta.x += dx;
        self.mouse_delta.y += dy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_bindings() {
        let bindings = InputBindings::default();
        assert!(bindings.actions.contains_key("move_forward"));
        assert!(bindings.actions.contains_key("jump"));
    }

    #[test]
    fn test_key_name_mapping() {
        assert_eq!(key_name_to_code("W"), Some(KeyCode::KeyW));
        assert_eq!(key_name_to_code("Space"), Some(KeyCode::Space));
        assert_eq!(key_name_to_code("ShiftLeft"), Some(KeyCode::ShiftLeft));
        assert_eq!(key_name_to_code("Invalid"), None);
    }

    #[test]
    fn mouse_buttons_in_bindings_yaml_are_mouse_buttons() {
        // A plain string in bindings.yaml always loads as a Key trigger.
        let bindings: InputBindings =
            serde_norway::from_str("actions:\n  attack:\n    - Left\n  aim:\n    - Right\n")
                .unwrap();
        assert!(
            matches!(&bindings.actions["attack"][0], InputTrigger::Key(name) if name == "Left")
        );

        let mut state = InputState::new(bindings);
        state.inject_mouse_press(MouseButton::Left);
        state.begin_frame();
        assert!(state.pressed("attack") && state.just_pressed("attack"));
        assert!(!state.pressed("aim"));

        // The arrow key is not the left mouse button.
        state.inject_mouse_release(MouseButton::Left);
        state.inject_key_press("ArrowLeft");
        state.begin_frame();
        assert!(!state.pressed("attack") && state.just_released("attack"));
    }

    #[test]
    fn test_input_state_pressed() {
        let bindings = InputBindings::default();
        let mut state = InputState::new(bindings);
        assert!(!state.pressed("move_forward"));

        // Simulate W key press
        state.keys_held.insert(KeyCode::KeyW);
        assert!(state.pressed("move_forward"));
    }

    #[test]
    fn test_axis_2d() {
        let bindings = InputBindings::default();
        let mut state = InputState::new(bindings);

        state.keys_held.insert(KeyCode::KeyW);
        state.keys_held.insert(KeyCode::KeyD);

        let axis = state.axis_2d("move_forward", "move_backward", "move_left", "move_right");
        assert!(axis.y > 0.0);
        assert!(axis.x > 0.0);
        assert!((axis.length() - 1.0).abs() < 0.001);
    }
}
