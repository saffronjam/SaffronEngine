// Triangle-versus-voxel parity: a plant drawn as aggregate voxels must look like the same plant
// drawn as triangles, from the same camera.
//
// The hierarchy cut is chosen by projected appearance error, so the only way to reach the aggregate
// form in a normal frame is to fly the camera away, which shrinks the subject at the same moment it
// coarsens it. Differencing those frames measures "the plant got smaller" and "the plant is drawn
// differently" together.
//
// So the cut is pinned: `SAFFRON_CUT_OVERRIDE` forces the traversal to stop refining (`coarse`,
// where aggregate voxels live) or refine fully (`fine`, triangle clusters), leaving camera, scene,
// lighting, and wind identical between runs.
//
// The bound is loose because an aggregate voxel is a different representation, not a
// finer-quantized one: it replaces a stack of leaves with occupancy-weighted matter, so silhouettes
// and high-frequency detail genuinely differ. What must not differ is gross energy — the plant
// cannot become markedly brighter or darker across the transition — so the assertion is on mean
// brightness, with a control showing the frames really are two different pictures.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { VegetationRuntimeQueryResult } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import {
  BOUNDS,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference, regionMean } from "./image.ts";

// The pose both runs render from, framing a cooked plant.
const CAMERA = { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 };

// Mean brightness (0-255) the two representations may differ by over the whole frame. Energy
// parity is the claim; silhouette and detail are expected to differ. Measured: 2.93, against a
// whole-frame difference of 8.97 between the same two captures — so the frames are visibly
// different pictures carrying the same amount of light, which is exactly the claim.
const BRIGHTNESS_TOLERANCE = 6;

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};

// Boots a host with the cut pinned, cooks the fixture cell, and captures one settled frame.
async function captureWithCut(cut: "coarse" | "fine"): Promise<Buffer> {
  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1", SAFFRON_CUT_OVERRIDE: cut });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, `repr-${cut}`);
  const world = await bindVegetationField(engine, cleaner, fixture, `Representation ${cut}`);
  await cookCells(engine, fixture.map);

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
