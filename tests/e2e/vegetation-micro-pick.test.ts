// A reconstructed grass blade is pickable, and it stands where the cooked density put it.
//
// A blade has no CPU instance and no persistent identity: it is regenerated on the device every
// frame from one texel of a cooked density grid. The viewport pick still has to answer for it,
// because a paint tool aiming at grass must learn that it hit grass rather than nothing, and must
// never be handed a PlantId it would treat as a saved object. The canonical acceptance run asserts
// the negative half of that (see `pickGroundClearOfTrunks`); this run asserts the hit.
//
// The fixture also decides where the blades are, which is the second claim here. Its understory
// scatters at 1 m, 3 m, … 63 m on both axes of the 64 m cell, against a 64x1x32 density grid: one
// candidate in every odd 1 m column of X, and one in every 2 m row of Z. So the occupied texels
// span the cell's whole X range while filling only half of it, and a reader that walked the grid
// X-fastest instead of Z-fastest would fold every blade into x >= 32 m — a shift square dimensions
// could never show, because there a transposed read is a symmetric relabel.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { PickResult } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import { awaitMicroCandidates } from "./vegetation-graph-cook.ts";
import {
  awaitResidentCell,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";

const CELL_EDGE_M = 64;

// Eyes parked inside an occupied 1 m column of the cell's near half, at blade-root height and
// dead level, so the ray runs the column's whole length through the layer the blades occupy
// instead of crossing it. Every station sits clear of the two macro trunks at x = 16 and x = 48.
const STATIONS = [
  { position: { x: 3.5, y: 0.1, z: 62 }, yaw: 0, pitch: 0 },
  { position: { x: 7.5, y: 0.1, z: 62 }, yaw: 0, pitch: 0 },
  { position: { x: 23.5, y: 0.1, z: 62 }, yaw: 0, pitch: 0 },
  { position: { x: 3.5, y: 0.1, z: 2 }, yaw: 180, pitch: 0 },
  { position: { x: 7.5, y: 0.1, z: 2 }, yaw: 180, pitch: 0 },
  { position: { x: 23.5, y: 0.1, z: 2 }, yaw: 180, pitch: 0 },
] as const;

// Aims hugging the view axis: further out the ray leaves its column, and with it the cell half
// the placement claim rests on.
const AIMS = [0.47, 0.5, 0.53];

const cleaner = new Cleaner();
let engine: Engine;
let attempts = 0;
const microPicks: PickResult[] = [];

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  const fixture = loadFixture("vegetation-micro-pick");
  await importVegetationPackage(engine, cleaner, fixture, "micro-pick");
  await bindVegetationField(engine, cleaner, fixture, "Micro pick understory");
  await cookCells(engine, fixture.map);
  await awaitResidentCell(engine, 1, { microTiles: true });
  await awaitMicroCandidates(engine);

  for (const station of STATIONS) {
    await engine.call("set-camera", station);
    await engine.settle(150);
    for (const u of AIMS) {
      attempts += 1;
      const picked = await engine.call("pick", { u, v: 0.5 });
      if (picked.kind === "micro-vegetation") {
        microPicks.push(picked);
      }
    }
  }
}, 240_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("a ray into the understory picks a blade as nonpersistent micro vegetation", () => {
  expect(attempts).toBe(STATIONS.length * AIMS.length);
  expect(microPicks.length).toBeGreaterThan(0);
  for (const picked of microPicks) {
    expect(picked.hit).toBe(true);
    // Cosmetic, so there is nothing to select and nothing to save.
    expect(picked.plant).toBeUndefined();
    expect(picked.id).toBeUndefined();
    const point = picked.position ?? [];
    expect(point).toHaveLength(3);
    expect(point[0]).toBeGreaterThanOrEqual(0);
    expect(point[0]).toBeLessThan(CELL_EDGE_M);
    expect(point[2]).toBeGreaterThanOrEqual(0);
    expect(point[2]).toBeLessThan(CELL_EDGE_M);
    // Blades root on the cell floor and stand well under a metre.
    expect(point[1]).toBeGreaterThanOrEqual(-0.1);
    expect(point[1]).toBeLessThan(1);
  }
});

test("blades reconstruct in the cell half their density samples cover", () => {
  // Every station aims down a column of the cell's near half, so a blade a ray lands on is a blade
  // the grid placed there; a transposed texel read moves the whole field into the far half. The
  // minimum carries the claim that any blade was hit at all, the maximum that none came from the
  // half no station looks at.
  const xs = microPicks.map((picked) => picked.position![0]!);
  expect(Math.min(...xs)).toBeLessThan(CELL_EDGE_M / 2);
  expect(Math.max(...xs)).toBeLessThan(CELL_EDGE_M / 2);
});

test("the understory renders validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
