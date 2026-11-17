//! CPU playback shared by native skeletal rendering and headless simulation.
use naive_core::animation::{AnimationClip, AnimationController};

/// Apply authored playback defaults identically before script initialization.
pub fn controller_from_definition(
    definition: Option<&naive_core::scene::AnimatorDef>,
) -> AnimationController {
    let Some(definition) = definition else {
        return AnimationController::default();
    };
    AnimationController {
        current_state: naive_core::animation::AnimState::from_name(&definition.state),
        speed: definition.speed,
        looping: definition.looping,
        ..Default::default()
    }
}

/// Resolve named clips with the native first-clip fallback, then advance the
/// controller using the imported duration. No synthetic duration is substituted.
pub fn advance_controller(
    controller: &mut AnimationController,
    clips: &[AnimationClip],
    dt: f32,
) -> Option<usize> {
    if controller.active_clip_index.is_none() {
        let name = controller.current_state.clip_name();
        controller.active_clip_index = clips
            .iter()
            .position(|clip| clip.name == name)
            .or_else(|| (!clips.is_empty()).then_some(0));
    }
    let index = controller.active_clip_index?;
    let clip = clips.get(index)?;
    controller.tick(dt, clip.duration);
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use naive_core::animation::AnimState;
    #[test]
    fn imported_duration_controls_looping_speed_and_named_fallback() {
        let clips = vec![AnimationClip {
            name: "walk".into(),
            duration: 0.5,
            channels: vec![],
        }];
        let mut controller = AnimationController {
            speed: 2.0,
            ..Default::default()
        };
        assert_eq!(advance_controller(&mut controller, &clips, 0.3), Some(0));
        assert!((controller.current_time - 0.1).abs() < 1e-6);
        controller.play(AnimState::Walk);
        controller.looping = false;
        advance_controller(&mut controller, &clips, 1.0);
        assert_eq!(controller.current_time, 0.5);
        controller.stop();
        assert_eq!(controller.current_time, 0.0);
        assert_eq!(advance_controller(&mut controller, &[], 1.0), None);
    }
}
