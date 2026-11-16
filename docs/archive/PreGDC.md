# Pre-GDC Plan (March 7–8, 2026)

**Goal:** Have a compelling 60-second demo ready for GDC conversations (March 9–13).

---

## 1. Fix `game-asset-mcp` Submodule

The submodule directory is currently empty.

```bash
cd /home/p0r0/clawd/projects/naive
git submodule update --init --recursive
```

If the upstream repo (`MubarakHAlketbi/game-asset-mcp`) is gone or broken, fork it to `poro/game-asset-mcp` and update `.gitmodules`.

**Verify:** `ls tools/game-asset-mcp/` should show actual files, not an empty directory.

---

## 2. Verify AI Asset Pipeline End-to-End

The pipeline: **text prompt → FLUX.1 2D image → Hunyuan3D GLB → nAIVE scene**

```bash
# Set up environment
cp .env.example .env
# Fill in: HF_TOKEN, GATEWAY_URL, GATEWAY_KEY (if H100 available)

# Test generation
node scripts/test_generate_asset.js "red sports car"

# Verify output
ls project/assets/meshes/*.glb
```

**Check:**
- [ ] 2D image generates via HF Inference API (FLUX.1-schnell)
- [ ] 3D GLB generates via gateway (local H100) or HuggingFace Spaces (Hunyuan3D)
- [ ] GLB loads in nAIVE with vertex colors and smooth normals
- [ ] Turntable Lua script (`project/logic/ai_asset_turntable.lua`) works

---

## 3. Verify Gaussian Splat Rendering

```bash
# Run the splat demo
cargo run --bin naive -- demo gaussians
```

**Check:**
- [ ] Splat demo renders without crashes
- [ ] Camera controls work (orbit, zoom)
- [ ] Performance is acceptable (>30 FPS)

---

## 4. Test All 15 Demos

```bash
cargo run --bin naive -- demo
# Run through each demo, verify none crash
```

Quick pass — just confirm they load and render. Note any that crash or look broken.

---

## 5. Record Demo Video (60 seconds)

**Script:**

| Time | What to Show | Narration |
|------|-------------|-----------|
| 0–10s | `naive demo` browser, click through 2-3 demos | "nAIVE is an AI-native game engine. Everything is YAML, Lua, and natural language." |
| 10–25s | Gaussian splat rendering demo | "Native Gaussian Splatting support — photorealistic environments from text prompts." |
| 25–40s | AI asset generation: type a prompt, watch GLB appear | "Type what you want. AI generates the 3D asset. It's in your scene in seconds." |
| 40–55s | Live scene editing via `naive edit` — move objects, change materials | "Hot-reload everything. Shaders in 200ms. Scenes in 100ms. Scripts in 50ms." |
| 55–60s | Title card: nAIVE — The AI-Native Game Engine | "Open source. MIT licensed. Built in Rust." |

**Tools:** OBS or QuickTime screen recording. 1080p minimum.

---

## 6. Prepare Talking Points

**For Epic/Unity people:**
- "We have native Gaussian Splatting — no plugins needed"
- "AI generates game assets from text. The engine is the editor."
- "Sub-second hot-reload on everything — 100x faster than Unity or Unreal"
- "MIT licensed engine, BSL server. No royalties."

**For educators/ASU connections:**
- "Students describe what they want in natural language, AI builds it"
- "YAML scenes are human-readable — perfect for learning game dev concepts"
- "Lua scripting with a clean API — accessible to beginners"

**For investors/industry:**
- "Rust + wgpu = runs on everything (macOS, Windows, Linux, WebGPU)"
- "50K GPU entities planned for March — that's our technical moat"
- "First engine designed from scratch for AI-generated content"

---

## Timeline

| When | Task | Time Estimate |
|------|------|--------------|
| **Sat Mar 7 AM** | Fix submodule, verify asset pipeline | 1 hour |
| **Sat Mar 7 PM** | Test all demos, fix any crashes | 2 hours |
| **Sun Mar 8 AM** | Record demo video, iterate on script | 2 hours |
| **Sun Mar 8 PM** | Prepare talking points, share video on LinkedIn | 1 hour |
| **Mon Mar 9** | GDC Day 1 — video on phone, ready to show | ✈️ |

---

## Success Criteria

- [ ] `game-asset-mcp` submodule initialized and working
- [ ] AI asset pipeline generates a GLB from text prompt
- [ ] Gaussian splat demo renders cleanly
- [ ] All 15 demos run without crashes
- [ ] 60-second demo video recorded and shareable
- [ ] Talking points memorized for Epic, Unity, educator, and investor audiences
