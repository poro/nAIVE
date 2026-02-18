#!/usr/bin/env node
import { Client, handle_file } from "@gradio/client";
import { InferenceClient } from "@huggingface/inference";
import { promises as fs } from "fs";
import path from "path";

const HF_TOKEN = process.env.HF_TOKEN;
const GATEWAY_URL = process.env.GATEWAY_URL; // e.g. http://192.168.1.100:8000
const GATEWAY_KEY = process.env.GATEWAY_KEY; // e.g. changeme-dev-key-001
const PROMPT = process.argv[2] || "red sports car";
const OUT_DIR = path.resolve(process.argv[3] || "../../project/assets/meshes");

// ---------------------------------------------------------------------------
// Step 1: Text → 2D image (HF Inference API)
// ---------------------------------------------------------------------------
async function generate2DImage(prompt, outPath) {
  try {
    const existing = await fs.readFile(outPath);
    console.log(`[1/2] Reusing 2D image (${(existing.length / 1024).toFixed(0)} KB)\n`);
    return existing;
  } catch { /* not cached, generate */ }

  console.log("[1/2] Generating 2D image...");
  const hf = new InferenceClient(HF_TOKEN);
  const image = await hf.textToImage({
    model: "black-forest-labs/FLUX.1-schnell",
    inputs: `${prompt}, high detailed, complete object, not cut off, white solid background, 3d game asset`,
    parameters: { num_inference_steps: 8 },
  });
  const buf = Buffer.from(await image.arrayBuffer());
  await fs.writeFile(outPath, buf);
  console.log(`  Saved (${(buf.length / 1024).toFixed(0)} KB)\n`);
  return buf;
}

// ---------------------------------------------------------------------------
// Step 2a: 2D → 3D via local gateway (your H100 server)
// ---------------------------------------------------------------------------
async function generateViaGateway(imageBuffer, outDir) {
  if (!GATEWAY_URL || !GATEWAY_KEY) return null;

  console.log(`  Trying gateway: ${GATEWAY_URL}...`);
  const headers = { "X-API-Key": GATEWAY_KEY, "Content-Type": "application/json" };

  // Submit job
  const submitResp = await fetch(`${GATEWAY_URL}/api/v1/hunyuan-3d`, {
    method: "POST",
    headers,
    body: JSON.stringify({
      image_base64: imageBuffer.toString("base64"),
      steps: 5,
      seed: 1234,
    }),
  });
  if (!submitResp.ok) {
    throw new Error(`Gateway submit failed: ${submitResp.status} ${await submitResp.text()}`);
  }
  const { job_id } = await submitResp.json();
  console.log(`  Job submitted: ${job_id}`);

  // Poll for completion
  const startTime = Date.now();
  const TIMEOUT_MS = 5 * 60 * 1000; // 5 min max
  while (Date.now() - startTime < TIMEOUT_MS) {
    const statusResp = await fetch(`${GATEWAY_URL}/api/v1/jobs/${job_id}`, { headers });
    const status = await statusResp.json();
    if (status.status === "completed") {
      console.log(`  Completed in ${((Date.now() - startTime) / 1000).toFixed(1)}s`);
      // Download GLB
      const glbResp = await fetch(`${GATEWAY_URL}/api/v1/results/${job_id}`, { headers });
      if (!glbResp.ok) throw new Error(`Download failed: ${glbResp.status}`);
      const buf = Buffer.from(await glbResp.arrayBuffer());
      const outPath = path.join(outDir, "generated_3d.glb");
      await fs.writeFile(outPath, buf);
      console.log(`  Saved: ${outPath} (${(buf.length / 1024).toFixed(0)} KB)`);
      return [outPath];
    }
    if (status.status === "failed") {
      throw new Error(`Job failed: ${status.error || "unknown"}`);
    }
    // Still processing
    const elapsed = ((Date.now() - startTime) / 1000).toFixed(0);
    process.stdout.write(`\r  Processing... ${elapsed}s (${status.status})`);
    await new Promise(r => setTimeout(r, 3000));
  }
  throw new Error("Timeout waiting for gateway result");
}

// ---------------------------------------------------------------------------
// Step 2b: 2D → 3D via HuggingFace Gradio space (fallback)
// ---------------------------------------------------------------------------
async function generateViaHFSpace(imageBuffer, outDir) {
  const imageFile = handle_file(new Blob([imageBuffer], { type: "image/png" }));
  const spaceName = process.env.MODEL_SPACE || "tencent/Hunyuan3D-2mini-Turbo";

  console.log(`  Trying HF Space: ${spaceName}...`);
  const client = await Client.connect(spaceName, { hf_token: HF_TOKEN });
  try { await client.predict("/on_gen_mode_change", ["Turbo"]); } catch {}
  const result = await client.predict("/generation_all", {
    caption: PROMPT, image: imageFile,
    steps: 5, guidance_scale: 5.0, seed: 1234, octree_resolution: 256,
    check_box_rembg: true, num_chunks: 8000, randomize_seed: true,
  });

  let saved = [];
  for (let i = 0; i < result.data.length; i++) {
    const item = result.data[i];
    const url = item?.url || item?.value?.url;
    if (!url) continue;
    let ext = path.extname(new URL(url).pathname).toLowerCase();
    if (!ext || ext.length > 5) ext = ".glb";
    const outPath = path.join(outDir, `generated_3d${saved.length > 0 ? `_${i}` : ""}${ext}`);
    const resp = await fetch(url, { headers: { Authorization: `Bearer ${HF_TOKEN}` } });
    if (resp.ok) {
      const buf = Buffer.from(await resp.arrayBuffer());
      await fs.writeFile(outPath, buf);
      console.log(`  Saved: ${outPath} (${(buf.length / 1024).toFixed(0)} KB)`);
      saved.push(outPath);
    }
  }
  return saved;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
async function main() {
  await fs.mkdir(OUT_DIR, { recursive: true });
  console.log(`Prompt: "${PROMPT}"`);
  console.log(`Gateway: ${GATEWAY_URL || "(not configured)"}\n`);

  // Step 1: Generate 2D image
  const imagePath = path.join(OUT_DIR, "generated_2d.png");
  const imageBuffer = await generate2DImage(PROMPT, imagePath);

  // Step 2: Generate 3D model
  console.log("[2/2] Generating 3D model...");

  // Try gateway first (local H100 server)
  if (GATEWAY_URL) {
    try {
      const saved = await generateViaGateway(imageBuffer, OUT_DIR);
      if (saved && saved.length > 0) {
        console.log("\nSuccess via gateway!");
        return;
      }
    } catch (e) {
      console.log(`\n  Gateway failed: ${e.message.substring(0, 150)}\n`);
    }
  }

  // Fallback to HuggingFace Space
  try {
    const saved = await generateViaHFSpace(imageBuffer, OUT_DIR);
    if (saved.length > 0) {
      console.log("\nSuccess via HuggingFace!");
      return;
    }
  } catch (e) {
    console.log(`  HF Space failed: ${e.message.substring(0, 150)}\n`);
  }

  console.error("\nAll generation methods failed.");
  console.error("The 2D image was saved at: " + imagePath);
  console.error("Configure GATEWAY_URL + GATEWAY_KEY in .env for local H100 generation.");
  process.exit(1);
}

main().catch(err => { console.error(`ERROR: ${err.message}`); process.exit(1); });
