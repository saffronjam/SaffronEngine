// Skinned ray tracing: on a ray-query device, an animated rig's BLAS is refit every frame from the
// deformed-vertex buffer and referenced in the per-frame TLAS. The test exercises the refit -> TLAS
// build -> ray-query synchronization when the active Vulkan device exposes the required features.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { RenderStats } from "@saffron/protocol";
import { bootEngine, captureViewport, Cleaner, prepareScene } from "./test-utils.ts";

let engine: Engine;
let meshId = "";
let rtToggleOk = false;
let rtSupported = false;
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const cleaner = new Cleaner();

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
  rtSupported = (await engine.call<RenderStats>("render-stats")).rtSupported;
  rtToggleOk = await engine
    .call("set-rt-shadows", { enabled: true })
    .then(() => true)
    .catch(() => false);
  const imported = await engine.importEntity(FIXTURE);
  meshId = imported.id;
  await engine.settle();
});
afterAll(async () => {
  await cleaner.cleanup();
});

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `skinrt-${tag}`);
}

test("ray-query shadows follow the device capability gate", () => {
  expect(rtToggleOk).toBe(rtSupported);
});

test("the rig plays many frames with the per-frame skinned BLAS refit running", async () => {
  if (!rtSupported) {
    return;
  }
  await engine.call("set-component-field", {
    entity: meshId,
    component: "AnimationPlayer",
    field: "autoplay",
    value: true,
  });
  await engine.call("focus", { entity: meshId });
  await engine.call("play");
  // Drive a long stretch of frames so the BLAS is BUILT once then UPDATE-refit every subsequent
  // frame while the pose changes, the TLAS rebuilds, and the mesh fragment traces ray-query
  // shadows against it. The screenshots force the present path so frames genuinely advance.
  await engine.settle(800);
  const a = await screenshot("a");
  await engine.settle(600);
  const b = await screenshot("b");
  // Both shots are real frames (non-empty); the validation assertion below is the real gate.
  expect(a.byteLength).toBeGreaterThan(0);
  expect(b.byteLength).toBeGreaterThan(0);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
