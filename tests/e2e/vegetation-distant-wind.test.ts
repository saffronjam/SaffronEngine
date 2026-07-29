// A plant drawn as an aggregate voxel brick still moves in the wind.
//
// The aggregate branch applies the same stored sway as the triangle path — every raster pass adds
// `gpuSceneWindDeform` at its world-compose line with no representation branch around it — so a far
// plant should keep its near-field motion. This measures that rather than asserting it.
//
// A PREVIOUS ATTEMPT AT THIS TEST WAS DELETED FOR PASSING WHEN IT SHOULD NOT HAVE. It forced the
// coarsest cut, confirmed `voxelRecords > 0`, measured motion under a gale, and passed — and it
// ALSO passed with the aggregate branch's sway mutated to zero. The counters explained it: the
// frame drew 6 records, of which 1 was the aggregate and 5 were micro-blade grass candidates. The
// motion it measured was the grass.
//
// Grass is reconstructed GPU-side and scatters across the whole ground, so no screen region
// contains the plant and not the blades — picking a region cannot separate them. The only honest
// separation is to stop drawing the blades, which is what `SAFFRON_MICRO_FIELD=off` now does, and
// the counters below assert the separation actually happened rather than trusting the flag.

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
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;
const BOUNDS = {
  minTicks: ["0", "0", "0"],
  maxTicksExclusive: ["262144", "262144", "262144"],
} as const;

const cleaner = new Cleaner();
let engine: Engine;
/// Counters with the plant resident, the cut pinned coarse, and the micro field suppressed.
let stats: GpuSceneStatsDto;
/// Two frames a moment apart under a gale, and two under a still field.
const gale: Buffer[] = [];
const still: Buffer[] = [];

beforeAll(async () => {
  engine = await bootEngine(cleaner, {
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_CUT_OVERRIDE: "coarse",
    SAFFRON_MICRO_FIELD: "off",
    // The two flags this test turns on. The cut override forces the aggregate representation; the
    // micro-field gate removes the grass that made the previous attempt meaningless.
  });
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, "distant-wind");
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  const world = await engine.call<EntityRef>("create-entity", { name: "Distant vegetation" });
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

  // The camera is set HERE rather than through `prepareScene`, and it is the framing
  // `vegetation-wind-visual` established — residency is camera-driven, so the view has to be in
  // place before the cell can become resident, and a camera aimed at one plant's coordinates
  // frames nothing: the field scatters its canopy across the cell rather than putting it where a
  // single runtime query happens to report.
  await engine.call("set-camera", { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 });
  await engine.settle(200);

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
      query: { kind: "bounds", bounds: BOUNDS },
    });
    if (hits.hits[0]) {
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for a resident macro plant");
    }
    await engine.settle(50);
  }
  await engine.settle(800);
  stats = await engine.call<GpuSceneStatsDto>("gpu-scene-stats");

  await engine.call("set-wind", { speed: 14, gust: 0.8 });
  await engine.settle(500);
  gale.push(await captureViewport(engine, cleaner, "distant-gale-a"));
  await engine.settle(260);
  gale.push(await captureViewport(engine, cleaner, "distant-gale-b"));

  await engine.call("set-wind", { speed: 0, gust: 0 });
  // Long enough for the sway to settle rather than merely to stop being driven.
  await engine.settle(1600);
  still.push(await captureViewport(engine, cleaner, "distant-still-a"));
  await engine.settle(260);
  still.push(await captureViewport(engine, cleaner, "distant-still-b"));
}, 240_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the frame draws the aggregate and nothing else that moves", () => {
  // THE PREMISE, asserted rather than assumed. If micro candidates survived, the motion measured
  // below would be theirs and the aggregate could be frozen without failing anything — which is
  // exactly how the previous version of this test passed against a broken engine.
  expect(stats.visibility.voxelRecords).toBeGreaterThan(0);
  expect(stats.visibility.microCandidates).toBe(0);
});

test("the aggregate keeps moving under a gale", () => {
  const a = decodeRgb8Png(gale[0]!);
  const b = decodeRgb8Png(gale[1]!);
  // The plant is the only thing in the frame that can move, so any frame-to-frame difference is
  // its sway. The threshold is above the noise a static frame shows (measured at 0 below).
  expect(meanAbsoluteDifference(a, b)).toBeGreaterThan(0.05);
});

test("a still field leaves the aggregate at rest", () => {
  // The control that makes the assertion above mean "wind moved it" rather than "frames differ".
  // Without this, a renderer with a dithered or noisy pass would satisfy the gale test forever.
  const a = decodeRgb8Png(still[0]!);
  const b = decodeRgb8Png(still[1]!);
  expect(meanAbsoluteDifference(a, b)).toBeLessThan(0.05);
});

test("the distant aggregate renders validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
