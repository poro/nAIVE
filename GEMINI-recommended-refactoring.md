# GEMINI-Recommended Refactoring: nAIVE Engine

**Date:** March 2026
**Status:** Strategic Proposal
**Target Version:** v0.2.0

This document outlines a comprehensive refactoring plan for the nAIVE engine to address technical debt, improve safety, and prepare for the **Tier 3 (GPU Scale)** milestone.

---

## 1. Modularization of "God Modules"

The current codebase contains two extremely large files that hinder maintainability and slow down compilation: `engine.rs` (~3,500 lines) and `pipeline.rs` (~3,100 lines).

### Phase 1.1: `naive-client/src/pipeline.rs`
**Goal:** Split the monolithic rendering pipeline into a structured module.
*   `pipeline/mod.rs`: Public API (`CompiledPipeline`, `execute_pipeline`).
*   `pipeline/def.rs`: YAML serialization structs (`PipelineFile`, `PassDef`).
*   `pipeline/resource.rs`: GPU resource allocation and resize logic (`GpuResource`).
*   `pipeline/compiler.rs`: Logic for creating `wgpu` pipelines from SLANG/WGSL.
*   `pipeline/executor.rs`: Individual pass execution logic (`execute_rasterize_pass`, etc.).

### Phase 1.2: `naive-client/src/engine.rs`
**Goal:** Extract specialized logic from the main `Engine` struct.
*   `engine/mod.rs`: `Engine` struct and `ApplicationHandler` implementation.
*   `engine/init.rs`: GPU and window initialization logic.
*   `engine/assets.rs`: Scene loading (`load_scene`) and asset cache management.
*   `engine/tick.rs`: The core simulation loop (`tick`, `update_physics`).
*   `engine/render.rs`: The frame orchestration logic (uniform updates and pipeline execution).

---

## 2. Safety Audit: The "Raw Pointer Bridge"

### Investigation Findings
The architecture documentation (`ARCHITECTURE.md`) references a **"raw pointer bridge (`*mut PhysicsWorld`, `*mut SceneWorld`)"** for zero-cost Lua access. However, the current implementation in `scripting.rs` uses **`Rc<RefCell<T>>`**.

### Recommendations
1.  **Discard Raw Pointers:** Do **not** implement the raw pointer bridge. While it offers "zero-cost" access, it introduces significant risk of use-after-free or race conditions that `Rc<RefCell<T>>` prevents at a negligible runtime cost.
2.  **Update Documentation:** Align `ARCHITECTURE.md` with the actual `scripting.rs` implementation to prevent confusion for new contributors.
3.  **Prepare for Multi-threading:** If Tier 3 requires offloading systems to worker threads, transition from `Rc<RefCell<T>>` to `Arc<RwLock<T>>`.

---

## 3. Build Ergonomics & Dependency Management

The current requirement to manually `curl` the SLANG SDK into `vendor/` is a friction point for new developers and CI pipelines.

### Proposed Plan
1.  **Automated Setup:** Create a `build.rs` script in `naive-client` that checks for the SLANG SDK and automatically downloads the correct version for the host platform (ARM/Intel macOS, Linux, Windows).
2.  **Environment Variables:** Use `include_str!` or a build-time constant to embed critical shader paths, reducing runtime path resolution errors.

---

## 4. Tier 3 Preparation (Scaling to 50K Entities)

To achieve the 50,000 entity goal, the engine must move away from per-entity CPU overhead.

### Refactoring Steps
1.  **Batch Scripting:** Instead of each entity having its own Lua environment and `update` call, move toward a "System" approach where one Lua call can process an entire component group (e.g., `scripts.update_all_particles(dt)`).
2.  **GPU Compute Migration:** Start moving the `ParticleSystem` and `Transform` hierarchy updates from CPU (Rust) to GPU Compute Shaders (SLANG).
3.  **Entity Command Queue Optimization:** The current `EntityCommandQueue` processes commands one by one. Refactor to a bulk-processing model to minimize cache misses during scene mutation.

---

## 5. Project & Engine Separation

Currently, the `project/` directory is mixed with engine source.

### Proposed Plan
*   Move `project/` to a dedicated `examples/default/` or `template/` folder.
*   Ensure the `naive` CLI can run a project from any directory by passing a `--path` flag, enforcing a clear boundary between "Engine" and "Game Data".

---

## Summary of Action Items

| Priority | Task | Target File(s) |
| :--- | :--- | :--- |
| **High** | Split `pipeline.rs` into `src/pipeline/` | `pipeline.rs` |
| **High** | Split `engine.rs` into `src/engine/` | `engine.rs` |
| **Medium** | Implement `build.rs` for SLANG SDK | `naive-client/Cargo.toml` |
| **Medium** | Sync ARCHITECTURE.md with scripting.rs | `docs/ARCHITECTURE.md` |
| **Low** | Batch-oriented Lua API research | `scripting.rs` |
