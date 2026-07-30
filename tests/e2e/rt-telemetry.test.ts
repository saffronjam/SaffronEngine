// Acceleration-structure telemetry: what the structures cost, and which representation each
// instance selected.
//
// `blasCount` says how many structures exist and nothing about what they cost. These fields carry
// the rest: storage bytes for both tiers, the scratch held for their builds, the pre-compaction
// figure the same structures would have occupied, and the per-representation counts (static build,
// skinned refit, tessellated rebuild).
//
// The load-bearing assertion is sharing. Byte totals are deduplicated by device address, because a
// structure shared by N instances would otherwise be charged N times, reporting instancing as memory
// growth. Adding instances of an existing mesh must move the instance count and leave the byte total
// alone.
//
// Compaction is asserted as an inequality: `blasBuiltBytes >= blasBytes` always holds, and whether
// the difference is positive depends on the driver, which is free to decline to shrink a structure.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, prepareScene } from "./test-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;
let rtSupported = false;

async function stats(): Promise<RenderStatsDto> {
  return engine.call<RenderStatsDto>("render-stats");
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
  rtSupported = (await stats()).rtSupported;
  if (rtSupported) {
    // The TLAS is only built when ray-query shadows are armed; without it there is nothing
    // to measure.
    await engine.call("set-rt-shadows", { enabled: true });
    await engine.call("add-entity", { preset: "cube" });
    await engine.settle(500);
  }
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("both acceleration-structure tiers report their storage", async () => {
  if (!rtSupported) {
    return;
  }
  const s = await stats();
  expect(s.blasCount).toBeGreaterThan(0);
  // Storage is reported as a decimal string (the counts exceed what a JS number holds exactly).
  expect(Number(s.blasBytes)).toBeGreaterThan(0);
  expect(Number(s.tlasBytes)).toBeGreaterThan(0);
  // Scratch is grow-only and shared across builds, so it is reported apart from the structures.
  expect(Number(s.rtScratchBytes)).toBeGreaterThan(0);
});

test("compaction never reports a structure larger than its build reserved", async () => {
  if (!rtSupported) {
    return;
  }
  const s = await stats();
  // A driver may decline to shrink, so the saving may be zero — but it can never be negative.
  expect(Number(s.blasBuiltBytes)).toBeGreaterThanOrEqual(Number(s.blasBytes));
});

test("byte totals charge a shared structure once, not once per instance", async () => {
  if (!rtSupported) {
    return;
  }
  const before = await stats();
  const added = 6;
  for (let i = 0; i < added; i += 1) {
    await engine.call("add-entity", { preset: "cube" });
  }
  await engine.settle(600);
  const after = await stats();

  // The instances landed.
  expect(after.rtInstances).toBe(before.rtInstances + added);
  // They all resolve to the one built-in cube mesh, so they add no structure and no bytes.
  // Charging per instance would have multiplied the total instead.
  expect(after.blasCount).toBe(before.blasCount);
  expect(after.blasBytes).toBe(before.blasBytes);
  expect(engine.validationErrors()).toEqual([]);
});

test("each instance's selected representation is reported separately", async () => {
  if (!rtSupported) {
    return;
  }
  const s = await stats();
  // This scene is all static geometry: no skinned refits, no tessellated rebuilds. The fields
  // exist so a deforming scene can be told apart from this one.
  expect(s.skinnedBlasCount).toBe(0);
  expect(s.tessellatedBlasCount).toBe(0);
  expect(s.blasCount).toBeGreaterThan(0);
});

test("a pinned coarse cut swaps every instance to its aggregate structure, and fine swaps back", async () => {
  if (!rtSupported) {
    return;
  }
  // The cut override pins both representations at once: raster draws the root voxel bricks
  // and the TLAS packs the matching family-space aggregate — one instance per entity, no
  // per-use expansion.
  await engine.call("set-hierarchy-cut", { cut: "coarse" });
  await engine.settle(400);
  const coarse = await stats();
  expect(coarse.rtAggregateInstances).toBeGreaterThan(0);
  // Every static instance here is a cube whose root cut is aggregate-eligible.
  expect(coarse.rtAggregateInstances).toBe(coarse.rtInstances);

  await engine.call("set-hierarchy-cut", { cut: "fine" });
  await engine.settle(400);
  const fine = await stats();
  expect(fine.rtAggregateInstances).toBe(0);
  expect(fine.rtInstances).toBeGreaterThan(0);

  await engine.call("set-hierarchy-cut", { cut: "auto" });
  await engine.settle(200);
  expect(engine.validationErrors()).toEqual([]);
});

test("the opacity-micromap capability is reported and the device came up clean with it", async () => {
  if (!rtSupported) {
    return;
  }
  const s = await stats();
  // The capability is device-dependent, so its VALUE is not asserted — what is asserted is that
  // it is reported at all, and that a device which enabled the extension raised no validation
  // message doing so. Enabling an extension whose feature was not requested, or requesting a
  // feature the device does not expose, both surface here.
  expect(typeof s.ommSupported).toBe("boolean");
  // A micromap is only meaningful under an acceleration-structure build, so the capability may
  // never be true without ray tracing.
  if (s.ommSupported) {
    expect(s.rtSupported).toBe(true);
  }
  expect(engine.validationErrors()).toEqual([]);
});

test("the GI occluder list reports what it dropped rather than only logging it", async () => {
  const s = await stats();
  // The SDF list is gathered by an unculled scan and hard-capped, so past the cap occluders
  // vanish from global illumination with no hierarchy to coarsen into. A warning in the log
  // cannot be asserted on; this can. A scene this size must drop nothing.
  expect(s.sdfInstancesDropped).toBe(0);
});

test("the cluster-AS capability is reported, and plain meshes never use it", async () => {
  if (!rtSupported) {
    return;
  }
  const s = await stats();
  // Device-dependent, so its VALUE is not asserted — what is asserted is coherence: the
  // extension is only meaningful under acceleration structures, and a device that enabled
  // it raised no validation message doing so.
  expect(typeof s.clusterAsSupported).toBe("boolean");
  if (s.clusterAsSupported) {
    expect(s.rtSupported).toBe(true);
  }
  // Cluster composition covers assembly prototypes; every mesh here is a plain
  // single-prototype cube on the KHR triangle build. A nonzero count in this scene would
  // mean the selection leaked past assemblies. The assembly-side positive proof lives in
  // the canopy suite.
  expect(s.clusterBlasCount).toBe(0);
  expect(s.clasCount).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});
