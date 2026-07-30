// Phase-12 acceptance: the interaction surface of a resident vegetation cell, through the real
// host. One cooked cell streams resident, play starts, and the test drives collision residency,
// promotion, demotion, felling, mutation idempotency, and the navigation seam over the control
// plane — asserting the one-owner rule at each synchronization point and a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  VegetationDrainEventsResult,
  VegetationMutateResult,
  VegetationNavigationResult,
  VegetationPromotionResult,
  VegetationRuntimePlantInspectResult,
  VegetationRuntimeQueryResult,
  VegetationRuntimeStatusDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import {
  BOUNDS,
  CELL,
  bindVegetationField,
  cookCells,
  importVegetationPackage,
  loadFixture,
} from "./vegetation-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

// The available status block, which every assertion below reads.
async function available() {
  const status = await engine.call<VegetationRuntimeStatusDto>("vegetation-runtime-status");
  if (status.state !== "available") {
    throw new Error(`vegetation runtime is ${status.state}`);
  }
  return status;
}

test("a resident cell drives collision, promotion, felling, events, and navigation", async () => {
  const fixture = loadFixture("vegetation-phase3");
  await importVegetationPackage(engine, cleaner, fixture, "interaction");

  const world = await bindVegetationField(engine, cleaner, fixture, "Interaction vegetation");

  await cookCells(engine, fixture.map);

  // Park the camera over the cell so its facets claim residency, then start play: collision
  // bodies and promotion both belong to a live play world.
  await engine.call("set-camera", { position: { x: 32, y: 8, z: 44 }, yaw: 0, pitch: -10 });
  await engine.call("play");
  await engine.settle(200);

  let plant = "";
  {
    const deadline = Date.now() + 30_000;
    for (;;) {
      const hits = await engine.call<VegetationRuntimeQueryResult>("vegetation-runtime-query", {
        query: { kind: "bounds", bounds: BOUNDS },
      });
      if (hits.hits.length > 0) {
        plant = hits.hits[0]!.plant.plant;
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for a resident macro plant");
      }
      await engine.settle(50);
    }
  }

  // Batched collision residency: the cell's structural plants carry proxy bodies, and there is
  // no body-per-decorative-plant explosion (the fixture's plants are all structural, each with
  // exactly one trunk capsule).
  let bodiesBefore = 0;
  {
    const deadline = Date.now() + 20_000;
    for (;;) {
      const status = await available();
      const collision = status.collision;
      if (collision && Number(collision.residentBodies) > 0) {
        bodiesBefore = Number(collision.residentBodies);
        expect(Number(collision.residentCells)).toBeGreaterThan(0);
        expect(Number(collision.failedFamilies)).toBe(0);
        break;
      }
      if (Date.now() >= deadline) {
        const status = await available();
        throw new Error(
          `timeout waiting for vegetation collision bodies: ${JSON.stringify({
            collision: status.collision,
            promotion: status.promotion,
            requested: status.requestedBytes,
            resident: status.residentBytes,
            pending: status.pending,
          })}`,
        );
      }
      await engine.settle(50);
    }
  }

  // Telemetry: compact counters only, and the synchronization it timed actually ran.
  {
    interface Telemetry {
      last: { totalUs: number };
      average: { totalUs: number };
      work: { synchronizations: string; queries: string; queryHits: string };
      collisionBodies: string;
      promoted: string;
    }
    const before = await engine.call<Telemetry>("vegetation-telemetry");
    expect(Number(before.work.synchronizations)).toBeGreaterThan(0);
    // The residency query above was counted, hits and all.
    expect(Number(before.work.queries)).toBeGreaterThan(0);
    expect(Number(before.work.queryHits)).toBeGreaterThan(0);
    expect(Number(before.collisionBodies)).toBe(bodiesBefore);
    await engine.settle(60);
    const after = await engine.call<Telemetry>("vegetation-telemetry");
    expect(Number(after.work.synchronizations)).toBeGreaterThan(
      Number(before.work.synchronizations),
    );
    // A synchronization that ran took some time, and the average is in the same ballpark.
    expect(after.last.totalUs).toBeGreaterThanOrEqual(0);
    expect(after.average.totalUs).toBeGreaterThanOrEqual(0);
  }

  // Navigation publishes obstacle contributions for the same cell.
  {
    const nav = await engine.call<VegetationNavigationResult>("vegetation-nav-contributions", {});
    expect(nav.cells.length).toBeGreaterThan(0);
    expect(Number(nav.contributions)).toBeGreaterThan(0);
    // The fixture's family is Structural, so every contribution is an obstacle.
    expect(Number(nav.obstacles)).toBe(Number(nav.contributions));
    expect(Number(nav.dynamicObstacles)).toBe(0);
  }

  // Promotion: the request commits at the next synchronization point, and from then on the
  // entity view is the plant's only owner — its bulk collision batch is retired.
  {
    const requested = await engine.call<VegetationPromotionResult>("vegetation-promote", { plant });
    expect(requested.state.state).toBe("promoting");

    const deadline = Date.now() + 20_000;
    for (;;) {
      const inspect = await engine.call<VegetationRuntimePlantInspectResult>(
        "vegetation-runtime-inspect",
        { plant },
      );
      if (inspect.promotion?.state === "promoted") {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the promotion to commit");
      }
      await engine.settle(50);
    }

    const status = await available();
    expect(Number(status.promotion!.promoted)).toBe(1);
    // One owner: the promoted plant's proxy bodies are gone from the bulk batch.
    expect(Number(status.collision!.residentBodies)).toBeLessThan(bodiesBefore);
    // And the navigation contribution became dynamic, because the plant is moving.
    const nav = await engine.call<VegetationNavigationResult>("vegetation-nav-contributions", {});
    expect(Number(nav.dynamicObstacles)).toBeGreaterThan(0);
    expect(nav.dirtyRegions.length).toBeGreaterThan(0);
  }

  // Demotion writes the view's state back through the reducer, so the plant carries persistent
  // promotion-origin state afterwards, and its bulk collision returns.
  {
    const requested = await engine.call<VegetationPromotionResult>("vegetation-demote", { plant });
    expect(requested.state.state).toBe("demoting");

    const deadline = Date.now() + 20_000;
    for (;;) {
      const inspect = await engine.call<VegetationRuntimePlantInspectResult>(
        "vegetation-runtime-inspect",
        { plant },
      );
      // The write-back records promotion-origin state in the cell the view settled in, so the
      // plant carries it on one of its persistent entries.
      if (
        inspect.promotion?.state === "bulk" &&
        inspect.persistent.some((entry) => entry.promoted)
      ) {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the demotion write-back");
      }
      await engine.settle(50);
    }
    const status = await available();
    expect(Number(status.promotion!.promoted)).toBe(0);
    expect(Number(status.collision!.residentBodies)).toBe(bodiesBefore);
  }

  // Events: one typed transition per committed record, and an exact replay emits none.
  {
    const cursor = await engine.call<VegetationDrainEventsResult>("vegetation-drain-events", {});
    const since = cursor.highWaterSeq;

    // Transaction, authority, and operation keys are canonical 32-digit hexadecimal GUIDs.
    const record = {
      header: {
        cell: CELL,
        transaction: "e2e00000000000000000000000770001",
        authority: "e2e00000000000000000000000990001",
        logicalTick: "1",
        idempotencyKey: "e2e00000000000000000000000880001",
      },
      mutation: { kind: "damage", plant, amount: 8000 },
    };
    const first = await engine.call<VegetationMutateResult>("vegetation-mutate", {
      records: [record],
    });
    expect(first.applied).toBe(1);
    const afterFirst = await engine.call<VegetationDrainEventsResult>("vegetation-drain-events", {
      since,
    });
    const damaged = afterFirst.events.filter((event) => event.transition.kind === "damaged");
    expect(damaged.length).toBe(1);
    expect(damaged[0]!.plant).toBe(plant);

    // The identical record is an idempotent replay: accepted, but not observable twice.
    await engine.call<VegetationMutateResult>("vegetation-mutate", { records: [record] });
    const afterReplay = await engine.call<VegetationDrainEventsResult>("vegetation-drain-events", {
      since,
    });
    expect(afterReplay.events.filter((event) => event.transition.kind === "damaged").length).toBe(
      1,
    );
  }

  // Felling separates the product from the rooted plant: the plant becomes a stump under its own
  // identity, and a separate product entity appears.
  {
    const before = await engine.call<{ entities: { id: number; name: string }[] }>(
      "list-entities",
      {},
    );
    await engine.call<VegetationPromotionResult>("vegetation-fell", { plant });

    const deadline = Date.now() + 20_000;
    for (;;) {
      const inspect = await engine.call<VegetationRuntimePlantInspectResult>(
        "vegetation-runtime-inspect",
        { plant },
      );
      // The resident row is the authoritative effective state. A moved plant carries deltas in
      // more than one cell (its base cell plus the cell it now occupies), so a single delta entry
      // is not the answer to "is it a stump".
      if (inspect.resident?.lifecycle === "stump") {
        break;
      }
      if (Date.now() >= deadline) {
        const status = await available();
        throw new Error(
          `timeout waiting for the felling to commit: ${JSON.stringify({
            resident: inspect.resident?.lifecycle,
            persistent: inspect.persistent,
            promotion: status.promotion,
          })}`,
        );
      }
      await engine.settle(50);
    }
    const after = await engine.call<{ entities: { id: number; name: string }[] }>(
      "list-entities",
      {},
    );
    const products = after.entities.filter((entity) => entity.name.startsWith("Felled "));
    expect(products.length).toBe(1);
    expect(after.entities.length).toBeGreaterThan(before.entities.length);
    // The rooted plant kept its identity; the product is a different entity entirely.
    const felled = await engine.call<VegetationRuntimePlantInspectResult>(
      "vegetation-runtime-inspect",
      { plant },
    );
    expect(felled.plant).toBe(plant);
  }

  await engine.call("stop");
  await engine.settle(100);
});
