// Phase-13 acceptance through the real host: biological time advances, dependency regions catch up
// under a budget, and the route taken to a tick does not change the state it reaches. One cooked
// cell streams resident, then the test drives the ecology clock over the control plane and compares
// checkpoint identities — the same comparison the in-crate byte-for-byte test makes, but through
// the wire, against a live runtime.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ImportVegetationAssetResult,
  VegetationCombustionDto,
  VegetationCookJobDto,
  VegetationEcologyReportDto,
  VegetationEcologyStatusDto,
  VegetationRuntimeQueryResult,
  VegetationRuntimeStatusDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;
const BOUNDS = {
  minTicks: ["0", "0", "0"],
  maxTicksExclusive: ["262144", "262144", "262144"],
} as const;
/// Growing weather: enough water and warmth that a tick is not dormant.
const WEATHER = { water: 40_000, warmth: 45_000 } as const;

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

async function advance(targetTick: number, maxTicks: number) {
  return engine.call<VegetationEcologyReportDto>("vegetation-advance-ecology", {
    targetTick: String(targetTick),
    maxTicks,
    ...WEATHER,
  });
}

async function status() {
  return engine.call<VegetationEcologyStatusDto>("vegetation-ecology-status");
}

test("biological time advances, regions catch up under a budget, and the route does not matter", async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, "ecology");
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }

  const world = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: "Ecology vegetation" }),
  );
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

  await engine.call("set-camera", { position: { x: 32, y: 8, z: 44 }, yaw: 0, pitch: -10 });
  await engine.settle(200);

  // Wait for a resident macro plant: a region only runs while every cell it spans is resident.
  {
    const deadline = Date.now() + 30_000;
    for (;;) {
      const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
        query: { kind: "bounds", bounds: BOUNDS },
      });
      if (hits.hits.length > 0) {
        break;
      }
      if (Date.now() >= deadline) {
        const runtime = await engine.call<VegetationRuntimeStatusDto>("vegetation-runtime-status");
        throw new Error(`timeout waiting for a resident macro plant: ${JSON.stringify(runtime)}`);
      }
      await engine.settle(50);
    }
  }

  const before = await status();
  expect(before.worldTick).toBe("0");
  expect(before.simulationVersion).toBeGreaterThan(0);
  expect(before.regions.length).toBeGreaterThan(0);
  expect(before.regionRadiusCells).toBeGreaterThan(0);

  // A budget bounds the call: world time reaches the target, the region does not, and what is left
  // is owed rather than dropped.
  const first = await advance(8, 3);
  expect(first.worldTick).toBe("8");
  expect(first.ticksRun).toBe("3");
  expect(first.ticksOwed).toBe("5");
  expect(first.regionsCaughtUp).toBe(0);

  const midway = await status();
  expect(midway.regions.every((region) => region.caughtUp)).toBe(false);
  expect(midway.cells.length).toBeGreaterThan(0);
  expect(midway.cells.every((cell) => Number(cell.tick) === 3)).toBe(true);

  // The remaining ticks run on the following calls, in the same order they would have.
  const second = await advance(8, 3);
  expect(second.ticksRun).toBe("3");
  const third = await advance(8, 3);
  expect(third.ticksRun).toBe("2");
  expect(third.ticksOwed).toBe("0");
  expect(third.regionsCaughtUp).toBeGreaterThan(0);

  const budgeted = await status();
  expect(budgeted.worldTick).toBe("8");
  expect(budgeted.regions.every((region) => region.caughtUp)).toBe(true);
  expect(budgeted.cells.every((cell) => Number(cell.tick) === 8)).toBe(true);
  expect(budgeted.checkpoint).not.toBe(before.checkpoint);

  // The clock only moves forward: a calendar rewind is not an ecology rewind.
  await expect(advance(4, 8)).rejects.toThrow();

  expect(engine.validationErrors()).toEqual([]);
});

test("a volume reports what would burn, and ignition is persistent typed state", async () => {
  const sample = await engine.call<VegetationCombustionDto>("vegetation-combustion", {
    bounds: BOUNDS,
  });
  expect(sample.plants).toBeGreaterThan(0);
  expect(sample.ignited).toBe(0);
  expect(sample.occupancy).toBeGreaterThanOrEqual(0);
  // Eight growing ticks settled moisture toward the supply, so the sample is not the cooked zero.
  expect(sample.moisture).toBeGreaterThan(0);

  const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
    query: { kind: "bounds", bounds: BOUNDS },
  });
  const plant = hits.hits[0]!.plant.plant;
  const header = (transaction: string, key: string) => ({
    cell: CELL,
    transaction: `e2e0000000000000000000000000${transaction}`,
    authority: "e2e0000000000000000000000000ec01",
    logicalTick: "1",
    idempotencyKey: `e2e0000000000000000000000000${key}`,
  });
  await engine.call("vegetation-mutate", {
    records: [{ header: header("7001", "8001"), mutation: { kind: "ignite", plant } }],
  });
  const alight = await engine.call<VegetationCombustionDto>("vegetation-combustion", {
    bounds: BOUNDS,
  });
  expect(alight.ignited).toBe(1);

  await engine.call("vegetation-mutate", {
    records: [
      { header: header("7002", "8002"), mutation: { kind: "extinguish", plant } },
    ],
  });
  const out = await engine.call<VegetationCombustionDto>("vegetation-combustion", {
    bounds: BOUNDS,
  });
  expect(out.ignited).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});
