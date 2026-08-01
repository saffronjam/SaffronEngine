// The persistent GPU scene's page residency runs in every host frame: a scene with a
// mesh entity registers the mesh's hierarchy pages, the guaranteed root streams from the
// source artifact through the page worker, and `gpu-scene-stats` reports the resident
// payload — all while the frame stays Vulkan-validation-clean.
//
// The file is also the stress bed for the visibility chain: rapid camera motion, cuts,
// teleports, viewport resizes, wind-bound stress, and page churn each invalidate a
// different piece of frame-to-frame state, and every one of them must leave the cut whole.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { GpuSceneMirrorStatsDto } from "@saffron/protocol";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

// A model outside the builtin primitives, so importing and deleting it registers and retires
// its own pages rather than sharing the cube's.
const CHURN_MODEL = join(REPO, "tests", "e2e", "fixtures", "multi-node.gltf");

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

async function stats(): Promise<GpuSceneMirrorStatsDto> {
  return await engine.call("gpu-scene-stats");
}

// Polls `gpu-scene-stats` until `settled` accepts a sample, returning the last one either
// way so the caller's assertions report the real state rather than a timeout.
async function awaitStats(
  settled: (sample: GpuSceneMirrorStatsDto) => boolean,
  timeoutMs = 15_000,
): Promise<GpuSceneMirrorStatsDto> {
  const deadline = Date.now() + timeoutMs;
  let current = await stats();
  while (Date.now() < deadline) {
    current = await stats();
    if (settled(current)) {
      return current;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return current;
}

// The whole-cut invariant every stress phase is scored by: the cube is on screen, the
// traversal emits its records, and no list ran out of room.
function expectWholeCut(sample: GpuSceneMirrorStatsDto): void {
  expect(sample.visibility.visible).toBeGreaterThan(0);
  expect(sample.visibility.records).toBeGreaterThan(0);
  expect(sample.visibility.overflowFlags).toBe(0);
  expect(sample.visibility.pressureFlags).toBe(0);
  expect(sample.pageResidency.resident).toBeGreaterThan(0);
}

const framed = { position: { x: 0, y: 1, z: 6 }, yaw: 0, pitch: 0 };

test("a mesh entity's hierarchy pages register and its root becomes resident", async () => {
  await engine.call("add-entity", { args: ["cube"] });

  // Registration is same-frame with the mirror sync; the root payload streams through
  // the worker within a few frames.
  const current = await awaitStats(
    (sample) => sample.pageResidency.registered > 0 && sample.pageResidency.resident > 0,
  );
  expect(current.pageResidency.registered).toBeGreaterThan(0);
  expect(current.pageResidency.resident).toBeGreaterThan(0);
  expect(current.pageResidency.residentBytes).toBeGreaterThan(0);
  expect(current.pageResidency.budgetBytes).toBeGreaterThan(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("the GPU visibility chain classifies the instance and emits draw records", async () => {
  // The cube from the previous test is on screen; the cull marks it visible, the
  // traversal emits at least one record for its resident root, and no list overflows.
  const current = await awaitStats(
    (sample) => sample.visibility.visible > 0 && sample.visibility.records > 0,
  );
  expectWholeCut(current);
  expect(engine.validationErrors()).toEqual([]);
});

test("rapid camera cuts and teleports keep the cut hole-free", async () => {
  // Hammer the camera: teleports far away and back, spins, and framing cuts. Each
  // jump invalidates occlusion history; the cull's history bypass + the retest +
  // survivor chain must re-emit the cube's records every time with no overflow.
  const jumps = [
    { position: { x: 0, y: 1, z: 9 }, yaw: 0, pitch: -5 },
    { position: { x: 400, y: 50, z: -300 }, yaw: 120, pitch: -40 },
    { position: { x: -2, y: 0.5, z: 3 }, yaw: -15, pitch: 5 },
    { position: { x: 0, y: 800, z: 0 }, yaw: 0, pitch: -89 },
    framed,
  ];
  for (const jump of jumps) {
    await engine.call("set-camera", jump);
    await new Promise((resolve) => setTimeout(resolve, 120));
  }

  // Settled back on the cube: the chain recovers the full cut, and every
  // return-to-cut crossfade the hammering started runs out (a fabricated
  // crossfade would keep re-arming and never settle to zero).
  const current = await awaitStats(
    (sample) =>
      sample.visibility.visible > 0 &&
      sample.visibility.records > 0 &&
      sample.visibility.transitioning === 0,
  );
  expectWholeCut(current);
  // The cube's hierarchy is a single node: no cut flip is possible, so the
  // camera hammering must never fabricate a representation crossfade.
  expect(current.visibility.transitioning).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("a resize storm rebuilds the pyramids without dropping the cut", async () => {
  // Every resize throws away the HZB pyramid pair and the per-view lists sized against
  // it. A frame that sampled a freed pyramid, or a cull that kept history words minted
  // at the old extent, shows up here as a missing record or a validation error.
  const extents = [
    { width: 200, height: 120 },
    { width: 641, height: 361 },
    { width: 64, height: 64 },
    { width: 1_280, height: 720 },
    { width: 480, height: 270 },
  ];
  for (const extent of extents) {
    const applied = await engine.call("set-viewport-size", {
      view: "scene",
      ...extent,
    });
    expect(applied.width).toBeGreaterThan(0);
    expect(applied.height).toBeGreaterThan(0);
    await new Promise((resolve) => setTimeout(resolve, 120));
  }

  const current = await awaitStats(
    (sample) => sample.visibility.visible > 0 && sample.visibility.records > 0,
  );
  expectWholeCut(current);
  expect(engine.validationErrors()).toEqual([]);
});

test("wind-bound stress widens the cull spheres without losing the instance", async () => {
  // The wind prepass writes a sway record per wind-flagged instance and the cull adds
  // its amplitude as bounds slack. A storm-scale field is the largest slack the cull
  // ever composes, and it must still contain the instance rather than push it out of
  // its own sphere.
  const storms = [
    { speed: 40, gust: 1 },
    { speed: 0, gust: 0 },
    { speed: 120, gust: 1 },
    { speed: 7, gust: 0.4 },
  ];
  for (const storm of storms) {
    await engine.call("set-wind", storm);
    await new Promise((resolve) => setTimeout(resolve, 150));
    expectWholeCut(
      await awaitStats(
        (sample) => sample.visibility.visible > 0 && sample.visibility.records > 0,
      ),
    );
  }
  await engine.call("set-wind", { speed: 0, gust: 0 });
  expect(engine.validationErrors()).toEqual([]);
});

test(
  "page churn recycles table slots and arena bytes without aliasing",
  async () => {
    // Import a multi-node model, place it, then remove it from the project — repeatedly.
    // Every import registers each mesh's pages and streams their payloads into the arena;
    // every removal unregisters them, retires their arena ranges, and frees the table slots
    // for the next round to take back at a bumped generation. A retire that gave the wrong
    // bytes back drifts `residentBytes` off its baseline; a record that kept a handle across
    // the recycle reads another mesh's payload, which the arena's own range checks and the
    // validation layers catch.
    const baseline = await awaitStats((sample) => sample.pageResidency.resident > 0);
    expect(baseline.visibility.records).toBeGreaterThan(0);

    for (let round = 0; round < 3; round += 1) {
      const model = await engine.call("import-model", { path: CHURN_MODEL });
      const placed = await engine.call("instantiate-model", { asset: model.id });
      const grown = await awaitStats(
        (sample) => sample.pageResidency.registered > baseline.pageResidency.registered,
        20_000,
      );
      expect(grown.pageResidency.registered).toBeGreaterThan(baseline.pageResidency.registered);
      expect(grown.pageResidency.residentBytes).toBeGreaterThan(
        baseline.pageResidency.residentBytes,
      );
      expect(grown.visibility.overflowFlags).toBe(0);

      await engine.call("destroy-entity", { entity: placed.id });
      // An import lands one mesh asset per node inside a container model. The pages belong
      // to the meshes, so removing the model alone retires nothing — the whole container
      // goes, which is what removing an imported model from a project means.
      const { assets } = await engine.call("list-assets");
      const imported = assets.filter(
        (asset) => asset.id === model.id || asset.container === model.id,
      );
      expect(imported.filter((asset) => asset.type === "mesh").length).toBeGreaterThan(1);
      for (const asset of imported) {
        await engine.call("delete-asset", { asset: asset.id });
      }
      const shrunk = await awaitStats(
        (sample) => sample.pageResidency.registered === baseline.pageResidency.registered,
        20_000,
      );
      expect(shrunk.pageResidency.registered).toBe(baseline.pageResidency.registered);
      expect(shrunk.pageResidency.residentBytes).toBe(baseline.pageResidency.residentBytes);
    }

    // Back to the one cube: the churn left exactly the pages it started with resident and
    // the cut it started with on screen.
    const after = await awaitStats(
      (sample) => sample.visibility.records === baseline.visibility.records,
    );
    expect(after.visibility.records).toBe(baseline.visibility.records);
    expectWholeCut(after);
    expect(engine.validationErrors()).toEqual([]);
  },
  120_000,
);
