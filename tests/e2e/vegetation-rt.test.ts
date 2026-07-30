// Cooked plants participate in ray tracing.
//
// Vegetation never enters the ECS — it streams through the GPU-scene mirror — so a CPU scan that
// builds the ray-instance list from entities alone cannot see it, and a canopy that renders but
// casts no ray-traced shadow is the visible consequence.
//
// Two shapes of plant, one claim. A multi-prototype family cannot be one bottom-level structure:
// KHR acceleration structures have no notion of nested micro-instance parts, so the family carries
// one structure per prototype and each placed use becomes its own TLAS instance. A single-prototype
// family is cooked as a plain mesh and places directly. Both must reach the TLAS, and requiring the
// assembly form silently drops the second.
//
// The counters are the observable: `rtInstances` counts TLAS instances while `blasCount` counts the
// distinct structures they reference, so plants arriving must move the first.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  RenderStatsDto,
  VegetationRuntimeQueryResult,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, prepareScene } from "./test-utils.ts";
import {
  BOUNDS,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;
let rtSupported = false;
// TLAS counters before the vegetation cell became resident, and after.
let before: RenderStatsDto;
let after: RenderStatsDto;
let residentPlants = 0;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    width: 480,
    height: 270,
    camera: { position: { x: 16, y: 4, z: 22 }, yaw: 0, pitch: 0 },
  });
  rtSupported = (await engine.call<RenderStatsDto>("render-stats")).rtSupported;
  if (!rtSupported) {
    return;
  }
  // Arming ray-query shadows is what makes the host build a TLAS at all.
  await engine.call("set-rt-shadows", { enabled: true });
  await engine.settle(400);
  before = await engine.call<RenderStatsDto>("render-stats");

  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "rt");
  const world = await bindVegetationField(engine, cleaner, fixture, "RT vegetation");
  await cookCells(engine, fixture.map);
  await engine.settle(200);

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
      query: { kind: "bounds", bounds: BOUNDS },
    });
    if (hits.hits.length > 0) {
      residentPlants = hits.hits.length;
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for a resident macro plant");
    }
    await engine.settle(50);
  }
  // Residency is reported by the runtime before the mirror has necessarily published the
  // matching instances, so settle before reading the counters.
  await engine.settle(800);
  after = await engine.call<RenderStatsDto>("render-stats");
}, 180_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("resident plants add TLAS instances", () => {
  if (!rtSupported) {
    return;
  }
  expect(residentPlants).toBeGreaterThan(0);
  // One instance per resident plant at minimum; a multi-prototype family contributes one per
  // active use, so the arrival is bounded below, not equated.
  expect(after.rtInstances).toBeGreaterThanOrEqual(before.rtInstances + residentPlants);
});

test("the skinning path does not splice the plants in a second time", async () => {
  if (!rtSupported) {
    return;
  }
  // Vegetation reaches the TLAS through the mirror rather than the ECS, so it is spliced in by
  // hand — and exactly ONCE, through the cascade-window gate. A second ungated splice from the
  // skinned gather would draw every plant twice with the copy ignoring the cut. The count is the
  // observable: toggling the skinning path may not move it, because plants are not skinned either
  // way.
  const enabled = await engine.call<RenderStatsDto>("render-stats");
  await engine.call("set-skinning", { enabled: false });
  await engine.settle(400);
  const disabled = await engine.call<RenderStatsDto>("render-stats");
  await engine.call("set-skinning", { enabled: true });
  await engine.settle(400);

  expect(disabled.rtInstances).toBe(enabled.rtInstances);
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);

test("the plants' geometry is a real structure, not the scene's", () => {
  if (!rtSupported) {
    return;
  }
  // The family brings its own bottom-level structure rather than reusing whatever the starter
  // scene had — otherwise the instance count could rise while the plants referenced nothing.
  expect(after.blasCount).toBeGreaterThan(before.blasCount);
  expect(Number(after.blasBytes)).toBeGreaterThan(Number(before.blasBytes));
});

test("placing plants in the TLAS is validation-clean", () => {
  if (!rtSupported) {
    return;
  }
  expect(engine.validationErrors()).toEqual([]);
});
