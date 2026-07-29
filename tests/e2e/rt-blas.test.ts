// Bottom-level acceleration structures on a ray-query device: many instances of one mesh share a
// single BLAS, and each mesh's structure is compacted to its exact size before it is kept.
//
// The sharing claim is what `render-stats` exposes: `rtInstances` counts TLAS instances while
// `blasCount` counts the distinct structures they reference, so instancing shows up as the two
// numbers diverging. Adding N copies of one mesh must move `rtInstances` by N and leave `blasCount`
// where it was — if every instance built its own structure the two would move together.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { RenderStats } from "@saffron/protocol";
import { bootEngine, Cleaner, prepareScene } from "./test-utils.ts";

let engine: Engine;
let rtSupported = false;
const cleaner = new Cleaner();

async function stats(): Promise<RenderStats> {
  return engine.call<RenderStats>("render-stats");
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
  rtSupported = (await stats()).rtSupported;
  if (rtSupported) {
    // Arming ray-query shadows is what makes the host build a TLAS at all; without it there is
    // nothing to count.
    await engine.call("set-rt-shadows", { enabled: true });
    await engine.settle(400);
  }
});
afterAll(async () => {
  await cleaner.cleanup();
});

test("instances of one mesh share a single bottom-level structure", async () => {
  if (!rtSupported) {
    return;
  }
  const before = await stats();
  expect(before.rtInstances).toBeGreaterThan(0);
  expect(before.blasCount).toBeGreaterThan(0);

  const added = 4;
  for (let i = 0; i < added; i += 1) {
    await engine.call("add-entity", { preset: "cube" });
  }
  await engine.settle(500);

  const after = await stats();
  expect(after.rtInstances).toBe(before.rtInstances + added);
  // The cube preset resolves to the one built-in cube mesh, so the four instances contribute at
  // most one structure between them — whether that mesh was already referenced depends on the
  // starter scene, so the bound is "one more", not "no more". Per-instance structures would have
  // added four.
  expect(after.blasCount).toBeGreaterThanOrEqual(before.blasCount);
  expect(after.blasCount).toBeLessThanOrEqual(before.blasCount + 1);
  expect(after.rtInstances - after.blasCount).toBeGreaterThanOrEqual(added - 1);
});

test("the ray-tracing scene stays validation-clean while the structures are shared", () => {
  if (!rtSupported) {
    return;
  }
  expect(engine.validationErrors()).toEqual([]);
});
