// Wind visibly moves cooked vegetation, and moves nothing else.
//
// Every other wind assertion is structural, so none would notice a path that computes a correct
// sway record and never displaces a vertex.
//
// The measurement is motion over time, not calm versus gale: comparing one calm frame to one gale
// frame conflates displacement with every other difference the two states carry. Each state is
// sampled twice across the same settle and compared to itself — nothing in the calm pair may move,
// the gale pair must. The calm pair is the control, and it fails if the frame is unstable for any
// reason (TAA that never converges, an animation left running, a nondeterministic pass), which
// would otherwise make the gale assertion pass for the wrong reason.
//
// What separates the two states is the size of the step a channel takes, not how much of the frame
// drifted. A resident micro field is matter in the global distance field, and the occlusion marches
// that read it rotate their sample set per frame and resolve over several, so the canopy's shading
// keeps stepping by one 8-bit level for as long as the frame runs — with the field at zero, the
// prepass recording zero sway, and the step never growing. The same scene with
// `SAFFRON_MICRO_FIELD=off` holds a byte-identical frame indefinitely, and so does a scene with no
// cooked cell. That residue covers the whole canopy, so a mean over the frame scores it as motion
// (0.02 to 0.04 — what a gale window scores when it samples the sway near a phase return), while a
// step threshold scores it as the stillness it is. A swaying silhouette moves whole tones.
//
// Wind only displaces instances carrying `GPU_SCENE_INSTANCE_FLAG_WIND`, which the mirror sets on
// vegetation points alone, so a cooked cell is required and a cube in the wind is motionless by
// design.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import {
  bindVegetationField,
  BOUNDS,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
} from "./vegetation-utils.ts";
import { channelsDifferingBy, decodeRgb8Png, peakAbsoluteDifference } from "./image.ts";

// The camera pose that frames the cooked cell's canopy.
const CAMERA = { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 };

// The greatest step one 8-bit channel may take between two captures of a *still* scene. Every
// still window measures 1 — the single quantization level the resolving occlusion costs — so the
// bound admits one more and still reads tones rather than rounding.
const STILL_STEP = 2;

// The step only a displaced silhouette takes, and how many channels must take it. The still frame
// puts zero channels there, so the states are separated by the metric rather than by a margin. One
// window of the gale puts between 178 and 11,973 there depending on where it samples the sway
// cycle; the worst of the two windows below never fell under 5,862.
const MOTION_STEP = 8;
const MOTION_CHANNELS = 100;

// How far two captures of one wind state moved: the greatest step any channel took, and how many
// channels took a step only real displacement produces.
interface Motion {
  peak: number;
  moved: number;
}

const cleaner = new Cleaner();
let engine: Engine;

// The most any two consecutive captures of the current wind state move, over three captures
// spanning the same settle. A canopy sways on a cycle, so a single window can catch it near a
// phase return and understate; the worst of two is what "does it move at all" asks for. A still
// canopy scores the same in every window, so the same worst-case reading tightens its control.
async function motionOver(tag: string): Promise<Motion> {
  let previous = decodeRgb8Png(await captureViewport(engine, cleaner, `${tag}-0`));
  const worst: Motion = { peak: 0, moved: 0 };
  for (let index = 1; index <= 2; index += 1) {
    await engine.settle(500);
    const next = decodeRgb8Png(await captureViewport(engine, cleaner, `${tag}-${index}`));
    worst.peak = Math.max(worst.peak, peakAbsoluteDifference(previous, next));
    worst.moved = Math.max(worst.moved, channelsDifferingBy(previous, next, MOTION_STEP));
    previous = next;
  }
  return worst;
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "wind-visual");
  await bindVegetationField(engine, cleaner, fixture, "Wind visual vegetation");
  await cookCells(engine, fixture.map);

  // Deliberately NOT in play mode: play renders the scene's primary camera, so `set-camera`
  // (the editor camera) would be ignored and every frame below would be the same picture of
  // nothing. Residency is camera-driven and works in edit mode.
  await engine.call("set-camera", CAMERA);
  await engine.settle(200);

  // The cell must actually be resident before any pixel means anything.
  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await queryPlants(engine, BOUNDS);
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
  // Calm first: with no speed and no gust the sway term is zero, so no silhouette in the frame
  // may move. This is the control for the gale measurement below.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1500);
  const still = await motionOver("wind-calm");
  expect(still.peak).toBeLessThanOrEqual(STILL_STEP);
  expect(still.moved).toBe(0);

  // Now a gale. The canopy sways continuously, so two frames of the *same* wind state disagree —
  // and by whole tones over thousands of channels, which the still frame reaches none of.
  await engine.call("set-wind", { speed: 22, gust: 0.9 });
  await engine.settle(1500);
  const moving = await motionOver("wind-gale");

  expect(moving.moved).toBeGreaterThan(MOTION_CHANNELS);
  // And decisively past the still frame's own residue, not merely above a constant.
  expect(moving.peak).toBeGreaterThan(still.peak * 8);
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);

test("dropping probe GI leaves the canopy as still, and a gale still moves it", async () => {
  // The canopy's whole indirect diffuse rides one sky-visibility map. With probe GI on, that map
  // is diluted per pixel by the probe cage's coverage; with it off, the analytic sky term carries
  // it at full weight, so anything restless in the map reaches the frame undiluted. The map is a
  // cone trace whose ring rotates every frame, and only its temporal accumulation converges — so
  // the configuration that stops diluting it is the one that says whether the renderer samples the
  // converged stage. Sampling any earlier stage reads as a canopy-wide tremor here and nowhere
  // else, which is why this case needs its own coverage rather than a wider tolerance on the one
  // above.
  //
  // Anti-aliasing is off for the whole suite, so this is also the only cover for the DDGI-off,
  // AA-off pair; both are pinned below so the case cannot silently become a different one.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.call("set-gi", { mode: "ddgi" });
  await engine.settle(1500);
  const diluted = await motionOver("wind-calm-ddgi");
  const dilutedStats = await engine.call("render-stats");

  await engine.call("set-gi", { mode: "off" });
  await engine.settle(1500);
  const still = await motionOver("wind-calm-no-ddgi");
  const stats = await engine.call("render-stats");

  // The gale has to survive the same configuration: a still frame that is still because the
  // canopy stopped being drawn would pass every assertion above it.
  await engine.call("set-wind", { speed: 22, gust: 0.9 });
  await engine.settle(1500);
  const moving = await motionOver("wind-gale-no-ddgi");
  await engine.call("set-gi", { mode: "ddgi" });

  expect(dilutedStats.aa).toBe("off");
  expect(dilutedStats.ddgi).toBe(true);
  expect(stats.aa).toBe("off");
  expect(stats.ddgi).toBe(false);

  expect(diluted.peak).toBeLessThanOrEqual(STILL_STEP);
  // Undiluted must be no worse than diluted — the property, rather than a constant that could be
  // widened until whatever the renderer does passes.
  expect(still.peak).toBeLessThanOrEqual(diluted.peak);
  expect(still.moved).toBe(0);

  expect(moving.moved).toBeGreaterThan(MOTION_CHANNELS);
  expect(moving.peak).toBeGreaterThan(still.peak * 8);
  expect(engine.validationErrors()).toEqual([]);
}, 180_000);

test("the wind field can be brought back to still", async () => {
  // Wind is a live field, not a one-way switch: dropping it back to calm must stop the motion
  // again. A deformation that latched at its last displacement would pass the test above and fail
  // here.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(2000);
  const still = await motionOver("wind-restill");
  expect(still.peak).toBeLessThanOrEqual(STILL_STEP);
  expect(still.moved).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);
