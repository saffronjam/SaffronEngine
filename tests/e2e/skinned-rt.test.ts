// Skinned ray tracing: with hardware ray-query shadows on, an animated rig's BLAS is refit
// every frame from the deformed-vertex buffer (Phase 9) and referenced in the per-frame TLAS
// with an identity transform. This drives the full refit -> TLAS build -> ray-query path with a
// deforming mesh and asserts it stays validation-clean — the AS-build synchronization (skin
// compute write -> AS build read, BLAS build -> TLAS build) is the thing under test. The
// software lavapipe device advertises accelerationStructure + rayQuery, so the path runs here;
// before the deformed buffer carried SHADER_DEVICE_ADDRESS usage this run flagged 20+ VUIDs, so
// a clean log here is a real signal the refit path executed and is correct.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
let rtToggleOk = false;
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const shots: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  // set-rt-shadows resolves only when the device supports ray-query (lavapipe does); its result
  // reflects whether shadows are *currently active*, which needs a built TLAS, so it is false
  // this early — we only care that the toggle was accepted (the call did not reject).
  rtToggleOk = await engine
    .call("set-rt-shadows", { enabled: true })
    .then(() => true)
    .catch(() => false);
  const imported = await engine.importEntity(FIXTURE);
  meshId = imported.id;
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-skinrt-${process.pid}-${tag}.png`;
  shots.push(path);
  await engine.call("screenshot", { target: "viewport", path });
  const deadline = Date.now() + 10_000;
  while (!existsSync(path)) {
    if (Date.now() > deadline) {
      throw new Error(`screenshot ${tag} never landed at ${path}`);
    }
    await engine.settle(100);
  }
  await engine.settle(200);
  return readFileSync(path);
}

test("ray-query shadows toggle is accepted on this device", () => {
  expect(rtToggleOk).toBe(true);
});

test("the rig plays many frames with the per-frame skinned BLAS refit running", async () => {
  await engine.call("set-component-field", {
    entity: meshId,
    component: "AnimationPlayer",
    field: "playing",
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
