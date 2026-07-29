// Virtual shadow maps under churn: rapid wind and camera travel must leave no stale shadow behind.
//
// The invalidation half of that claim is already covered by `vsm.test.ts` through the page counters
// — repeat demand answers from residency, a moving caster dirties far below the atlas page count,
// nothing overflows. What counters cannot say is whether the *image* is right afterwards: a page
// that was dirtied but never re-rasterized still reports clean counters while showing last
// position's shadow.
//
// So this compares pixels. A settled reference frame is captured, the scene is then driven hard —
// wind swung between calm and gale, the camera flown away and back, repeatedly, without letting
// anything settle — and the camera is returned to the reference pose. The frame must converge back
// to the reference within a tolerance that admits temporal accumulation but not a stale shadow.
//
// The tolerance is a mean absolute per-channel difference over the whole frame. TAA and the wind
// clock keep it from being exactly zero; a shadow left at a stale position moves far more than the
// budget below.
//
// PAGE CHURN IS NOW COVERED, which it was not before: a 4096² atlas (32×32 tiles) never evicted
// under a scene with seven casters and a twelve-pose sweep, and the earlier note here recorded the
// case as untestable because the render budget was a compile-time constant. `vsm-page-budget` makes
// it settable, so the throttle can be driven to one page a frame — the atlas then cannot keep up
// with the dirty set, which is the pressure the reconvergence path exists to survive. The case
// still asserts that the pressure REALLY happened rather than trusting the knob.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { EntityRef, RenderStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene, trackEntity } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";

/// Mean absolute per-channel difference (0-255) allowed between the reference frame and the frame
/// re-settled after churn. Temporal accumulation and the monotonic wind clock account for a small
/// residue; a shadow rendered at a stale caster position does not fit inside it.
const CONVERGENCE_TOLERANCE = 2.0;

const CAMERA = { position: { x: 0, y: 6, z: 12 }, yaw: 0, pitch: -22 };

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: CAMERA });
  trackEntity(cleaner, engine, await engine.call<EntityRef>("add-entity", { preset: "plane" }));
  const caster = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("add-entity", { preset: "cube" }),
  );
  await engine.call("set-component", {
    entity: caster.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 2, z: 0 },
      scale: { x: 1.5, y: 1.5, z: 1.5 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  await engine.settle(600);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("the atlas converges back to its settled image after wind and camera churn", async () => {
  const reference = decodeRgb8Png(await captureViewport(engine, cleaner, "churn-reference"));

  // Churn: swing the wind between calm and gale and fly the camera out and back, without settling.
  // Each pass dirties pages the next pass must re-rasterize.
  for (let pass = 0; pass < 4; pass += 1) {
    await engine.call("set-wind", { speed: 18, gust: 0.9 });
    await engine.call("set-camera", { position: { x: 40, y: 30, z: 60 }, yaw: 35, pitch: -40 });
    await engine.settle(80);
    await engine.call("set-wind", { speed: 0, gust: 0 });
    await engine.call("set-camera", { position: { x: -25, y: 3, z: -18 }, yaw: 200, pitch: 5 });
    await engine.settle(80);
  }

  // Back to the reference state, and give the atlas time to answer the demand it was left with.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.call("set-camera", CAMERA);
  await engine.settle(1200);

  const settled = decodeRgb8Png(await captureViewport(engine, cleaner, "churn-settled"));
  const difference = meanAbsoluteDifference(reference, settled);

  // Prove the metric discriminates before trusting it: a frame from one of the churn poses must
  // score far ABOVE the tolerance the settled frame scores below. Without this the assertion could
  // pass on a metric that is blind to everything, which is how a visual test quietly becomes
  // decorative.
  await engine.call("set-camera", { position: { x: 40, y: 30, z: 60 }, yaw: 35, pitch: -40 });
  await engine.settle(600);
  const elsewhere = decodeRgb8Png(await captureViewport(engine, cleaner, "churn-control"));
  const control = meanAbsoluteDifference(reference, elsewhere);
  expect(control).toBeGreaterThan(CONVERGENCE_TOLERANCE * 5);

  expect(difference).toBeLessThan(CONVERGENCE_TOLERANCE);

  // The churn must not have overflowed the atlas or left the run dirty.
  await engine.call("set-camera", CAMERA);
  await engine.settle(200);
  const stats = await engine.call<RenderStatsDto>("render-stats");
  expect(stats.vsm.overflow).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});


test("a starved page budget still reconverges to the reference image", async () => {
  // The arm the earlier note recorded as untestable. Throttling the atlas to ONE page a frame
  // means the dirty set outruns the refresh — the backlog the reconvergence path exists to survive
  // — where the default budget of 64 drains a churned frame almost immediately.
  await engine.call("set-camera", CAMERA);
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1400);
  const reference = decodeRgb8Png(await captureViewport(engine, cleaner, "starved-reference"));

  await engine.call("vsm-page-budget", { pages: 1 });
  // The VSM counters are PER FRAME, so they have to be read while the churn is happening — a
  // sample taken after everything settled reports zero dirty pages, which is true and useless.
  let peakDirtied = 0;
  let peakRendered = 0;
  for (const speed of [16, 0, 16, 0]) {
    await engine.call("set-wind", { speed, gust: speed > 0 ? 0.9 : 0 });
    await engine.call("set-camera", { position: { x: 18, y: 10, z: 26 }, yaw: 24, pitch: -30 });
    await engine.settle(120);
    const during = await engine.call<RenderStatsDto>("render-stats");
    peakDirtied = Math.max(peakDirtied, during.vsm.dirtied);
    peakRendered = Math.max(peakRendered, during.vsm.rendered);
    await engine.call("set-camera", CAMERA);
    await engine.settle(120);
  }

  // THE PRESSURE MUST BE REAL, not assumed from the knob. A budget of one that still drained
  // everything would make the reconvergence below a test of an idle atlas.
  expect(peakDirtied).toBeGreaterThan(0);
  expect(peakRendered).toBeLessThanOrEqual(1);

  // Restore the budget and let the backlog drain. The image must come back to where it started:
  // a page dirtied under starvation and never re-rasterized shows the old shadow while every
  // counter reads clean.
  await engine.call("vsm-page-budget", { pages: 64 });
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(2200);
  const settled = decodeRgb8Png(await captureViewport(engine, cleaner, "starved-settled"));
  expect(meanAbsoluteDifference(reference, settled)).toBeLessThan(CONVERGENCE_TOLERANCE);

  const recovered = await engine.call<RenderStatsDto>("render-stats");
  expect(recovered.vsm.overflow).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});
