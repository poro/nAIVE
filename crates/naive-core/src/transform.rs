use glam::Mat4;
use hecs::World;

use crate::components::Transform;

/// Compute world matrices for all entities with Transform components.
pub fn update_transforms(world: &mut World) {
    // First pass: root entities (no parent)
    let mut updates: Vec<(hecs::Entity, Mat4)> = Vec::new();

    for (entity, transform) in world.query_mut::<&Transform>() {
        if transform.parent.is_none() {
            let local = Mat4::from_scale_rotation_translation(
                transform.scale,
                transform.rotation,
                transform.position,
            );
            updates.push((entity, local));
        }
    }

    for (entity, world_matrix) in &updates {
        if let Ok(mut transform) = world.get::<&mut Transform>(*entity) {
            transform.world_matrix = *world_matrix;
            transform.dirty = false;
        }
    }

    // Second pass: collect child info (entity, parent_entity, local_matrix)
    let mut children: Vec<(hecs::Entity, hecs::Entity, Mat4)> = Vec::new();

    for (entity, transform) in world.query_mut::<&Transform>() {
        if let Some(parent_entity) = transform.parent {
            let local = Mat4::from_scale_rotation_translation(
                transform.scale,
                transform.rotation,
                transform.position,
            );
            children.push((entity, parent_entity, local));
        }
    }

    // Now look up parent world matrices separately and compute child world matrices
    let mut child_updates: Vec<(hecs::Entity, Mat4)> = Vec::new();

    for (entity, parent_entity, local) in &children {
        if let Ok(parent_t) = world.get::<&Transform>(*parent_entity) {
            child_updates.push((*entity, parent_t.world_matrix * *local));
        }
    }

    for (entity, world_matrix) in &child_updates {
        if let Ok(mut transform) = world.get::<&mut Transform>(*entity) {
            transform.world_matrix = *world_matrix;
            transform.dirty = false;
        }
    }
}
