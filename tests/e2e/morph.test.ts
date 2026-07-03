// Morph targets end to end: import a cube with one blend shape ("bulge") + a weights clip,
// round-trip the weights over the control plane (a written 0..1 vector reads back identically),
// and prove the GPU deform actually moves geometry — playing the weight clip changes the
// rendered silhouette across frames. Closes Phase 4/6 against the running engine.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let morphId = "";
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "AnimatedMorphCube.gltf");
const shots: string[] = [];

/// The mesh-bearing entity carrying the durable `Morph` component (import seeds it on the
/// mesh node, which the single-node model may collapse onto the instantiated root).
async function morphEntity(engine: Engine): Promise<string> {
  const { entities } = await engine.call<{ entities: { id: string }[] }>("list-entities");
  for (const e of entities) {
    const info = await engine.call<{ components: Record<string, unknown> }>("inspect", {
      entity: e.id,
    });
    if (info.components.Morph) {
      return e.id;
    }
  }
  throw new Error("no entity carries a Morph component");
}

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-morph-${process.pid}-${tag}.png`;
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

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.importEntity(FIXTURE);
  morphId = await morphEntity(engine);
  await engine.call("focus", { entity: morphId });
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

test("the morph mesh seeds rest weights + names", async () => {
  const got = await engine.call<{ weights: number[]; names: string[] }>("get-morph-weights", {
    entity: morphId,
  });
  expect(got.weights).toEqual([0]);
  expect(got.names).toEqual(["bulge"]);
});

test("set-morph-weights round-trips a 0..1 vector", async () => {
  const set = await engine.call<{ weights: number[] }>("set-morph-weights", {
    entity: morphId,
    weights: [0.75],
  });
  expect(set.weights).toEqual([0.75]);
  const got = await engine.call<{ weights: number[] }>("get-morph-weights", { entity: morphId });
  expect(got.weights).toEqual([0.75]);
});

test("a wrong-length weight vector is rejected", async () => {
  let rejected = false;
  try {
    await engine.call("set-morph-weights", { entity: morphId, weights: [0.1, 0.2] });
  } catch {
    rejected = true;
  }
  expect(rejected).toBe(true);
});

test("playing the weight clip deforms the geometry on the GPU", async () => {
  // Rest (weight 0) — the cube is undeformed.
  await engine.call("set-morph-weights", { entity: morphId, weights: [0] });
  await engine.settle(200);
  const rest = await screenshot("rest");
  // Full bulge (weight 1) — the top face lifts by +1, a large silhouette change. If the morph
  // compute pass did not run, the two frames would be identical.
  await engine.call("set-morph-weights", { entity: morphId, weights: [1] });
  await engine.settle(300);
  const bulged = await screenshot("bulged");
  expect(bulged.equals(rest)).toBe(false);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
