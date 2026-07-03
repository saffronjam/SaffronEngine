// Morph perf budget: import a high-target-count grid mesh, drive all its weights non-zero so
// the morph scatter dispatch runs at scale, and confirm the morph compute pass shows up in the
// per-pass profiler. The pass is timed for free by being a named render-graph pass; we assert
// it is present with gpuMs >= 0, and only bound its magnitude on a real (non-llvmpipe) GPU.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import type { ProfilerModeResult, RenderPassTimingsDto } from "@saffron/protocol";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let caps: ProfilerModeResult;
let morphId = "";
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "MorphStressTest.gltf");

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

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.importEntity(FIXTURE);
  morphId = await morphEntity(engine);
  await engine.call("focus", { entity: morphId });
  // Drive every morph target non-zero so the scatter dispatch processes a high active count.
  const got = await engine.call<{ weights: number[] }>("get-morph-weights", { entity: morphId });
  await engine.call("set-morph-weights", {
    entity: morphId,
    weights: got.weights.map(() => 1),
  });
  caps = await engine.call<ProfilerModeResult>("profiler.set-mode", { args: ["timestamps"] });
  await engine.settle(500);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("the stress mesh seeds one weight per morph target", async () => {
  const got = await engine.call<{ weights: number[] }>("get-morph-weights", { entity: morphId });
  // The generator builds N=16 morph targets (one per grid row).
  expect(got.weights.length).toBe(16);
});

test("the morph compute pass is present and timed", async () => {
  const timings = await engine.call<RenderPassTimingsDto>("pass-timings");
  if (!caps.timestampsSupported) {
    return; // device cannot time passes; nothing to assert (mirrors perf.test.ts)
  }
  const morph = timings.passes.find((p) => p.name === "morph");
  expect(morph).toBeDefined();
  expect(morph!.gpuMs).toBeGreaterThanOrEqual(0);
  // A magnitude bound is only meaningful on a real GPU; llvmpipe timings are not a budget.
  if (!caps.softwareGpu) {
    expect(morph!.gpuMs).toBeLessThan(5);
  }
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
