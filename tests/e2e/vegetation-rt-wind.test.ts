// The wind-deformed geometry a frame materializes for ray traversal carries the displacement.
//
// Every other assertion about this path is structural: a dispatch was planned, a structure was
// refit, a counter moved. None of them would notice a materialization that wrote the rest pose —
// wrong instance slot, no wind record, a zero sway sample — and the visible consequence is a canopy
// that sways in every raster pass while its ray-traced shadow stands still.
//
// So the observable is the shadow, isolated from everything else in the frame. The directional
// shadow map is switched off, leaving ray traversal as the only thing that shadows a plane under the
// plant; differencing a frame that traces against the same frame that does not leaves the shadow
// term alone, because every other term is identical between the two. The wind then moves between two
// STATIC states — none, and a gust-free breeze whose sampled velocity is the mean direction alone
// and therefore constant in space and time — and the isolated shadow must move with it.
//
// The rest state is isolated twice, which calibrates the floor: two samples of a scene that did not
// change measure what temporal residue alone contributes.
//
// The framing comes from the resident plant's own position rather than a constant: a shadow a few
// pixels across measures nothing, and the fixture decides where the plant stands.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import {
  absoluteDifferenceImage,
  decodeRgb8Png,
  meanAbsoluteDifference,
  type Rgb8Image,
} from "./image.ts";
import {
  bindVegetationField,
  BOUNDS,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
  TICKS_PER_METER,
} from "./vegetation-utils.ts";

// The breeze both bent states use. Gust zero is what makes it static: the gust term is what scales
// the turbulence, so without it the sampled velocity is the mean direction and nothing else.
const BREEZE_SPEED = 22;

// Mean absolute per-channel difference (0-255) the ray shadows must cover at rest, so the
// measurement below cannot pass on a frame with no ray shadow in it at all. Measured: 0.091.
const FOOTPRINT_FLOOR = 0.03;

// How far the isolated shadow must move under the breeze: a multiple of the rest state's own
// residue, and an absolute floor because that residue measures zero. Measured: 0.034 against a
// residue of exactly 0, since nothing in the frame accumulates over time.
const DISPLACEMENT_MARGIN = 4;
const DISPLACEMENT_FLOOR = 0.01;

const cleaner = new Cleaner();
let engine: Engine;
let rtSupported = false;
const frames: Record<string, Rgb8Image> = {};

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: 480, height: 270 });
  rtSupported = (await engine.call("render-stats")).rtSupported;
  if (!rtSupported) {
    return;
  }
  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "rt-wind");
  await bindVegetationField(engine, cleaner, fixture, "RT wind vegetation");
  await cookCells(engine, fixture.map);
  // Residency is camera-driven, so a pose over the cell comes before the cell can stream.
  await engine.call("set-camera", { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 });
  await engine.settle(200);

  const deadline = Date.now() + 30_000;
  let plant: [number, number, number] = [0, 0, 0];
  for (;;) {
    const hit = (await queryPlants(engine, BOUNDS)).hits[0];
    if (hit) {
      const ticks = hit.plant.positionTicks;
      plant = [
        Number(ticks[0]) / TICKS_PER_METER,
        Number(ticks[1]) / TICKS_PER_METER,
        Number(ticks[2]) / TICKS_PER_METER,
      ];
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for a resident cell");
    }
    await engine.settle(50);
  }

  // The receiver: without a surface under the plant its ray shadow falls on nothing and every
  // difference below reads zero.
  const ground = await engine.call("add-entity", { preset: "plane" });
  await engine.call("set-component", {
    entity: ground.id,
    component: "Transform",
    json: {
      translation: { x: plant[0], y: plant[1], z: plant[2] },
      scale: { x: 3, y: 1, z: 3 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  await engine.call("set-camera", {
    position: { x: plant[0], y: plant[1] + 0.9, z: plant[2] + 2.2 },
    yaw: 0,
    pitch: -14,
  });
  // A low sun, held: the shadow is long enough to measure and the frame does not drift between
  // captures.
  await engine.call("set-time-of-day", {
    enabled: true,
    manualOverride: true,
    timeOfDay: 0.3,
    dayLengthSeconds: 0,
  });
  // Ray traversal becomes the only thing that shadows the plane, so a moving shadow is a moving
  // acceleration structure and not a rasterized shadow page.
  await engine.call("set-shadows", { enabled: false });
  // Nothing that accumulates over frames: each isolation below differences two captures, and a
  // temporal history or a round-robin probe update leaves residue in every one of them. Direct
  // light and the shadow term are what remains, and they are deterministic.
  await engine.call("set-aa", { mode: "off" });
  await engine.call("set-gi", { mode: "off" });
  await engine.call("set-gdf", { enabled: false });

  const capture = async (speed: number, rays: boolean, tag: string): Promise<Rgb8Image> => {
    await engine.call("set-wind", { speed, gust: 0 });
    await engine.call("set-rt-shadows", { enabled: rays });
    await engine.settle(1800);
    return decodeRgb8Png(await captureViewport(engine, cleaner, tag));
  };
  frames.restFlat = await capture(0, false, "rt-wind-rest-flat");
  frames.restRays = await capture(0, true, "rt-wind-rest-rays");
  // The rest state again, for the floor: the same scene, the same two toggles, nothing moved.
  frames.restFlatAgain = await capture(0, false, "rt-wind-rest-flat-again");
  frames.restRaysAgain = await capture(0, true, "rt-wind-rest-rays-again");
  frames.bentFlat = await capture(BREEZE_SPEED, false, "rt-wind-bent-flat");
  frames.bentRays = await capture(BREEZE_SPEED, true, "rt-wind-bent-rays");
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("ray traversal is what shadows the plane", () => {
  if (!rtSupported) {
    return;
  }
  // The premise. With the shadow map off and no ray shadow either, the two rest frames would be the
  // same picture and the displacement measurement below would compare nothing to nothing.
  expect(meanAbsoluteDifference(frames.restRays!, frames.restFlat!)).toBeGreaterThan(
    FOOTPRINT_FLOOR,
  );
});

test("the wind moves the ray-traced shadow", async () => {
  if (!rtSupported) {
    return;
  }
  // The shadow term alone: within one state the two frames differ in nothing but whether ray
  // traversal shadowed them, so the plant's own moving pixels cancel out of each isolation.
  const restShadow = absoluteDifferenceImage(frames.restRays!, frames.restFlat!);
  const restShadowAgain = absoluteDifferenceImage(frames.restRaysAgain!, frames.restFlatAgain!);
  const bentShadow = absoluteDifferenceImage(frames.bentRays!, frames.bentFlat!);
  // Two samples of the unchanged rest state: temporal residue and nothing else.
  const floor = meanAbsoluteDifference(restShadow, restShadowAgain);
  // The same isolation under the breeze. A materialization that wrote the rest pose leaves the
  // shadow where it was, and this collapses onto the floor.
  const moved = meanAbsoluteDifference(restShadow, bentShadow);
  expect(moved).toBeGreaterThan(Math.max(floor * DISPLACEMENT_MARGIN, DISPLACEMENT_FLOOR));

  const stats = await engine.call("render-stats");
  // And the structures that shadow came from are the materialized ones: the placed uses whose wind
  // pose the frame wrote into the deformed arena.
  expect(stats.windDeformedInstances).toBeGreaterThan(0);
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);
