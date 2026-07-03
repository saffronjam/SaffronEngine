// Skinned motion vectors under TAA: with anti-aliasing set to TAA the motion-vector prepass
// runs and emits velocity for skinned geometry (a second deformed buffer skinned with last
// frame's palette + per-instance prevModel). This proves the host loop drives the animated
// rig through the motion pass with TAA active and stays validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const shots: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  await engine.call("set-aa", { mode: "taa" });
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
  const path = `/tmp/saffron-e2e-skinmotion-${process.pid}-${tag}.png`;
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

test("TAA is active and the rig plays through the motion pass", async () => {
  await engine.call("set-component-field", {
    entity: meshId,
    component: "AnimationPlayer",
    field: "playing",
    value: true,
  });
  await engine.call("focus", { entity: meshId });
  await engine.call("play");
  // Many frames so the motion pass sees a moving bone across consecutive frames and TAA
  // accumulates history against the skinned velocity.
  await engine.settle(800);
  const moving = await screenshot("moving");
  await engine.settle(400);
  const later = await screenshot("later");
  // The animation keeps deforming, so two shots a few hundred ms apart differ — the rig is
  // genuinely moving while the motion pass + TAA run.
  expect(later.equals(moving)).toBe(false);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
