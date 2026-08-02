// Trampling a cooked canopy and releasing it leaves no stale shadow behind.
//
// `vsm-churn` covers wind, camera travel and page pressure over a plane-and-cube scene. The
// interaction field is the churn source it cannot reach, and it is different in kind: wind is a pure
// function of time that every pass re-evaluates, while the field is STATEFUL — camera-centred
// cascades of damped-oscillator texels that impulses push and that spring back over about a second.
// A stale shadow here is the atlas still holding the trampled silhouette after the plants have stood
// back up, which the page counters cannot see: they read clean while showing the old picture.
//
// This runs on the WOODLAND fixture rather than the canonical one. The canonical cell is four metres
// across and its plants cover too little of the frame — a full lean moves the whole-frame metric by
// about 0.6 against a post-churn floor near 0.2, and three times separation is not enough to assert
// on. The woodland spans taller trunks over a wider cell, which is what makes the measurement
// discriminating: the push moves the frame by ~0.97 and it settles back to ~0.21.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import {
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
  WIDE_BOUNDS,
} from "./vegetation-utils.ts";

// The framing the stress suite established for a woodland cell: cells are 64 m apart and this
// parks the camera over cell (0,0,0) looking into it.
const CAMERA = { position: { x: 32, y: 8, z: 44 }, yaw: 0, pitch: -10 } as const;

// Mean absolute per-channel difference (0-255) a settled frame may drift by. Temporal accumulation
// accounts for a small residue; a shadow left at a stale silhouette does not fit inside it.
const CONVERGENCE_TOLERANCE = 0.3;

const cleaner = new Cleaner();
let engine: Engine;
let plants: string[] = [];

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = loadFixture("vegetation-stress-woodland");
  await importVegetationPackage(engine, cleaner, fixture, "churn");
  await bindVegetationField(engine, cleaner, fixture, "Churn vegetation");
  await cookCells(engine, fixture.map);

  // After the cook: residency is camera-driven, so the view has to be in place before the cell can
  // become resident.
  await engine.call("set-camera", CAMERA);
  await engine.settle(200);

  const deadline = Date.now() + 40_000;
  for (;;) {
    const hits = await queryPlants(engine, WIDE_BOUNDS);
    if (hits.hits.length > 0) {
      plants = hits.hits.map((hit) => hit.plant.plant);
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for resident woodland plants");
    }
    await engine.settle(50);
  }
  // No wind for the whole run: the transition and the interaction field are the only things allowed
  // to move, or the convergence budget would be measuring a gust.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(2500);
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the cell streams a canopy to trample", () => {
  // The premise, asserted rather than assumed: with nothing resident the impulses below push empty
  // ground and every difference reads zero, which passes a reconvergence check by accident.
  expect(plants.length).toBeGreaterThan(0);
});

test("trampling the canopy and releasing it leaves the frame where it started", async () => {
  await engine.settle(1500);
  const reference = decodeRgb8Png(await captureViewport(engine, cleaner, "churn-trample-before"));

  // Every impulse carries an explicit DIRECTION. A directionless one pushes radially outward from
  // its own centre and therefore cancels AT the centre, so a push aimed at the canopy would move
  // nothing and look exactly like a broken interaction path.
  const trample = async () => {
    const directions: [number, number][] = [
      [1, 0],
      [0, 1],
    ];
    for (const direction of directions) {
      await engine.call("emit-interaction-impulse", {
        positionM: [32, 32],
        radiusM: 64,
        strength: 45,
        direction,
        depress: 4,
      });
    }
  };

  let pushed: Buffer | undefined;
  for (let pass = 0; pass < 3; pass += 1) {
    await trample();
    await engine.settle(110);
    if (pass === 0) {
      pushed = await captureViewport(engine, cleaner, "churn-trample-pushed");
    }
  }
  // The metric has to discriminate before the recovery below proves anything.
  expect(meanAbsoluteDifference(reference, decodeRgb8Png(pushed!))).toBeGreaterThan(
    CONVERGENCE_TOLERANCE * 2,
  );

  // The texel oscillator recovers over about a second; saturating pushes ring for longer, so this
  // waits well past that before asking for the reference back.
  await engine.settle(6000);
  const settled = decodeRgb8Png(await captureViewport(engine, cleaner, "churn-trample-settled"));
  expect(meanAbsoluteDifference(reference, settled)).toBeLessThan(CONVERGENCE_TOLERANCE);

  const stats = await engine.call("render-stats");
  expect(stats.vsm.overflow).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
}, 180_000);
