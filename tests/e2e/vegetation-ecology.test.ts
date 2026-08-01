// Ecology through the real host: the world simulation clock ages biology while play runs, an
// explicit step drives the same ticks under the same rules, dependency regions catch up under a
// budget, a region behind world time publishes nothing for a facet to read, and the route taken to a
// tick does not change the state it reaches. One cooked cell streams resident, then the test drives
// the clock over the control plane and compares checkpoint identities — the same comparison the
// in-crate byte-for-byte test makes, but through the wire, against a live runtime.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import {
  BOUNDS,
  CELL,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
  UNFILTERED,
} from "./vegetation-utils.ts";

// Growing weather: enough water and warmth that a tick is not dormant.
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
  return engine.call("vegetation-advance-ecology", {
    targetTick: String(targetTick),
    maxTicks,
  });
}

async function status() {
  return engine.call("vegetation-ecology-status");
}

async function clock(params: Record<string, unknown> = {}) {
  return engine.call("vegetation-ecology-clock", params);
}

async function navigation() {
  return engine.call("vegetation-nav-contributions", {});
}

test("biological time advances, regions catch up under a budget, and the route does not matter", async () => {
  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "ecology");

  await bindVegetationField(engine, cleaner, fixture, "Ecology vegetation");

  await cookCells(engine, fixture.map);

  await engine.call("set-camera", { position: { x: 32, y: 8, z: 44 }, yaw: 0, pitch: -10 });
  await engine.settle(200);

  // Wait for a resident macro plant: a region only runs while every cell it spans is resident.
  {
    const deadline = Date.now() + 30_000;
    for (;;) {
      const hits = await queryPlants(engine, BOUNDS);
      if (hits.hits.length > 0) {
        break;
      }
      if (Date.now() >= deadline) {
        const runtime = await engine.call("vegetation-runtime-status");
        throw new Error(`timeout waiting for a resident macro plant: ${JSON.stringify(runtime)}`);
      }
      await engine.settle(50);
    }
  }

  // Stop the world clock so the explicit steps below are the only thing moving biology. A running
  // clock works off what a budget leaves owed, which is the point of it — and would finish these
  // ticks between two commands.
  const stopped = await clock({ running: false, ...WEATHER });
  expect(stopped.running).toBe(false);
  expect(stopped.water).toBe(WEATHER.water);
  expect(stopped.warmth).toBe(WEATHER.warmth);

  const before = await status();
  expect(before.worldTick).toBe("0");
  expect(before.simulationVersion).toBeGreaterThan(0);
  expect(before.regions.length).toBeGreaterThan(0);
  expect(before.regionRadiusCells).toBeGreaterThan(0);
  expect(before.clock.running).toBe(false);
  expect(before.regions.every((region) => region.caughtUp)).toBe(true);

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
  expect(midway.clock.ticksOwed).toBe("5");

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

test("the world simulation clock ages biology while play runs, and the telemetry counts its ticks", async () => {
  // One tick per 16 ms of simulated play, so a short play window earns several.
  const configured = await clock({
    running: true,
    tickMilliseconds: 16,
    maxTicksPerSync: 8,
    ...WEATHER,
  });
  expect(configured.running).toBe(true);
  expect(configured.tickMilliseconds).toBe(16);
  expect(configured.maxTicksPerSync).toBe(8);
  expect(configured.workers).toBeGreaterThan(0);

  const before = await status();
  const ticksBefore = Number(
    (await engine.call("vegetation-telemetry")).work.ecologyTicks,
  );

  await engine.call("play");
  await engine.settle(600);
  await engine.call("stop");

  const after = await status();
  expect(Number(after.worldTick)).toBeGreaterThan(Number(before.worldTick));
  expect(after.checkpoint).not.toBe(before.checkpoint);
  // Nothing asked for these ticks: the world advanced and biology advanced with it.
  const telemetry = await engine.call("vegetation-telemetry");
  expect(Number(telemetry.work.ecologyTicks)).toBeGreaterThan(ticksBefore);

  // Stopping the clock stops biology: no further tick, and no payment of what a budget left owed.
  const halted = await clock({ running: false });
  expect(halted.running).toBe(false);
  const held = await status();
  await engine.call("play");
  await engine.settle(300);
  await engine.call("stop");
  const stillHeld = await status();
  expect(stillHeld.worldTick).toBe(held.worldTick);
  expect(stillHeld.checkpoint).toBe(held.checkpoint);

  expect(engine.validationErrors()).toEqual([]);
});

test("a cell mid-catch-up publishes for no facet", async () => {
  // The physics and navigation facets are claimed only in play, so the seam has to be live before
  // the gate is observable at all.
  await clock({ running: false });
  await engine.call("play");
  const deadline = Date.now() + 30_000;
  for (;;) {
    if (Number((await navigation()).contributions) > 0) {
      break;
    }
    if (Date.now() >= deadline) {
      await engine.call("stop");
      throw new Error("timeout waiting for the navigation seam to publish a contribution");
    }
    await engine.settle(100);
  }

  // Leave the resident region behind world time. Mid-catch-up its lifecycle state changes every
  // tick, so the seam retires what it published rather than republishing biology from a moment nobody
  // was meant to observe — and a consumer is spared a tile rebuild per executed tick.
  const target = Number((await status()).worldTick) + 4;
  const behindReport = await advance(target, 1);
  expect(behindReport.ticksOwed).toBe("3");
  // The one cell is resident, so the arrears are work the next call runs, not ground waiting to load.
  expect(behindReport.ticksAwaitingResidency).toBe("0");
  expect(behindReport.regionsAwaitingResidency).toBe(0);
  expect(behindReport.workers).toBeGreaterThan(0);
  await engine.settle(150);
  expect(Number((await navigation()).contributions)).toBe(0);

  // Reaching world time settles the cell again, and the seam republishes it.
  const caughtUp = await advance(target, 8);
  expect(caughtUp.ticksOwed).toBe("0");
  await engine.settle(150);
  expect(Number((await navigation()).contributions)).toBeGreaterThan(0);

  await engine.call("stop");
  expect(engine.validationErrors()).toEqual([]);
});

test("a volume reports what would burn, and ignition is persistent typed state", async () => {
  const sample = await engine.call("vegetation-combustion", { bounds: BOUNDS, filter: UNFILTERED });
  expect(sample.plants).toBeGreaterThan(0);
  expect(sample.ignited).toBe(0);
  expect(sample.occupancy).toBeGreaterThanOrEqual(0);
  // Growing ticks settled moisture toward the supply, so the sample is not the cooked zero.
  expect(sample.moisture).toBeGreaterThan(0);

  const hits = await queryPlants(engine, BOUNDS);
  const plant = hits.hits[0]!.plant.plant;
  const header = (transaction: string, key: string) => ({
    cell: CELL,
    transaction: `e2e0000000000000000000000000${transaction}`,
    authority: "e2e0000000000000000000000000ec01",
    logicalTick: "1",
    idempotencyKey: `e2e0000000000000000000000000${key}`,
  });
  await engine.call("vegetation-mutate", {
    gesture: "e2e00000000000000000000000ec0001",
    records: [{ header: header("7001", "8001"), mutation: { kind: "ignite", plant } }],
  });
  const alight = await engine.call("vegetation-combustion", { bounds: BOUNDS, filter: UNFILTERED });
  expect(alight.ignited).toBe(1);

  await engine.call("vegetation-mutate", {
    gesture: "e2e00000000000000000000000ec0002",
    records: [{ header: header("7002", "8002"), mutation: { kind: "extinguish", plant } }],
  });
  const out = await engine.call("vegetation-combustion", { bounds: BOUNDS, filter: UNFILTERED });
  expect(out.ignited).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});
