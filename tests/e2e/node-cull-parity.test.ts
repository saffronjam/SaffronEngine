// Rejecting a hierarchy node never removes anything the frame can see.
//
// The instance cull tests one sphere for a whole prototype. For a plant that is the coarsest
// bound there is: a family passes or fails as a unit, and every node under a surviving one
// emits records whether or not its geometry is on screen. The node cull is the sub-instance
// test — it rejects a node whose swept world bounds leave the view and takes the whole
// subtree with it. Under an assembly it runs per use, so one part can be rejected while its
// siblings draw, which is what makes it worth having at all.
//
// THAT SUBTREE CLAUSE IS THE RISK, and it is why this test exists. Descending on one node's
// bounds is sound only if two things hold. First, the node's box must enclose its whole
// subtree — the cooker establishes that (`close_subtree_bounds`), because simplification
// picks a coarse parent's bounds from the simplified geometry and can shrink them inside the
// children's silhouette. Second, the box must cover runtime deformation — the cooked swept
// extent covers the authored kind, and the world box additionally gains the wind prepass's
// `boundsInflation`, the same slack the instance sphere adds.
//
// Both of those are arguments. The measurement is two hosts, identical but for
// `SAFFRON_NODE_CULL`, rendering the same poses.
//
// WHY A SWEEP RATHER THAN ONE POSE. No single camera proves both halves. At a pose where the
// cull fires, the rejected geometry is off screen by construction, so an over-eager cull
// there would still produce the identical image — the comparison is blind. The poses that
// expose an over-eager cull are the ones with plant geometry near the frame edge, and at
// those the correct cull rejects nothing. So the sweep asserts parity at every pose and
// requires rejections somewhere across it.
//
// THE TOLERANCE IS CALIBRATED, not guessed. Two identically configured hosts measure
// ~0.0002 apart. Mutating the frustum test to reject anything outside ±0.2 NDC — an
// over-eager cull that removes visible geometry — moves the `close-x` pose to 0.045. The
// bound below sits between them, catching the mutation with room over the noise.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  GpuSceneStatsDto,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
  VegetationRuntimeQueryResult,
} from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;
const BOUNDS = {
  minTicks: ["0", "0", "0"],
  maxTicksExclusive: ["262144", "262144", "262144"],
} as const;

/// The cooked plants stand near (1, 0, 1) and are roughly a quarter-metre across, so these
/// poses are close ones. `close-x` fills the frame edges (the over-cull detector); `edge`
/// puts part of the family outside the frustum (where the cull fires).
const CAMERAS = [
  ["close-z", { position: { x: 1.0, y: 0.25, z: 0.2 }, yaw: 180, pitch: 0 }],
  ["close-x", { position: { x: 0.3, y: 0.25, z: 1.0 }, yaw: 90, pitch: 0 }],
  ["edge", { position: { x: 1.4, y: 0.25, z: 0.3 }, yaw: 195, pitch: 0 }],
  ["mid", { position: { x: 1.0, y: 0.3, z: -0.6 }, yaw: 180, pitch: 0 }],
] as const;

/// Mean absolute per-channel difference (0-255) the two hosts may differ by, per pose. See
/// the calibration note above: noise measures ~0.0002 and the over-cull mutation 0.045.
const PARITY_TOLERANCE = 0.01;

type Capture = { frame: Buffer; stats: GpuSceneStatsDto };

const cleaner = new Cleaner();
const runs: Record<string, Record<string, Capture>> = {};

/// Boots a host with the node cull selected, cooks the vegetation cell, waits for a resident
/// plant, then captures every pose in the sweep.
async function sweepWithNodeCull(nodeCull: boolean): Promise<Record<string, Capture>> {
  const label = nodeCull ? "on" : "off";
  const engine = await Engine.boot({
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_NODE_CULL: label,
  });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270, camera: CAMERAS[0][1] });

  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, `node-cull-${label}`);
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  const world = await engine.call<EntityRef>("create-entity", { name: "Node-cull vegetation" });
  await engine.call("add-component", { entity: world.id, component: "VegetationField" });
  await engine.call("set-component", {
    entity: world.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });
  const cook = await engine.call<VegetationCookJobDto>("vegetation-cook", {
    map: fixture.map,
    scope: { kind: "cells", cells: [CELL] },
    workers: 1,
  });
  await awaitCook(engine, cook.job);

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
      stats: await engine.call<GpuSceneStatsDto>("gpu-scene-stats"),
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
