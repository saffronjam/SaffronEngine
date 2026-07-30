// Wind visibly moves cooked vegetation, and moves nothing else.
//
// Every other wind assertion is structural, so none would notice a path that computes a correct
// sway record and never displaces a vertex.
//
// The measurement is motion over time, not calm versus gale: comparing one calm frame to one gale
// frame conflates displacement with every other difference the two states carry. Each state is
// sampled twice across the same settle and compared to itself — the calm pair must be
// near-identical, the gale pair must differ. The calm pair is the control, and it fails if the frame
// is unstable for any reason (TAA that never converges, an animation left running, a
// nondeterministic pass), which would otherwise make the gale assertion pass for the wrong reason.
//
// Wind only displaces instances carrying `GPU_SCENE_INSTANCE_FLAG_WIND`, which the mirror sets on
// vegetation points alone, so a cooked cell is required and a cube in the wind is motionless by
// design.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { VegetationRuntimeQueryResult } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import {
  BOUNDS,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";

// The camera pose that frames the cooked cell's canopy.
const CAMERA = { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 };

// Mean absolute per-channel difference (0-255) two captures of a *still* scene may differ by.
// A settled frame measures 0.0001 here, so this is three orders of magnitude of headroom for
// temporal accumulation — and still far below what a swaying canopy produces.
const STILL_TOLERANCE = 0.01;

// The same difference a *moving* canopy must at least reach. The gale measures 0.408 with this
// framing, roughly 3,500x the still frame, so the floor sits well clear of both.
const MOTION_FLOOR = 0.1;

const cleaner = new Cleaner();
let engine: Engine;

// Captures two frames of the current wind state, separated by a settle, and returns how far the
// second moved from the first.
async function motionOver(tag: string): Promise<number> {
  const first = decodeRgb8Png(await captureViewport(engine, cleaner, `${tag}-a`));
  await engine.settle(500);
  const second = decodeRgb8Png(await captureViewport(engine, cleaner, `${tag}-b`));
  return meanAbsoluteDifference(first, second);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "wind-visual");
  const world = await bindVegetationField(engine, cleaner, fixture, "Wind visual vegetation");
  await cookCells(engine, fixture.map);

  // Deliberately NOT in play mode: play renders the scene's primary camera, so `set-camera`
  // (the editor camera) would be ignored and every frame below would be the same picture of
  // nothing. Residency is camera-driven and works in edit mode.
  await engine.call("set-camera", CAMERA);
  await engine.settle(200);

  // The cell must actually be resident before any pixel means anything.
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
}, 180_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("a still wind field leaves the canopy still, and a gale visibly moves it", async () => {
  // Calm first: with no speed and no gust the sway term is zero, so consecutive frames of the same
  // scene must agree. This is the control for the gale measurement below.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1500);
  const still = await motionOver("wind-calm");
  expect(still).toBeLessThan(STILL_TOLERANCE);

  // Now a gale. The canopy sways continuously, so two frames of the *same* wind state disagree —
  // and by far more than a still scene's own frame-to-frame residue.
  await engine.call("set-wind", { speed: 22, gust: 0.9 });
  await engine.settle(1500);
  const moving = await motionOver("wind-gale");

  expect(moving).toBeGreaterThan(MOTION_FLOOR);
  // And decisively more than the still frame's own residue, not merely above a constant.
  expect(moving).toBeGreaterThan(still * 20);
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);

test("the wind field can be brought back to still", async () => {
  // Wind is a live field, not a one-way switch: dropping it back to calm must stop the motion
  // again. A deformation that latched at its last displacement would pass the test above and fail
  // here.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(2000);
  const still = await motionOver("wind-restill");
  expect(still).toBeLessThan(STILL_TOLERANCE);
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);
