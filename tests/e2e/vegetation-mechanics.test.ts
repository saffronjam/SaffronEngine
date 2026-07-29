// A plant family's authored wind response reaches the GPU and changes how it moves.
//
// `MechanicalResponse` — stiffness, damping, drag, flutter, bend limit — was authored on
// `.splant`, written into the cooked part table, and read by nothing. The wind prepass
// derived its whole response from the plant's height, so a stiff sapling and a supple reed
// of the same height swayed identically no matter what the author wrote.
//
// The chain being proven is four links long and every one of them could drop the value
// silently: the part-table decoder, the family render load, the mirror's prototype record,
// and the prepass that reads it. A unit test covers the first link and a mirror test the
// third; only a running host covers all four, because only there does the record the GPU
// actually wrote come back.
//
// `vegetation-wind-record` is that readback. It is an explicit one-shot capture — the
// record buffer is device-local and reading it idles the queue, which is fine when a person
// asks a question and ruinous every frame.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
  VegetationRuntimeQueryResult,
  VegetationWindRecordResult,
} from "@saffron/protocol";
import type { ActiveAlarmsDto, DrainAlarmsResult, GpuSceneStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, prepareScene } from "./test-utils.ts";
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
/// The first resident plant, and its captured record under a steady wind.
let plant: { plant: string; cell: typeof CELL } | undefined;
let record: VegetationWindRecordResult;
let stillRecord: VegetationWindRecordResult;
/// GPU-scene counters with the plant resident and the wind blowing.
let stats: GpuSceneStatsDto;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    width: 480,
    height: 270,
    camera: { position: { x: 2, y: 1, z: 4 }, yaw: 0, pitch: 0 },
  });

  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, "mechanics");
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  const world = await engine.call<EntityRef>("create-entity", { name: "Mechanics vegetation" });
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

  const deadline = Date.now() + 30_000;
  for (;;) {
    const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
      query: { kind: "bounds", bounds: BOUNDS },
    });
    const first = hits.hits[0];
    if (first) {
      plant = { plant: first.plant.plant, cell: CELL };
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for a resident macro plant");
    }
    await engine.settle(50);
  }
  await engine.settle(800);

  await engine.call("set-wind", { speed: 12, gust: 0.6 });
  await engine.settle(600);
  record = await engine.call<VegetationWindRecordResult>("vegetation-wind-record", plant);
  stats = await engine.call<GpuSceneStatsDto>("gpu-scene-stats");

  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(600);
  stillRecord = await engine.call<VegetationWindRecordResult>("vegetation-wind-record", plant);
}, 180_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the capture reaches a real mirrored plant", () => {
  expect(plant).toBeDefined();
  // Slot zero is a legitimate slot, so the check is that the capture happened at all.
  expect(Number.isInteger(record.slot)).toBe(true);
});

test("the family's authored response reaches the GPU", () => {
  // The whole point: nothing read these numbers before, so an all-zero prototype record
  // (the "not a plant family" value) would mean the chain still drops them.
  expect(record.mechanics).not.toBeNull();
  const mechanics = record.mechanics!;
  expect(mechanics.stiffness).toBeGreaterThan(0);
  expect(mechanics.drag).toBeGreaterThanOrEqual(0);
  expect(mechanics.flutter).toBeGreaterThanOrEqual(0);
  expect(mechanics.damping).toBeGreaterThanOrEqual(0);
  expect(mechanics.damping).toBeLessThanOrEqual(1);
  expect(mechanics.bendLimit).toBeGreaterThanOrEqual(0);
  expect(mechanics.bendLimit).toBeLessThanOrEqual(1);
});

test("wind moves the plant and the prepass records it", () => {
  const sway = Math.hypot(...record.swayCurrentM);
  expect(sway).toBeGreaterThan(0);
  // The branch mode is stored as a quadrature so the vertex path can apply a per-use
  // phase with no clock; a zero pair would mean no branch motion was evaluated at all.
  expect(Math.hypot(record.branchQuadrature[0], record.branchQuadrature[1])).toBeGreaterThan(0);
  expect(record.branchAmplitudeM).toBeGreaterThan(0);
  // The cull's slack must cover every displacement it is derived from, or the plant can
  // sway out of the bounds the visibility pass tested.
  expect(record.boundsInflationM).toBeGreaterThanOrEqual(sway);
});

test("a still field leaves the recorded sway at rest", () => {
  // The response scales the field; with no field there is nothing to scale, which is the
  // control that separates "the response is applied" from "the numbers are noise".
  expect(Math.hypot(...stillRecord.swayCurrentM)).toBeLessThan(
    Math.hypot(...record.swayCurrentM),
  );
  expect(stillRecord.branchAmplitudeM).toBeLessThan(record.branchAmplitudeM);
});

test("the prepass applies the response, not merely carries it", () => {
  // The link the assertions above cannot reach: they read the record the mirror published,
  // which would look identical if the shader loaded the response and then ignored it.
  //
  // The leaf term carries the proof. The fixture family authors flutter at 0.25, and the
  // prepass scales the flutter amplitude by it while leaving the branch amplitude alone,
  // so the ratio between the two separates the two worlds by 4x: ~0.15 with the response
  // applied against ~0.60 without it. Stiffness and drag are authored at 1.0 in this
  // fixture and so prove nothing by their value; flutter is the one that does, and the
  // shader reads all of them from the same struct.
  const mechanics = record.mechanics!;
  expect(mechanics.flutter).toBeLessThan(1);
  expect(record.branchAmplitudeM).toBeGreaterThan(0);
  expect(record.flutterAmplitudeM / record.branchAmplitudeM).toBeLessThan(0.3);
});

test("the height scale is the plant's, not a constant", () => {
  // `heightScale` is the reciprocal of the local top height and weights the whole vertex
  // path; a zero would flatten every plant's response to nothing.
  expect(record.heightScale).toBeGreaterThan(0);
  expect(record.heightScale).toBe(stillRecord.heightScale);
});

test("the deformed counter counts this plant", () => {
  // The positive half of the deformed-instance counter. `visibility-counters` proves it stays
  // zero for a scene with nothing wind-flagged, which a counter incrementing on every instance
  // would fail; this proves it is not simply always zero, which that test alone cannot.
  expect(stats.visibility.deformed).toBeGreaterThan(0);
  expect(stats.visibility.deformed).toBeLessThanOrEqual(stats.visibility.visible);
});

test("a camera jump across a cascade edge marks the plants reactive; standing still does not", async () => {
  // The interaction field follows the camera and zeroes every texel that scrolls in, so an
  // instance whose covering cascade changed reads a displacement that JUMPED rather than
  // moved. Reprojecting that jump smears it across the accumulation window, which is why the
  // prepass flags the instance and the reactive-coverage pass marks it.
  //
  // The counter is a running total on purpose. A reset is an event lasting one frame, and no
  // caller can time a stats query to the frame the camera crossed an edge.
  const resets = async () =>
    (await engine.call<GpuSceneStatsDto>("gpu-scene-stats")).visibility.interactionResets;

  const idle = await resets();
  await engine.settle(500);
  // The control, and the half that fails if the flag were simply always set: a still camera
  // holds the cascade centres still, so no instance changes cascade and nothing is flagged.
  expect(await resets()).toBe(idle);
  const stillCapture = await engine.call<VegetationWindRecordResult>(
    "vegetation-wind-record",
    plant!,
  );
  expect(stillCapture.interactionReset).toBe(false);

  // Cascade 0 is 256 texels of 0.25 m: a 64 m window centred on the eye. The plants sit near
  // the origin, so putting the eye 40 m out drops them into cascade 1 — separate, coarser
  // state — in a single frame.
  await engine.call("set-camera", { position: { x: 42, y: 1, z: 4 }, yaw: 0, pitch: 0 });
  await engine.settle(500);
  await engine.call("set-camera", { position: { x: 2, y: 1, z: 4 }, yaw: 0, pitch: 0 });
  await engine.settle(500);
  expect(await resets()).toBeGreaterThan(idle);

  // And the flag is not sticky: with the camera at rest again the plants are ordinary
  // deformed instances, which is what keeps the reactive mask from disabling TAA outright.
  const settled = await resets();
  await engine.settle(400);
  expect(await resets()).toBe(settled);
}, 60_000);

test("a tightened budget alarms on the cell and the family that broke it", async () => {
  // "Actionable" is the whole claim here. The renderer's own detectors read frame timings and GPU
  // counters and can name a pass at best — which cell filled up, and which family filled it, is
  // invisible from there and is exactly what an author needs to go and fix. The breach is computed
  // where the population is known and handed to the alarm machinery with its owner attached.
  const vegetationAlarms = async () =>
    (await engine.call<ActiveAlarmsDto>("list-active-alarms")).alarms.filter((alarm) =>
      alarm.metric.startsWith("vegetation-"),
    );

  // The default budgets are generous on purpose, so a fixture cell sits well inside them. Without
  // this the assertions below could pass on alarms that were already firing.
  expect(await vegetationAlarms()).toEqual([]);

  await engine.call("vegetation-budgets", { cellPlants: 1, familyInstances: 1 });
  await engine.settle(500);
  const firing = await vegetationAlarms();
  const cell = firing.find((alarm) => alarm.metric === "vegetation-cell-plants");
  const family = firing.find((alarm) => alarm.metric === "vegetation-family-instances");
  expect(cell).toBeDefined();
  expect(family).toBeDefined();
  // The ownership itself, not merely that something fired: a cell alarm carries its coordinates
  // and level, a family alarm carries the catalog name the author authored it under.
  expect(cell!.owner).toMatch(/^cell -?\d+,-?\d+,-?\d+ L\d+$/);
  expect(family!.owner).toMatch(/^family .+ \(\d+\)$/);
  expect(cell!.value).toBeGreaterThan(cell!.threshold);

  // And the events carry it too, so a listener that never polls the active set still learns who.
  const drained = await engine.call<DrainAlarmsResult>("drain-alarms", { since: 0 });
  expect(
    drained.events.some(
      (event) => event.metric === "vegetation-cell-plants" && event.owner === cell!.owner,
    ),
  ).toBe(true);

  // Resolution is by ABSENCE — the reporter publishes the complete breach set every frame, so
  // restoring the budget has to clear the alarms rather than leave them firing forever.
  await engine.call("vegetation-budgets", { cellPlants: 4096, familyInstances: 16384 });
  await engine.settle(500);
  expect(await vegetationAlarms()).toEqual([]);
}, 60_000);

test("capturing a wind record is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
