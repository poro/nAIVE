# Codex Refactoring Recommendations

## Address Critical Weaknesses

1. **Scene inheritance resolution**  
   - Location: `crates/naive-core/src/scene.rs:382`  
   - Issue: `resolve_inheritance` merges only direct parents; multi-level `extends` loses inherited components/tags and can allow cyclic references to slip through.  
   - Recommendation: Replace the single-pass merge with a depth-first resolver that walks ancestor chains, caches resolved entities, and detects cycles properly (e.g., DFS with visitation states). Add regression tests covering grandparent inheritance and cycle detection.

2. **Physics integration during scene hot-reload**  
   - Location: `crates/naive-client/src/world.rs:971`  
   - Issue: Newly spawned entities on hot-reload bypass `PhysicsWorld`, so colliders/rigid bodies are never registered until a full restart.  
   - Recommendation: Thread an optional `PhysicsWorld` reference through the spawn path during hot reload (mirroring initial load) and ensure physics components are added when available. Include a smoke test that hot-reloads a scene with new colliders.

3. **Component patch coverage on hot-reload**  
   - Location: `crates/naive-client/src/world.rs:998`  
   - Issue: `patch_entity` updates only transforms, cameras, and point lights. Other mutable components (materials, physics, scripts, splats, etc.) remain stale.  
   - Recommendation: Expand `patch_entity` (or refactor into per-component patchers) to reconcile all component types or adopt a diff-driven respawn strategy. Add tests to confirm YAML edits propagate to active entities without restart.

4. **File watcher deletion handling**  
   - Location: `crates/naive-client/src/watcher.rs:27`  
   - Issue: Removes are ignored, so deleting assets leaves cached data live with no feedback.  
   - Recommendation: Handle `EventKind::Remove` (and `ModifyKind::Name`) by emitting the same `WatchEvent`, then teach higher layers to clear caches or emit user-facing warnings when backing files disappear.

## Reinforce Strengths

1. **Scene/test automation**  
   - Enhancement: Extend the headless `TestRunner` to include sample regression cases for inheritance, hot-reload, and physics syncing. This leverages the existing test harness to protect the fast iteration workflow.

2. **Render pipeline flexibility**  
   - Enhancement: Document a template for custom pass authoring that highlights the YAML DAG plus Slang shaders, and provide a scripted example that can be dropped into new projects. This capitalizes on the modular rendering architecture.

3. **Subsystem modularity**  
   - Enhancement: Formalize module boundaries by adding brief README snippets (or rustdoc module headers) explaining responsibilities and extension points for key subsystems (renderer, physics, scripting). This helps onboard contributors while keeping the current clean split between `naive-core` and `naive-client`.

4. **Hot-reload UX**  
   - Enhancement: Surface reload status and errors in the HUD/UI overlay so users immediately see when shaders, scenes, or scripts recompile successfully versus failing—building on the existing watcher infrastructure.
