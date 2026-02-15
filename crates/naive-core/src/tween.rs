use std::collections::HashMap;

/// Easing functions for tweens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Bounce,
}

impl Easing {
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => t * (2.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
            Easing::Bounce => {
                let t = 1.0 - t;
                let v = if t < 1.0 / 2.75 {
                    7.5625 * t * t
                } else if t < 2.0 / 2.75 {
                    let t = t - 1.5 / 2.75;
                    7.5625 * t * t + 0.75
                } else if t < 2.5 / 2.75 {
                    let t = t - 2.25 / 2.75;
                    7.5625 * t * t + 0.9375
                } else {
                    let t = t - 2.625 / 2.75;
                    7.5625 * t * t + 0.984375
                };
                1.0 - v
            }
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "ease_in" => Easing::EaseIn,
            "ease_out" => Easing::EaseOut,
            "ease_in_out" => Easing::EaseInOut,
            "bounce" => Easing::Bounce,
            _ => Easing::Linear,
        }
    }
}

/// A property tween animation.
#[derive(Debug, Clone)]
pub struct Tween {
    pub entity: hecs::Entity,
    pub property: String,
    pub from: f32,
    pub to: f32,
    pub duration: f32,
    pub elapsed: f32,
    pub easing: Easing,
    pub on_complete: Option<String>, // Event to emit when done
}

impl Tween {
    pub fn new(
        entity: hecs::Entity,
        property: &str,
        from: f32,
        to: f32,
        duration: f32,
        easing: Easing,
    ) -> Self {
        Self {
            entity,
            property: property.to_string(),
            from,
            to,
            duration,
            elapsed: 0.0,
            easing,
            on_complete: None,
        }
    }

    /// Update the tween, returning the current interpolated value.
    /// Returns None if the tween is complete.
    pub fn update(&mut self, dt: f32) -> Option<f32> {
        self.elapsed += dt;
        if self.elapsed >= self.duration {
            return None; // Complete
        }
        let t = self.elapsed / self.duration;
        let eased = self.easing.apply(t);
        Some(self.from + (self.to - self.from) * eased)
    }

    /// Get the final value.
    pub fn final_value(&self) -> f32 {
        self.to
    }

    /// Check if tween is complete.
    pub fn is_complete(&self) -> bool {
        self.elapsed >= self.duration
    }
}

/// Manages active tweens.
pub struct TweenSystem {
    tweens: Vec<Tween>,
    next_id: u64,
    tween_ids: HashMap<u64, usize>,
}

impl TweenSystem {
    pub fn new() -> Self {
        Self {
            tweens: Vec::new(),
            next_id: 0,
            tween_ids: HashMap::new(),
        }
    }

    /// Add a new tween. Returns an ID.
    pub fn add(&mut self, tween: Tween) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.tween_ids.insert(id, self.tweens.len());
        self.tweens.push(tween);
        id
    }

    /// Update all tweens. Returns completed tween events.
    pub fn update(&mut self, dt: f32) -> Vec<(hecs::Entity, String, f32, Option<String>)> {
        let mut results = Vec::new();
        let mut completed_indices = Vec::new();

        for (i, tween) in self.tweens.iter_mut().enumerate() {
            match tween.update(dt) {
                Some(value) => {
                    results.push((tween.entity, tween.property.clone(), value, None));
                }
                None => {
                    results.push((
                        tween.entity,
                        tween.property.clone(),
                        tween.final_value(),
                        tween.on_complete.clone(),
                    ));
                    completed_indices.push(i);
                }
            }
        }

        // Remove completed tweens (in reverse to preserve indices)
        for &i in completed_indices.iter().rev() {
            self.tweens.swap_remove(i);
        }

        // Rebuild ID mapping
        self.tween_ids.clear();

        results
    }

    /// Cancel a tween by ID.
    pub fn cancel(&mut self, id: u64) {
        if let Some(&idx) = self.tween_ids.get(&id) {
            if idx < self.tweens.len() {
                self.tweens.swap_remove(idx);
                self.tween_ids.remove(&id);
            }
        }
    }

    /// Get the number of active tweens.
    pub fn active_count(&self) -> usize {
        self.tweens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_easing_linear() {
        assert!((Easing::Linear.apply(0.0) - 0.0).abs() < 0.001);
        assert!((Easing::Linear.apply(0.5) - 0.5).abs() < 0.001);
        assert!((Easing::Linear.apply(1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_easing_ease_in() {
        assert!((Easing::EaseIn.apply(0.0) - 0.0).abs() < 0.001);
        assert!((Easing::EaseIn.apply(1.0) - 1.0).abs() < 0.001);
        // EaseIn should be slower at start
        assert!(Easing::EaseIn.apply(0.5) < 0.5);
    }

    #[test]
    fn test_tween_update() {
        let world = hecs::World::new();
        let entity = world.reserve_entity();
        let mut tween = Tween::new(entity, "x", 0.0, 10.0, 1.0, Easing::Linear);

        let val = tween.update(0.5).unwrap();
        assert!((val - 5.0).abs() < 0.1);

        let val = tween.update(0.6);
        assert!(val.is_none()); // Complete
    }

    #[test]
    fn test_tween_system() {
        let world = hecs::World::new();
        let entity = world.reserve_entity();
        let mut system = TweenSystem::new();

        system.add(Tween::new(entity, "opacity", 1.0, 0.0, 0.5, Easing::EaseOut));
        assert_eq!(system.active_count(), 1);

        let results = system.update(0.25);
        assert_eq!(results.len(), 1);
        assert_eq!(system.active_count(), 1);

        let results = system.update(0.3);
        assert_eq!(results.len(), 1);
        assert_eq!(system.active_count(), 0); // Completed
    }
}
