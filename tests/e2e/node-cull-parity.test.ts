// Rejecting a hierarchy node never removes anything the frame can see.
//
// The node cull is the sub-instance test: it rejects a node whose swept world bounds leave the view
// and takes the whole subtree with it, per use under an assembly, so one part can be rejected while
// its siblings draw.
//
// Descending on one node's bounds is sound only if the node's box encloses its whole subtree —
// which the cooker establishes in `close_subtree_bounds`, because simplification picks a coarse
// parent's bounds from the simplified geometry and can shrink them inside the children's silhouette
// — and if the box covers runtime deformation, which the cooked swept extent plus the wind
// prepass's `boundsInflation` provide.
//
// No single camera proves both halves. Where the cull fires, the rejected geometry is off screen by
// construction, so an over-eager cull produces the identical image and the comparison is blind; the
// poses that expose an over-eager cull have plant geometry near the frame edge, and there a correct
// cull rejects nothing. So the sweep asserts parity at every pose and requires rejections somewhere
// across it, over two hosts identical but for `SAFFRON_NODE_CULL`.
//
// The tolerance is calibrated: two identically configured hosts measure ~0.0002 apart, and mutating
// the frustum test to reject anything outside +/-0.2 NDC moves the `close-x` pose to 0.045. The
// bound sits between them.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  EntityRef,
  GpuSceneMirrorStatsDto,
  VegetationRuntimeQueryResult,
} from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import {
  BOUNDS,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";

// The cooked plants stand near (1, 0, 1) and are roughly a quarter-metre across, so these
// poses are close ones. `close-x` fills the frame edges (the over-cull detector); `edge`
// puts part of the family outside the frustum (where the cull fires).
const CAMERAS = [
  ["close-z", { position: { x: 1.0, y: 0.25, z: 0.2 }, yaw: 180, pitch: 0 }],
  ["close-x", { position: { x: 0.3, y: 0.25, z: 1.0 }, yaw: 90, pitch: 0 }],
  ["edge", { position: { x: 1.4, y: 0.25, z: 0.3 }, yaw: 195, pitch: 0 }],
  ["mid", { position: { x: 1.0, y: 0.3, z: -0.6 }, yaw: 180, pitch: 0 }],
] as const;

// Mean absolute per-channel difference (0-255) the two hosts may differ by, per pose. See
// the calibration note above: noise measures ~0.0002 and the over-cull mutation 0.045.
const PARITY_TOLERANCE = 0.01;

type Capture = { frame: Buffer; stats: GpuSceneMirrorStatsDto };

const cleaner = new Cleaner();
const runs: Record<string, Record<string, Capture>> = {};

// Boots a host with the node cull selected, cooks the vegetation cell, waits for a resident
// plant, then captures every pose in the sweep.
async function sweepWithNodeCull(nodeCull: boolean): Promise<Record<string, Capture>> {
  const label = nodeCull ? "on" : "off";
  const engine = await Engine.boot({
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_NODE_CULL: label,
  });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270, camera: CAMERAS[0][1] });

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, `node-cull-${label}`);
  const world = await engine.call<EntityRef>("create-entity", { name: "Node-cull vegetation" });
  await engine.call("add-component", { entity: world.id, component: "VegetationField" });
  await engine.call("set-component", {
    entity: world.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });
  await cookCells(engine, fixture.map);

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
      query: { kind: "bounds", bounds: BOUNDS },
    });
    if (hits.hits.length > 0) {
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for a resident macro plant");
    }
    await engine.settle(50);
  }
  // A still field: the comparison is about which records survive, not about where the wind
  // put them, and the two hosts advance their clocks independently.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1500);

  const captures: Record<string, Capture> = {};
  for (const [pose, camera] of CAMERAS) {
    await engine.call("set-camera", camera);
    await engine.settle(900);
    captures[pose] = {
      frame: await captureViewport(engine, cleaner, `node-cull-${label}-${pose}`),
      stats: await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats"),
    };
  }
  expect(engine.validationErrors()).toEqual([]);
  return captures;
}

beforeAll(async () => {
  runs.off = await sweepWithNodeCull(false);
  runs.on = await sweepWithNodeCull(true);
}, 240_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the node cull rejects nodes somewhere in the sweep", () => {
  // Without this the parity assertions would pass on a cull that never fired.
  const visited = CAMERAS.reduce((t, [p]) => t + runs.on[p].stats.visibility.visitedNodes, 0);
  const culled = CAMERAS.reduce((t, [p]) => t + runs.on[p].stats.visibility.culledNodes, 0);
  expect(visited).toBeGreaterThan(0);
  expect(culled).toBeGreaterThan(0);
});

test("turning the node cull off walks every node", () => {
  for (const [pose] of CAMERAS) {
    expect(runs.off[pose].stats.visibility.culledNodes).toBe(0);
  }
});

test("the cull emits fewer records without ever emitting more", () => {
  let saved = 0;
  for (const [pose] of CAMERAS) {
    const on = runs.on[pose].stats.visibility.records;
    const off = runs.off[pose].stats.visibility.records;
    expect(on).toBeLessThanOrEqual(off);
    saved += off - on;
  }
  // The point of the cull is the same image from less work. Zero saved would mean the
  // rejected nodes carried nothing and the saving claimed here is imaginary.
  expect(saved).toBeGreaterThan(0);
});

test("every culled pose renders what the unculled host renders", () => {
  for (const [pose] of CAMERAS) {
    const on = decodeRgb8Png(runs.on[pose].frame);
    const off = decodeRgb8Png(runs.off[pose].frame);
    expect(on.width).toBe(off.width);
    expect(on.height).toBe(off.height);
    expect({ pose, diff: meanAbsoluteDifference(on, off) <= PARITY_TOLERANCE }).toEqual({
      pose,
      diff: true,
    });
  }
});
