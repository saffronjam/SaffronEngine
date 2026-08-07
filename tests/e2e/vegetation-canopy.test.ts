// A multi-object plant family renders every part, and a phenotype flip changes the parts.
//
// The canopy fixture is the one family whose cooked hierarchy is a real assembly: two per-part
// prototypes (trunk and crown, split from one source by the family's submesh semantic targets), each
// placed by its own use, two material slots. Every other fixture is a trivial single-use family that
// draws as a plain mesh, so this is the only end-to-end coverage of the GPU assembly-use path — the
// fork, per-use records, per-use representation crossfades, and the combination masks.
//
// The phenotype assertion is the sharp one: the senescent phenotype's mask keeps only the trunk part
// active, so a flip removes the crown use's draw records and its pixels, and a flip back restores
// both. That proves combination masks gate uses over real per-part geometry and that a confirmed
// mutation re-mirrors its cell.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { PlantId, VegetationMutationRecordDto, WorldCellDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import {
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
  WIDE_BOUNDS,
} from "./vegetation-utils.ts";

// Faces the cooked cell's tallest plant from the far side (this map spans 64 m cells).
const CAMERA = { position: { x: 53, y: 7, z: -11 }, yaw: 180, pitch: -10 } as const;
// The senescent flip must move the frame at least this much (crowns really leave the
// picture; measured ~0.28) and the flip back must return it below the run-to-run noise
// floor (measured ~0.007).
const SHED_FLOOR = 0.1;
const RESTORE_TOLERANCE = 0.05;

const cleaner = new Cleaner();
let engine: Engine;
let subjects: { plant: PlantId; cell: WorldCellDto }[] = [];

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: 480, height: 270 });

  const fixture = loadFixture("vegetation-canopy");
  await importVegetationPackage(engine, cleaner, fixture, "canopy");
  await bindVegetationField(engine, cleaner, fixture, "Canopy vegetation");
  await cookCells(engine, fixture.map);
  await engine.call("set-camera", CAMERA);
  const deadline = Date.now() + 40_000;
  for (;;) {
    const hits = await queryPlants(engine, WIDE_BOUNDS);
    if (hits.hits.length > 0) {
      subjects = hits.hits.map((hit) => ({ plant: hit.plant.plant, cell: hit.plant.cell }));
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for resident canopy plants");
    }
    await engine.settle(50);
  }
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(2500);
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the assembly family draws, and its crossfades settle at the fine cut", async () => {
  // Settled means no aggregate remains on the cut: a perpetual representation crossfade
  // draws the family's voxel box over the fine geometry every frame, which is exactly the
  // defect this suite pins.
  await engine.call("set-hierarchy-cut", { cut: "fine" });
  await engine.settle(1500);
  const gpu = await engine.call("gpu-scene-stats");
  expect(gpu.visibility.voxelRecords).toBe(0);
  expect(gpu.visibility.records).toBeGreaterThan(0);
  await engine.call("set-hierarchy-cut", { cut: "auto" });
  expect(engine.validationErrors()).toEqual([]);
});

test("a confirmed phenotype flip re-mirrors the cell and masks the crown's records", async () => {
  const override = async (transaction: string, phenotype: number) => {
    await engine.call("vegetation-mutate", {
      gesture: `e2e0000000000000000000000000${transaction}`,
      records: subjects.map<VegetationMutationRecordDto>((subject, index) => ({
        header: {
          cell: subject.cell,
          transaction: `e2e0000000000000000000000000${transaction}`,
          authority: "e2e0000000000000000000000000ca01",
          logicalTick: "1",
          idempotencyKey: `e2e00000000000000000000000${transaction}${index.toString(16).padStart(2, "0")}`,
        },
        mutation: {
          kind: "state-override",
          plant: subject.plant,
          phenotype,
          lifecycle: null,
          health: null,
          moisture: null,
          fuel: null,
          interactionPolicy: null,
        },
      })),
    });
  };
  // The masks gate USES, and the aggregate representation is the merged family — its box
  // draws identically whichever uses are active — so the difference exists only on the
  // fine cut. Both halves are asserted: structural (the crown use's records vanish and
  // return — a confirmed mutation must bump the cell's bulk revision or the mirror keeps
  // rendering the old combination while the runtime reports the new one) and visual (the
  // crowns leave the picture and come back — a use placing more than its own part's
  // geometry passes the record assertion while the image never changes).
  await engine.call("set-hierarchy-cut", { cut: "fine" });
  await engine.settle(1500);
  const records = async () => (await engine.call("gpu-scene-stats")).visibility.records;
  const healthy = await records();
  const healthyFrame = decodeRgb8Png(await captureViewport(engine, cleaner, "canopy-healthy"));

  await override("9001", 1);
  await engine.settle(1500);
  const bare = await records();
  expect(bare).toBeLessThan(healthy);
  const bareFrame = decodeRgb8Png(await captureViewport(engine, cleaner, "canopy-bare"));
  const shed = meanAbsoluteDifference(healthyFrame, bareFrame);

  await override("9002", 0);
  await engine.settle(1500);
  const restored = await records();
  const restoredFrame = decodeRgb8Png(await captureViewport(engine, cleaner, "canopy-restored"));
  await engine.call("set-hierarchy-cut", { cut: "auto" });
  expect(restored).toBe(healthy);
  expect(shed).toBeGreaterThan(SHED_FLOOR);
  expect(meanAbsoluteDifference(healthyFrame, restoredFrame)).toBeLessThan(RESTORE_TOLERANCE);
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);

test("on a cluster-AS device the family's structures compose from its cooked clusters", async () => {
  const stats = await engine.call("render-stats");
  if (!stats.rtSupported) {
    return;
  }
  await engine.call("set-rt-shadows", { enabled: true });
  await engine.settle(800);
  const rt = await engine.call("render-stats");
  await engine.call("set-rt-shadows", { enabled: false });
  // The canopy family is the assembly: per-part prototypes whose bottom levels compose
  // from the cooked clusters when the device has the extension. The counts are
  // deduplicated by structure, so however many plants instance the family, at least one
  // composed structure with at least one CLAS must be referenced — and none on a device
  // without the extension, where the same prototypes take the KHR build.
  expect(rt.rtInstances).toBeGreaterThan(0);
  if (rt.clusterAsSupported) {
    expect(rt.clusterBlasCount).toBeGreaterThan(0);
    expect(rt.clasCount).toBeGreaterThanOrEqual(rt.clusterBlasCount);
  } else {
    expect(rt.clusterBlasCount).toBe(0);
  }
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);
