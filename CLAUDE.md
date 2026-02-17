# nAIVE Engine — Claude Code Context

## Project

AI-native game engine — create worlds with YAML, Lua, and natural language.

- **Language:** Rust (2021 edition)
- **Renderer:** wgpu + SLANG shaders
- **Physics:** Rapier3D
- **Scripting:** Lua 5.4 (mlua)
- **ECS:** hecs
- **Audio:** kira
- **Current version:** 0.1.8

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `naive-core` | Scene schema, shared types (YAML deserialization) |
| `naive-client` | Engine loop, renderer, physics, scripting, world management |
| `naive-runtime` | CLI binary (`naive`, `naive-runtime`, `naive_mcp`) |
| `naive-server` | Multiplayer server (future) |

## Key Source Files

| File | Responsibility |
|------|---------------|
| `crates/naive-core/src/scene.rs` | YAML scene schema types (`SceneDef`, `EntityDef`, `RigidBodyDef`, etc.) |
| `crates/naive-client/src/engine.rs` | Main `Engine` struct, game loop, camera, scene loading |
| `crates/naive-client/src/physics.rs` | `PhysicsWorld` — Rapier3D wrapper (bodies, colliders, CCD, impulse/force) |
| `crates/naive-client/src/world.rs` | `SceneWorld` (ECS + registry), `EntityCommandQueue`, entity spawning, pooling |
| `crates/naive-client/src/scripting.rs` | Lua API registration (physics, entity, camera, scene, events) |
| `crates/naive-client/src/renderer.rs` | wgpu render pipeline, instance buffers, particles |
| `crates/naive-client/src/test_runner.rs` | Headless test runner for `naive test` CLI command |

## Architecture Patterns

- **Deferred commands:** Lua scripts push commands to `EntityCommandQueue`; engine processes them end-of-frame to avoid aliasing/dangling pointers.
- **Raw pointers for Lua closures:** Lua API closures capture `*mut PhysicsWorld` / `*mut SceneWorld` since mlua requires `'static` closures. Safe because Engine is pinned in the winit event loop.
- **In-place replacement for scene transitions:** `scene.load()` clears ECS and replaces physics world in-place (same memory address) so captured raw pointers remain valid.
- **Pool manager:** Entity recycling for projectiles and spawned entities to avoid allocation churn.

## Tier Status

| Tier | Description | Status |
|------|-------------|--------|
| Tier 1 | Gameplay Primitives (health, damage, hitscan, projectiles, third-person camera) | **DONE v0.1.2** |
| Tier 2 | Production Foundations (dynamic instance buffer, entity lifecycle, pooling, particles, events) | **DONE v0.1.4** |
| Tier 2.5 | Physics & Scene API (impulse/force, velocity, CCD, collider materials, entity tags, camera shake, scene loading) | **DONE v0.1.7** |
| Tier 3 | GPU Scale (50K+ GPU compute entities, neighbor-grid collisions, flow field) | Planned |
| Tier 4 | Animation & Polish (skeletal animation, VAT, UI) | Planned |

## Building

```sh
# Requires SLANG SDK in vendor/ (see homebrew formula or download from shader-slang/slang releases)
export SLANG_DIR=vendor
cargo build
cargo test
```

## Homebrew

Formula lives at `../homebrew-tap/Formula/naive.rb`. Update URL, SHA256, and version on each release.
