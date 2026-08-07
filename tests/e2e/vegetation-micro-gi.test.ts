// A dense micro field occludes global illumination and reaches the acceleration structures.
//
// A reconstructed grass blade has no CPU instance and no cooked distance field — it exists only as a
// device-side reconstruction of the tile's density samples. So neither the occluder scatter, which
// walks the reach view's visible list, nor the ray-instance gather can see one, and a field that
// renders occludes nothing and casts nothing. The resident-tile directory closes both: one aggregate
// slab occluder per tile for the distance field, and one materialized blade-geometry structure per
// near tile for ray traversal.
//
// Observing the occlusion needs the field's indirect contribution separated from its pixels, so each
// boot is measured twice — once with global illumination on and once with every indirect path off.
// At this range a blade is sub-pixel and the field's own pixels move the frame by nothing, which is
// the control; the lit frame is where the aggregate takes light out of the marches. A field that
// occluded nothing would leave both measurements equal.
//
// The receiver is a ground plane across the cell: its indirect diffuse comes from probes that sit
// inside the slab once the field is resident. `SAFFRON_MICRO_FIELD=off` suppresses the
// reconstruction, so the two boots differ in the field and in nothing else.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, regionMean } from "./image.ts";
import {
  bindVegetationField,
  BOUNDS,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
} from "./vegetation-utils.ts";

// The pose both boots render from: above the cell, framing the ground plane the field covers.
const CAMERA = { position: { x: 32, y: 10, z: 46 }, yaw: 0, pitch: -32 };

// Mean brightness (0-255) two captures of the *same* scene may differ by. Measured: identical to the
// byte, so this is headroom for temporal residue and nothing more.
const STILL_TOLERANCE = 0.05;

// Mean brightness the resident field must cost the lit frame. Measured: 1.42, against 0.00 from the
// field's own pixels, so the floor sits well below the signal and clear of the control.
const OCCLUSION_FLOOR = 0.5;

// One boot's measurements: the settled frame's mean brightness with global illumination on and with
// every indirect path off, plus the counters the ray half moves.
interface FieldRun {
  giOn: number;
  giOff: number;
  stats: RenderStatsDto;
}

const cleaner = new Cleaner();
const runs: Record<string, FieldRun> = {};
let rtSupported = false;

// Boots a host with or without the micro reconstruction, cooks the fixture over a ground plane, and
// measures the settled frame at both global-illumination states.
async function measure(microField: boolean): Promise<FieldRun> {
  const env: Record<string, string> = { SAFFRON_SCRATCH_PROJECT: "1" };
  if (!microField) {
    env.SAFFRON_MICRO_FIELD = "off";
  }
  const tag = microField ? "micro-on" : "micro-off";
  const engine = await Engine.boot(env);
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270 });
  // The receiver: a ground plane across the cell the field covers.
  const ground = await engine.call("add-entity", { preset: "plane" });
  await engine.call("set-component", {
    entity: ground.id,
    component: "Transform",
    json: {
      translation: { x: 32, y: 0, z: 32 },
      scale: { x: 12, y: 1, z: 12 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  rtSupported = (await engine.call("render-stats")).rtSupported;
  if (rtSupported) {
    // Arming ray-query shadows is what makes the host build a TLAS at all, and with it the
    // materialized blade structures.
    await engine.call("set-rt-shadows", { enabled: true });
  }

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, tag);
  await bindVegetationField(engine, cleaner, fixture, `Micro GI ${tag}`);
  await cookCells(engine, fixture.map);

  await engine.call("set-camera", CAMERA);
  // Wind would move the blades between captures, and their bend rides the materialized geometry.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(200);

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await queryPlants(engine, BOUNDS);
    if (hits.hits.length > 0) {
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for a resident cell (${tag})`);
    }
    await engine.settle(50);
  }
  await engine.settle(1500);

  const wholeFrameMean = async (label: string): Promise<number> => {
    const image = decodeRgb8Png(await captureViewport(engine, cleaner, `${tag}-${label}`));
    return regionMean(image, { x: 0, y: 0, width: image.width, height: image.height });
  };
  const giOn = await wholeFrameMean("gi-on");
  const stats = await engine.call("render-stats");

  // Every indirect path off: no probe irradiance, no distance-field march, no sky-visibility cone,
  // so no occluder of any kind reaches the frame and what remains of the field is its pixels.
  await engine.call("set-gi", { mode: "off" });
  await engine.call("set-gdf", { enabled: false });
  await engine.call("set-sky-occlusion", { enabled: false });
  await engine.settle(2500);
  const giOff = await wholeFrameMean("gi-off");

  expect(engine.validationErrors()).toEqual([]);
  return { giOn, giOff, stats };
}

beforeAll(async () => {
  runs.on = await measure(true);
  runs.off = await measure(false);
}, 400_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the resident field's own pixels move the frame by nothing", () => {
  // The control. Without it the assertion below could pass on the blades' geometry alone, which is
  // the confusion the whole claim rests on: a field that draws is not a field that occludes.
  expect(Math.abs(runs.off!.giOff - runs.on!.giOff)).toBeLessThan(STILL_TOLERANCE);
});

test("a resident field darkens the lit frame it covers", () => {
  // The aggregate slab taking light out of the marches, and nothing else — the control above shows
  // the blades' pixels account for none of it. With no slab occluder emitted this is zero.
  expect(runs.off!.giOn - runs.on!.giOn).toBeGreaterThan(OCCLUSION_FLOOR);
});

test("resident tiles materialize blade geometry into the acceleration structures", () => {
  if (!rtSupported) {
    return;
  }
  // Generated topology takes the per-frame full-rebuild path, so each materialized tile is one such
  // structure and one TLAS instance. With the reconstruction suppressed there are none.
  expect(runs.on!.stats.tessellatedBlasCount).toBeGreaterThan(0);
  expect(runs.off!.stats.tessellatedBlasCount).toBe(0);
  expect(runs.on!.stats.rtInstances).toBeGreaterThan(runs.off!.stats.rtInstances);
});
