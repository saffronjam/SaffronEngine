// Triangle↔voxel parity: a plant drawn as aggregate voxels must look like the same plant drawn as
// triangles, from the same camera.
//
// THE OBVIOUS TEST IS THE WRONG ONE. The hierarchy cut is chosen by projected appearance error, so
// the only way to reach the aggregate form in a normal frame is to fly the camera away — which
// shrinks the subject at the same moment it coarsens it. Differencing those two frames measures
// "the plant got smaller" and "the plant is drawn differently" together, and cannot separate them.
//
// So the cut is pinned instead. `SAFFRON_CUT_OVERRIDE` forces the traversal to stop refining
// (`coarse`, where aggregate voxels live) or to refine fully (`fine`, triangle clusters), leaving
// the camera, the scene, the lighting and the wind identical between the two runs. One thing
// changes, which is the only arrangement under which the difference means anything.
//
// The bound is deliberately loose. An aggregate voxel is a *different representation*, not a
// finer-quantized one: it replaces a stack of leaves with occupancy-weighted matter, so silhouettes
// and high-frequency detail genuinely differ. What must NOT differ is gross energy — the plant
// cannot become markedly brighter or darker when it crosses the transition, which is the artifact
// the derived parity occupancy exists to prevent. The assertion is therefore on mean brightness,
// not on per-pixel agreement, and the control below shows the frames really are two different
// pictures rather than one.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
  VegetationRuntimeQueryResult,
} from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene, trackEntity } from "./test-utils.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference, regionMean } from "./image.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;
const BOUNDS = {
  minTicks: ["0", "0", "0"],
  maxTicksExclusive: ["262144", "262144", "262144"],
} as const;

/// The pose both runs render from, framing a cooked plant.
const CAMERA = { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 };

/// Mean brightness (0-255) the two representations may differ by over the whole frame. Energy
/// parity is the claim; silhouette and detail are expected to differ. Measured: 2.93, against a
/// whole-frame difference of 8.97 between the same two captures — so the frames are visibly
/// different pictures carrying the same amount of light, which is exactly the claim.
const BRIGHTNESS_TOLERANCE = 6;

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};

/// Boots a host with the cut pinned, cooks the fixture cell, and captures one settled frame.
async function captureWithCut(cut: "coarse" | "fine"): Promise<Buffer> {
  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1", SAFFRON_CUT_OVERRIDE: cut });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, `repr-${cut}`);
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  const world = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: `Representation ${cut}` }),
  );
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

  await engine.call("set-camera", CAMERA);
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(200);

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
      query: { kind: "bounds", bounds: BOUNDS },
    });
    if (hits.hits.length > 0) {
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for a resident macro plant (${cut})`);
    }
    await engine.settle(50);
  }
  await engine.settle(1200);
  const frame = await captureViewport(engine, cleaner, `repr-${cut}`);
  expect(engine.validationErrors()).toEqual([]);
  return frame;
}

beforeAll(async () => {
  frames.fine = await captureWithCut("fine");
  frames.coarse = await captureWithCut("coarse");
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the coarse and fine cuts really are different pictures", () => {
  const fine = decodeRgb8Png(frames.fine!);
  const coarse = decodeRgb8Png(frames.coarse!);
  // Without this the brightness assertion below could pass because the override did nothing and
  // both runs rendered the identical cut — the failure mode that makes a parity test decorative.
  // The two cuts measure 8.97 apart; the floor sits well clear of that and far from zero.
  expect(meanAbsoluteDifference(fine, coarse)).toBeGreaterThan(2);
});

test("crossing to the aggregate representation preserves gross energy", () => {
  const fine = decodeRgb8Png(frames.fine!);
  const coarse = decodeRgb8Png(frames.coarse!);
  const whole = { x: 0, y: 0, width: fine.width, height: fine.height };
  const fineMean = regionMean(fine, whole);
  const coarseMean = regionMean(coarse, whole);
  // Detail and silhouette may differ; total light may not. A voxel whose occupancy disagreed with
  // the transmission of the leaves it replaces would shift this.
  expect(Math.abs(fineMean - coarseMean)).toBeLessThan(BRIGHTNESS_TOLERANCE);
});
