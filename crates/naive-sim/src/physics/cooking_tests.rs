use super::*;
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
enum BodyKind {
    #[default]
    Static,
    Kinematic,
    Dynamic,
}

#[derive(Clone, Copy)]
struct BodyOptions {
    kind: BodyKind,
    position: Vec3,
    rotation: Quat,
    sensor: bool,
    friction: f32,
    restitution: f32,
    mass: f32,
    ccd: bool,
}

impl Default for BodyOptions {
    fn default() -> Self {
        Self {
            kind: BodyKind::Static,
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            sensor: false,
            friction: 0.5,
            restitution: 0.0,
            mass: 1.0,
            ccd: false,
        }
    }
}

fn plane(scale: Vec3) -> PhysicsShape {
    PhysicsShape::Trimesh {
        vertices: [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(-1.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
        ]
        .map(|point| to_rapier_vec(point * scale))
        .to_vec(),
        indices: vec![[0, 2, 1], [1, 2, 3]],
    }
}

fn add(
    physics: &mut PhysicsWorld,
    ecs: &mut hecs::World,
    shape: PhysicsShape,
    options: BodyOptions,
) -> (hecs::Entity, RigidBodyHandle, ColliderHandle) {
    let entity = ecs.spawn(());
    let (body, collider) = match options.kind {
        BodyKind::Static => physics.add_static_body(
            entity,
            options.position,
            options.rotation,
            shape,
            options.sensor,
            options.restitution,
            options.friction,
        ),
        BodyKind::Kinematic => physics.add_kinematic_body(
            entity,
            options.position,
            options.rotation,
            shape,
            options.sensor,
            options.restitution,
            options.friction,
        ),
        BodyKind::Dynamic => physics.add_dynamic_body_with_sensor(
            entity,
            options.position,
            options.rotation,
            shape,
            options.mass,
            options.restitution,
            options.friction,
            options.ccd,
            options.sensor,
        ),
    };
    (entity, body, collider)
}

fn shared(physics: &PhysicsWorld, collider: ColliderHandle) -> SharedShape {
    physics.collider_set[collider].shared_shape().clone()
}

#[test]
fn identical_meshes_share_cooking_across_body_types_and_keep_collider_properties() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    let cases = [
        BodyOptions::default(),
        BodyOptions {
            kind: BodyKind::Kinematic,
            position: Vec3::X * 5.0,
            rotation: Quat::from_rotation_y(0.7),
            sensor: true,
            friction: 0.25,
            restitution: 0.8,
            ..Default::default()
        },
        BodyOptions {
            kind: BodyKind::Dynamic,
            position: Vec3::X * 10.0,
            friction: 0.9,
            restitution: 0.3,
            mass: 5.0,
            ccd: true,
            ..Default::default()
        },
        BodyOptions {
            kind: BodyKind::Dynamic,
            position: Vec3::X * 15.0,
            sensor: true,
            mass: 2.0,
            ..Default::default()
        },
    ];
    let mut handles = Vec::new();
    for options in cases {
        let (_, body, collider) = add(&mut physics, &mut ecs, plane(Vec3::ONE), options);
        let collider_ref = &physics.collider_set[collider];
        assert_eq!(collider_ref.is_sensor(), options.sensor);
        assert_eq!(collider_ref.friction(), options.friction);
        assert_eq!(collider_ref.restitution(), options.restitution);
        let body_ref = &physics.rigid_body_set[body];
        assert!((from_rapier_vec(body_ref.translation()) - options.position).length() < 1e-6);
        match options.kind {
            BodyKind::Static => assert!(body_ref.is_fixed()),
            BodyKind::Kinematic => assert!(body_ref.is_kinematic()),
            BodyKind::Dynamic => {
                assert!(body_ref.is_dynamic());
                assert_eq!(body_ref.is_ccd_enabled(), options.ccd);
                assert_eq!(collider_ref.mass(), options.mass);
            }
        }
        handles.push((body, collider));
    }
    let first = shared(&physics, handles[0].1);
    for &(body, collider) in &handles[1..] {
        assert_ne!(body, handles[0].0);
        assert_ne!(collider, handles[0].1);
        assert!(Arc::ptr_eq(&first.0, &shared(&physics, collider).0));
    }
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 1);
    assert_eq!(stats.cache_hits, 3);
    assert_eq!(stats.cached_meshes, 1);
    assert!(stats.cached_geometry_bytes > 0);
}

#[test]
fn mutable_collider_access_detaches_from_other_colliders_and_the_cached_shape() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    let (_, _, first) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    let (_, _, second) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    let original = shared(&physics, first);
    assert!(Arc::ptr_eq(&original.0, &shared(&physics, second).0));

    // Mutable access takes Rapier's copy-on-write path before any edits.
    let _ = physics.collider_set[first].shape_mut();
    assert!(!Arc::ptr_eq(&original.0, &shared(&physics, first).0));
    assert!(Arc::ptr_eq(&original.0, &shared(&physics, second).0));
    let (_, _, third) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert!(Arc::ptr_eq(&original.0, &shared(&physics, third).0));
    assert_eq!(physics.collider_cooking_stats().mesh_cooks, 1);
    assert_eq!(physics.collider_cooking_stats().cache_hits, 2);
}

#[test]
fn changed_coordinates_ordered_indices_and_scale_get_distinct_cooked_shapes() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    let original = plane(Vec3::ONE);
    let mut moved_vertex = original.clone();
    if let PhysicsShape::Trimesh { vertices, .. } = &mut moved_vertex {
        vertices[0].x -= 0.25;
    }
    let mut reordered_indices = original.clone();
    if let PhysicsShape::Trimesh { indices, .. } = &mut reordered_indices {
        indices.reverse();
    }
    let scaled = plane(Vec3::new(2.0, 1.0, 3.0));
    let mut shapes = Vec::new();
    for geometry in [original, moved_vertex, reordered_indices, scaled] {
        let (_, _, first) = add(&mut physics, &mut ecs, geometry.clone(), Default::default());
        let (_, _, repeated) = add(
            &mut physics,
            &mut ecs,
            geometry,
            BodyOptions {
                position: Vec3::Y * 10.0,
                rotation: Quat::from_rotation_y(0.5),
                ..Default::default()
            },
        );
        let first = shared(&physics, first);
        assert!(Arc::ptr_eq(&first.0, &shared(&physics, repeated).0));
        assert!(shapes
            .iter()
            .all(|other: &SharedShape| !Arc::ptr_eq(&first.0, &other.0)));
        shapes.push(first);
    }
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 4);
    assert_eq!(stats.cache_hits, 4);
    assert_eq!(stats.cached_meshes, 4);
}

#[test]
fn coordinate_bit_patterns_are_exact_cache_keys() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    let positive_zero = plane(Vec3::ONE);
    let mut negative_zero = positive_zero.clone();
    if let PhysicsShape::Trimesh { vertices, .. } = &mut negative_zero {
        vertices[0].y = -0.0;
    }
    let (_, _, first) = add(&mut physics, &mut ecs, positive_zero, Default::default());
    let (_, _, second) = add(&mut physics, &mut ecs, negative_zero, Default::default());
    assert!(!Arc::ptr_eq(
        &shared(&physics, first).0,
        &shared(&physics, second).0
    ));
    assert_eq!(physics.collider_cooking_stats().mesh_cooks, 2);
    assert_eq!(physics.collider_cooking_stats().cache_hits, 0);
}

#[test]
fn eviction_and_body_destruction_leave_live_shapes_valid_and_allow_reuse() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    physics.cooked_shapes = cooking::ColliderShapeCache::with_limits(1, usize::MAX);
    let mut ecs = hecs::World::new();
    let (first_entity, first_body, first) =
        add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    let first_shape = shared(&physics, first);
    let (_, _, second) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(2.0)),
        BodyOptions {
            position: Vec3::X * 10.0,
            ..Default::default()
        },
    );
    assert_eq!(physics.collider_cooking_stats().cached_meshes, 1);
    assert_eq!(physics.collider_cooking_stats().evictions, 1);
    physics.step_fixed();
    assert_eq!(
        physics.raycast(Vec3::Y * 2.0, Vec3::NEG_Y, 4.0).unwrap().0,
        first_entity
    );
    assert!(Arc::ptr_eq(&first_shape.0, &shared(&physics, first).0));

    let (_, replacement_body, replacement) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::ONE),
        BodyOptions {
            position: Vec3::X * 20.0,
            ..Default::default()
        },
    );
    let replacement_shape = shared(&physics, replacement);
    assert!(!Arc::ptr_eq(&first_shape.0, &replacement_shape.0));
    assert_eq!(physics.collider_cooking_stats().mesh_cooks, 3);
    assert_eq!(physics.collider_cooking_stats().evictions, 2);
    physics.remove_body(first_body);
    physics.remove_body(replacement_body);
    assert!(physics.collider_set.contains(second));
    let (_, _, reused) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert!(Arc::ptr_eq(
        &replacement_shape.0,
        &shared(&physics, reused).0
    ));
    assert_eq!(physics.collider_cooking_stats().mesh_cooks, 3);
    assert_eq!(physics.collider_cooking_stats().cache_hits, 1);
}

#[test]
fn cache_hits_promote_entries_before_lru_eviction() {
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    physics.cooked_shapes = cooking::ColliderShapeCache::with_limits(2, usize::MAX);
    let mut ecs = hecs::World::new();
    let (_, _, a) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    let (_, _, b) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(2.0)),
        Default::default(),
    );
    let original_a = shared(&physics, a);
    let original_b = shared(&physics, b);
    let (_, _, a_hit) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert!(Arc::ptr_eq(&original_a.0, &shared(&physics, a_hit).0));
    add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(3.0)),
        Default::default(),
    );
    let (_, _, retained_a) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert!(Arc::ptr_eq(&original_a.0, &shared(&physics, retained_a).0));
    let (_, _, recooked_b) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(2.0)),
        Default::default(),
    );
    assert!(!Arc::ptr_eq(&original_b.0, &shared(&physics, recooked_b).0));
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 4);
    assert_eq!(stats.cache_hits, 2);
    assert_eq!(stats.cached_meshes, 2);
    assert_eq!(stats.evictions, 2);
}

#[test]
fn key_byte_budget_evicts_and_oversized_geometry_bypasses_without_displacing_entries() {
    let mut reference = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    add(
        &mut reference,
        &mut ecs,
        plane(Vec3::ONE),
        Default::default(),
    );
    let one_key_bytes = reference.collider_cooking_stats().cached_geometry_bytes;
    assert_eq!(one_key_bytes, (4 + 2) * std::mem::size_of::<[u32; 3]>());

    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    physics.cooked_shapes = cooking::ColliderShapeCache::with_limits(64, one_key_bytes);
    add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert_eq!(physics.collider_cooking_stats().cached_meshes, 1);
    let (_, _, retained) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(2.0)),
        Default::default(),
    );
    let retained_shape = shared(&physics, retained);
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.cached_meshes, 1);
    assert_eq!(stats.cached_geometry_bytes, one_key_bytes);
    assert_eq!(stats.evictions, 1);

    let mut oversized = plane(Vec3::splat(2.0));
    if let PhysicsShape::Trimesh { vertices, .. } = &mut oversized {
        vertices.push(to_rapier_vec(Vec3::ZERO));
    }
    for _ in 0..2 {
        add(
            &mut physics,
            &mut ecs,
            oversized.clone(),
            Default::default(),
        );
    }
    let (_, _, reused) = add(
        &mut physics,
        &mut ecs,
        plane(Vec3::splat(2.0)),
        Default::default(),
    );
    assert!(Arc::ptr_eq(&retained_shape.0, &shared(&physics, reused).0));
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 4);
    assert_eq!(stats.cache_hits, 1);
    assert_eq!(stats.cached_meshes, 1);
    assert_eq!(stats.cached_geometry_bytes, one_key_bytes);
    assert_eq!(stats.evictions, 1);
}

#[test]
fn zero_capacity_and_zero_key_budget_disable_retention_and_world_reset_clears_stats() {
    for (entries, bytes) in [(0, usize::MAX), (64, 0)] {
        let mut physics = PhysicsWorld::new(Vec3::ZERO);
        physics.cooked_shapes = cooking::ColliderShapeCache::with_limits(entries, bytes);
        let mut ecs = hecs::World::new();
        for _ in 0..2 {
            add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
        }
        let stats = physics.collider_cooking_stats();
        assert_eq!(stats.mesh_cooks, 2);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cached_meshes, 0);
        assert_eq!(stats.cached_geometry_bytes, 0);
        assert_eq!(stats.evictions, 0);
    }
    let mut physics = PhysicsWorld::new(Vec3::ZERO);
    let mut ecs = hecs::World::new();
    add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    let retained = shared(&physics, physics.collider_set.iter().next().unwrap().0);
    physics = PhysicsWorld::new(Vec3::ZERO);
    let stats = physics.collider_cooking_stats();
    assert_eq!(stats.mesh_cooks, 0);
    assert_eq!(stats.cache_hits, 0);
    assert_eq!(stats.cached_meshes, 0);
    assert_eq!(stats.cached_geometry_bytes, 0);
    assert_eq!(stats.evictions, 0);
    let (_, _, new) = add(&mut physics, &mut ecs, plane(Vec3::ONE), Default::default());
    assert!(!Arc::ptr_eq(&retained.0, &shared(&physics, new).0));
    assert_eq!(physics.collider_cooking_stats().mesh_cooks, 1);
}

struct SimulationResult {
    position: Vec3,
    velocity: Vec3,
    mesh_contact_started: bool,
    ray_distance: f32,
    ray_normal: Vec3,
    cooks: u64,
    hits: u64,
}

#[derive(Clone, Copy)]
enum CookingMode {
    Cached,
    Disabled,
    Original,
}

fn add_original_mesh(
    physics: &mut PhysicsWorld,
    ecs: &mut hecs::World,
    position: Vec3,
) -> hecs::Entity {
    let PhysicsShape::Trimesh { vertices, indices } = plane(Vec3::ONE) else {
        unreachable!()
    };
    // This reference preserves the original builder and parameters without
    // calling the new cache or its cooking helper.
    let params = rapier3d::parry::transformation::vhacd::VHACDParameters {
        resolution: 256,
        concavity: 0.002,
        ..Default::default()
    };
    let entity = ecs.spawn(());
    let body = physics.rigid_body_set.insert(
        RigidBodyBuilder::fixed()
            .translation(to_rapier_vec(position))
            .build(),
    );
    let collider = ColliderBuilder::convex_decomposition_with_params(&vertices, &indices, &params)
        .friction(0.5)
        .restitution(0.0)
        .build();
    let collider =
        physics
            .collider_set
            .insert_with_parent(collider, body, &mut physics.rigid_body_set);
    physics.body_to_entity.insert(body, entity);
    physics.collider_to_entity.insert(collider, entity);
    entity
}

fn simulate_contacts(mode: CookingMode, side: f32) -> SimulationResult {
    let mut physics = PhysicsWorld::new(Vec3::new(0.0, -9.81 * side, 0.0));
    if matches!(mode, CookingMode::Disabled) {
        physics.cooked_shapes = cooking::ColliderShapeCache::with_limits(0, 0);
    }
    let mut ecs = hecs::World::new();
    let mut floor = None;
    for position in [Vec3::ZERO, Vec3::X * 5.0] {
        floor = Some(if matches!(mode, CookingMode::Original) {
            add_original_mesh(&mut physics, &mut ecs, position)
        } else {
            add(
                &mut physics,
                &mut ecs,
                plane(Vec3::ONE),
                BodyOptions {
                    position,
                    ..Default::default()
                },
            )
            .0
        });
    }
    let floor = floor.unwrap();
    let (ball, body, _) = add(
        &mut physics,
        &mut ecs,
        PhysicsShape::Sphere { radius: 0.5 },
        BodyOptions {
            kind: BodyKind::Dynamic,
            position: Vec3::new(5.0, 2.0 * side, 0.0),
            ..Default::default()
        },
    );
    for _ in 0..240 {
        physics.step_fixed();
    }
    let mesh_contact_started = physics.collision_events.iter().any(|event| {
        event.started
            && ((event.entity_a == floor && event.entity_b == ball)
                || (event.entity_a == ball && event.entity_b == floor))
    });
    let (hit, ray_distance, ray_normal) = physics
        .raycast(Vec3::new(5.75, 3.0 * side, 0.0), Vec3::NEG_Y * side, 5.0)
        .expect("ray must hit the second mesh floor");
    assert_eq!(hit, floor);
    let stats = physics.collider_cooking_stats();
    SimulationResult {
        position: from_rapier_vec(physics.rigid_body_set[body].translation()),
        velocity: from_rapier_vec(physics.rigid_body_set[body].linvel()),
        mesh_contact_started,
        ray_distance,
        ray_normal,
        cooks: stats.mesh_cooks,
        hits: stats.cache_hits,
    }
}

#[test]
fn cached_and_uncached_cooking_preserve_mesh_contacts_and_rays() {
    // Both sides of the thin surface keep the original contact/ray behavior.
    for side in [1.0, -1.0] {
        let cached = simulate_contacts(CookingMode::Cached, side);
        let uncached = simulate_contacts(CookingMode::Disabled, side);
        let original = simulate_contacts(CookingMode::Original, side);
        assert!(
            cached.mesh_contact_started
                && uncached.mesh_contact_started
                && original.mesh_contact_started
        );
        assert!((cached.position.y - 0.5 * side).abs() < 0.1);
        for reference in [&uncached, &original] {
            assert!((cached.position - reference.position).length() < 1e-4);
            assert!((cached.velocity - reference.velocity).length() < 1e-4);
            assert!((cached.ray_distance - reference.ray_distance).abs() < 1e-5);
            assert!((cached.ray_normal - reference.ray_normal).length() < 1e-5);
        }
        assert_eq!((cached.cooks, cached.hits), (1, 1));
        assert_eq!((uncached.cooks, uncached.hits), (2, 0));
        assert_eq!((original.cooks, original.hits), (0, 0));
    }
}
