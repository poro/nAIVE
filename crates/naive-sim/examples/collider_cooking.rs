//! Compare one cold collider cook with repeated adds of unchanged geometry.
//! Run with: cargo run --release -p naive-sim --example collider_cooking -- 64

use std::time::{Duration, Instant};

use glam::{Quat, Vec3};
use naive_sim::assets::load_mesh;
use naive_sim::physics::{PhysicsShape, PhysicsWorld};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let count = match args.next().as_deref() {
        None => 64,
        Some("--help" | "-h") => {
            println!("Usage: collider_cooking [warm-add-count: 1..=10000, default 64]");
            return Ok(());
        }
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| "warm-add-count must be an integer from 1 through 10000")?,
    };
    if !(1..=10_000).contains(&count) || args.next().is_some() {
        return Err("expected at most one warm-add-count from 1 through 10000".into());
    }

    let mesh = load_mesh(&std::env::current_dir()?, "procedural:cube")?;
    let (vertices, indices) = mesh.physics_trimesh(Vec3::ONE);
    let shape = PhysicsShape::Trimesh { vertices, indices };
    let mut entities = hecs::World::new();
    let entity = entities.spawn(());
    let mut physics = PhysicsWorld::new(Vec3::ZERO);

    // Prepare and clone inputs before starting each timer. Removal is also
    // outside the timed section, so samples cover cooking and body creation.
    let cold_shape = shape.clone();
    let started = Instant::now();
    let (body, _) = physics.add_static_body(
        entity,
        Vec3::ZERO,
        Quat::IDENTITY,
        cold_shape,
        false,
        0.0,
        0.5,
    );
    let cold = started.elapsed();
    physics.remove_body(body);

    let mut warm = Vec::with_capacity(count);
    for _ in 0..count {
        let warm_shape = shape.clone();
        let started = Instant::now();
        let (body, _) = physics.add_static_body(
            entity,
            Vec3::ZERO,
            Quat::IDENTITY,
            warm_shape,
            false,
            0.0,
            0.5,
        );
        warm.push(started.elapsed());
        physics.remove_body(body);
    }
    warm.sort_unstable();
    let median = (warm[(count - 1) / 2] + warm[count / 2]) / 2;
    let stats = physics.collider_cooking_stats();
    println!("Geometry: procedural:cube, scale [1, 1, 1]");
    println!("Cold add: {:.3} ms", milliseconds(cold));
    println!(
        "Warm adds: {count}, median {:.3} ms, max {:.3} ms",
        milliseconds(median),
        milliseconds(warm[count - 1]),
    );
    println!(
        "Cooking stats: mesh_cooks={}, cache_hits={}, cached_meshes={}, cached_geometry_bytes={}, evictions={}",
        stats.mesh_cooks,
        stats.cache_hits,
        stats.cached_meshes,
        stats.cached_geometry_bytes,
        stats.evictions,
    );
    println!("Expected unchanged-geometry counts: mesh_cooks=1, cache_hits={count}");
    println!("Timings are informational; no pass/fail threshold is applied.");
    if stats.mesh_cooks != 1 || stats.cache_hits != count as u64 {
        return Err(format!(
            "unchanged geometry expected one cook and {count} cache hits; observed {} cooks and {} hits",
            stats.mesh_cooks, stats.cache_hits,
        )
        .into());
    }
    Ok(())
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
