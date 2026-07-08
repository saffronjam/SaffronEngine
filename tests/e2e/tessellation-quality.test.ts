// The `set-tessellation-quality` control command (Phase 9): drive the displacement-tessellation
// budget over the wire and assert the READ-BACK — the command echoes the applied, clamped budget, so
// "ok:true" alone is not the check. A displaced cube keeps the tessellation path live, and the suite
// asserts the engine stays Vulkan-validation-clean across the re-diced frames (the budget feeds the
// per-instance factor cap / min factor / edge-length target the emit kernel reserves against).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { SetTessellationQualityResult } from "@saffron/protocol";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("add-entity", { preset: "cube" });
});
afterAll(async () => {
  await engine?.shutdown();
});

const setQuality = (params: Record<string, unknown>) =>
  engine.call<SetTessellationQualityResult>("set-tessellation-quality", params);

test("set-tessellation-quality applies and echoes the budget", async () => {
  const r = await setQuality({ factorCap: 24, minFactor: 2, edgeLengthTarget: 8 });
  expect(r.factorCap).toBe(24);
  expect(r.minFactor).toBe(2);
  expect(r.edgeLengthTarget).toBe(8);
});

test("a partial request tunes only the named knobs", async () => {
  // Establish a known budget, then change only the edge-length target.
  await setQuality({ factorCap: 16, minFactor: 1, edgeLengthTarget: 12 });
  const r = await setQuality({ edgeLengthTarget: 5 });
  expect(r.edgeLengthTarget).toBe(5);
  expect(r.factorCap).toBe(16); // unchanged
  expect(r.minFactor).toBe(1); // unchanged
});

test("out-of-range values are clamped, not rejected", async () => {
  const r = await setQuality({ factorCap: 9999, minFactor: 0.1, edgeLengthTarget: 0 });
  expect(r.factorCap).toBe(2048); // cap ∈ [1, 2048] (the budget, not the cap, bounds dense scenes)
  expect(r.minFactor).toBe(1); // min ∈ [1, cap]
  expect(r.edgeLengthTarget).toBe(1); // edge target ≥ 1
});

test("a min factor above the cap is pinned to the cap", async () => {
  const r = await setQuality({ factorCap: 4, minFactor: 10 });
  expect(r.factorCap).toBe(4);
  expect(r.minFactor).toBe(4);
});

test("driving the tessellation budget stays validation-clean", async () => {
  // Sweep the budget while the displaced cube re-dices; the extra frames give the validation
  // layers something to flag if a budget change desyncs the transient reservation.
  await setQuality({ factorCap: 32, minFactor: 1, edgeLengthTarget: 4 });
  await engine.call("render-stats");
  await setQuality({ factorCap: 8, minFactor: 1, edgeLengthTarget: 16 });
  await engine.call("render-stats");
  expect(engine.validationErrors()).toEqual([]);
});
