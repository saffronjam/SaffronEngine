// Phase-12 acceptance: the interaction surface of a resident vegetation cell, through the real
// host. One cooked cell streams resident, play starts, and the test drives collision residency,
// promotion, demotion, felling, mutation idempotency, and the navigation seam over the control
// plane — asserting the one-owner rule at each synchronization point and a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import type {
  VegetationMutationRecordDto,
  VegetationNavigationContributionDto,
  WorldBoundsDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import {
  awaitCook,
  bindVegetationField,
  BOUNDS,
  CELL,
  cookCells,
  importVegetationPackage,
  loadFixture,
  queryPlants,
  TICKS_PER_METER,
} from "./vegetation-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;
const fixture = loadFixture("vegetation-phase3");
let world = "";
// The plant the reload test removes outright, which the generation change below must keep removed.
let removedPlant = "";

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

// The available status block, which every assertion below reads.
async function available() {
  const status = await engine.call("vegetation-runtime-status");
  if (status.state !== "available") {
    throw new Error(`vegetation runtime is ${status.state}`);
  }
  return status;
}

// Waits until `predicate` holds, re-reading through `read` — the shape every synchronization the
// engine commits on its own frame loop is observed through.
async function settleUntil<T>(
  read: () => Promise<T>,
  predicate: (value: T) => boolean,
  what: string,
  timeoutMs = 20_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await read();
    if (predicate(value)) {
      return value;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for ${what}: ${JSON.stringify(value)}`);
    }
    await engine.settle(50);
  }
}

// Counts, at one synchronization point, how many owners each of the three systems holds for the
// cell's macro plants: bulk vegetation instances plus promoted entity views must equal the macro
// row count exactly, on every system. A plant that kept its bulk owner while its view came up
// pushes a sum to `macro + 1`, and a plant that lost both pushes it to `macro - 1`.
async function assertOneOwnerPerPlant(macro: number) {
  const status = await available();
  const render = await engine.call("vegetation-render-stats", {});
  const nav = await engine.call("vegetation-nav-contributions", {});
  const entities = await engine.call("list-entities", {});

  const promoted = Number(status.promotion!.promoted);
  const views = entities.entities.filter((entity) => entity.name.startsWith("Plant ")).length;
  expect(views).toBe(promoted);

  // Render: bulk vegetation instances plus entity views.
  const bulkInstances = render.cells.reduce((total, row) => total + Number(row.plants), 0);
  expect(bulkInstances + views).toBe(macro);

  // Collision: the fixture's family declares exactly one trunk capsule, so one macro plant is one
  // bulk proxy body and a promoted plant's body belongs to its view instead.
  expect(Number(status.collision!.residentBodies) + promoted).toBe(macro);

  // Navigation: every plant declares, and a promoted one declares dynamic rather than static.
  const declaring = new Set(nav.cells.flatMap((cell) => cell.contributions).map((row) => row.plant));
  expect(declaring.size).toBe(macro);
  const dynamic = new Set(
    nav.cells
      .flatMap((cell) => cell.contributions)
      .filter((row) => row.kind === "dynamic-obstacle")
      .map((row) => row.plant),
  );
  expect(dynamic.size).toBe(promoted);
  return { promoted, macro };
}

// Every navigation contribution the seam publishes, flattened across cells.
async function navContributions(): Promise<VegetationNavigationContributionDto[]> {
  const nav = await engine.call("vegetation-nav-contributions", {});
  return nav.cells.flatMap((cell) => cell.contributions);
}

function overlaps(left: WorldBoundsDto, right: WorldBoundsDto): boolean {
  return [0, 1, 2].every(
    (axis) =>
      BigInt(left.minTicks[axis]!) < BigInt(right.maxTicksExclusive[axis]!) &&
      BigInt(right.minTicks[axis]!) < BigInt(left.maxTicksExclusive[axis]!),
  );
}

// Moves every viewpoint the shared residency source follows to `z`. In play the source follows the
// scene's primary camera when there is one and the editor fly camera otherwise, so both move.
async function moveViewpoint(z: number) {
  await engine.call("set-camera", { position: { x: 32, y: 8, z }, yaw: 0, pitch: -10 });
  const entities = await engine.call("list-entities", {});
  for (const entity of entities.entities) {
    const inspected = await engine.call("inspect", { entity: entity.id });
    if (!inspected.componentOrder.includes("Camera")) {
      continue;
    }
    await engine.call("set-transform", {
      entity: entity.id,
      translation: { x: 32, y: 8, z },
    });
  }
}

async function projectRoot(): Promise<string> {
  const status = await engine.call("project-status");
  return status.path.endsWith("project.json") ? dirname(status.path) : status.path;
}

test("a resident cell drives collision, promotion, felling, events, and navigation", async () => {
  await importVegetationPackage(engine, cleaner, fixture, "interaction");
  world = (await bindVegetationField(engine, cleaner, fixture, "Interaction vegetation")).id;

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
      const hits = await queryPlants(engine, BOUNDS);
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

  // Physics casts and vegetation queries are separate surfaces, and only the first one is allowed
  // to answer `sa.raycast`. The cell carries cosmetic micro grass; a blade has no body, so a
  // physics ray can only ever report one of the cell's collidable macro plants — a column through
  // a plant resolves to that plant, and every other column resolves to no vegetation at all.
  let macro = 0;
  {
    const cell = await engine.call("vegetation-runtime-cell", { cell: CELL });
    macro = Number(cell.macroPlants);
    expect(Number(cell.microTiles)).toBeGreaterThan(0);
    expect(macro).toBeGreaterThan(0);
    // Every resident body came from a macro plant's single trunk capsule, so the batch counts
    // plants and not blades.
    expect(bodiesBefore).toBe(macro);

    const published = (await queryPlants(engine, BOUNDS, 4096)).hits.map((hit) => ({
      plant: hit.plant.plant,
      x: Number(hit.plant.positionTicks[0]) / TICKS_PER_METER,
      z: Number(hit.plant.positionTicks[2]) / TICKS_PER_METER,
    }));
    expect(published.length).toBe(macro);

    const castDown = (x: number, z: number) =>
      engine.call("raycast", {
        origin: { x, y: 64, z },
        dir: { x: 0, y: -1, z: 0 },
        maxDist: 128,
      });

    // A column straight through a macro plant resolves to that plant, which is what proves the
    // physics surface reports vegetation at all — without it the empty columns below say nothing.
    for (const { plant: id, x, z } of published) {
      const cast = await castDown(x, z);
      expect(cast.target).toEqual({ kind: "vegetation", plant: id });
    }

    // Everywhere else the cell is grass, and grass has no body. Any hit off a plant's column is a
    // proxy that should never have been registered.
    let grassColumns = 0;
    for (let x = 1; x < 64; x += 3) {
      for (let z = 1; z < 64; z += 3) {
        if (published.some((row) => Math.hypot(row.x - x, row.z - z) < 4)) {
          continue;
        }
        grassColumns += 1;
        const cast = await castDown(x, z);
        expect(cast.target?.kind).not.toBe("vegetation");
      }
    }
    expect(grassColumns).toBeGreaterThan(0);
  }

  // Telemetry: compact counters only, and the synchronization it timed actually ran.
  {
    const before = await engine.call("vegetation-telemetry");
    expect(Number(before.work.synchronizations)).toBeGreaterThan(0);
    // The residency queries above were counted, hits and traversal work alike. The traversal
    // terms are what the query actually cost, and they track macro rows: never blade count.
    expect(Number(before.work.queries)).toBeGreaterThan(0);
    expect(Number(before.work.queryHits)).toBeGreaterThan(0);
    expect(Number(before.work.queryGenerationsVisited)).toBeGreaterThan(0);
    expect(Number(before.work.queryNodesVisited)).toBeGreaterThan(0);
    expect(Number(before.work.queryRowsTested)).toBeGreaterThan(0);
    expect(Number(before.collisionBodies)).toBe(bodiesBefore);
    await engine.settle(60);
    const after = await engine.call("vegetation-telemetry");
    expect(Number(after.work.synchronizations)).toBeGreaterThan(
      Number(before.work.synchronizations),
    );
    // A synchronization that ran took some time, and the average is in the same ballpark.
    expect(after.last.totalUs).toBeGreaterThanOrEqual(0);
    expect(after.average.totalUs).toBeGreaterThanOrEqual(0);
  }

  // Navigation publishes obstacle contributions for the same cell.
  {
    const nav = await engine.call("vegetation-nav-contributions", {});
    expect(nav.cells.length).toBeGreaterThan(0);
    expect(Number(nav.contributions)).toBeGreaterThan(0);
    // The fixture's family is Structural, so every contribution is an obstacle.
    expect(Number(nav.obstacles)).toBe(Number(nav.contributions));
    expect(Number(nav.dynamicObstacles)).toBe(0);
    // Nothing is promoted yet, so every system owns every plant exactly once through the bulk
    // representation. This is the baseline the promotion below must not disturb the arithmetic of.
    await assertOneOwnerPerPlant(macro);
  }

  // Promotion: the request commits at the next synchronization point, and from then on the
  // entity view is the plant's only owner — its bulk render instance, collision batch, and static
  // navigation declaration are all retired for exactly that plant.
  {
    // Take ownership of whatever the bring-up dirtied, so what the promotion dirties stands alone.
    await engine.call("vegetation-nav-contributions", { drainDirty: true });
    const others = (await navContributions()).filter((row) => row.plant !== plant);
    expect(others.length).toBe(macro - 1);

    const requested = await engine.call("vegetation-promote", { plant });
    expect(requested.state.state).toBe("promoting");

    await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) => inspect.promotion?.state === "promoted",
      "the promotion to commit",
    );

    const status = await available();
    expect(Number(status.promotion!.promoted)).toBe(1);
    // One owner across render, collision, navigation, and simulation — for every plant in the
    // cell, not only the promoted one.
    await assertOneOwnerPerPlant(macro);
    expect(Number(status.collision!.residentBodies)).toBe(bodiesBefore - 1);

    // The promoted plant's own contribution turned dynamic; every other plant kept the exact
    // declaration it already published.
    const after = await navContributions();
    expect(after.filter((row) => row.plant === plant).map((row) => row.kind)).toEqual([
      "dynamic-obstacle",
    ]);
    expect(after.filter((row) => row.plant !== plant)).toEqual(others);

    // Promoting one plant hands back one plant's ground. A whole-cell re-publication would come
    // back as a region covering every other plant's contribution too.
    const drained = await engine.call("vegetation-nav-contributions", { drainDirty: true });
    expect(drained.drained).toBe(true);
    expect(drained.dirtyRegions.length).toBeGreaterThan(0);
    for (const region of drained.dirtyRegions) {
      for (const row of others) {
        expect(overlaps(region, row.bounds)).toBe(false);
      }
    }
  }

  // A save barrier taken while the plant is promoted: the flush reduces the live view's state
  // through the reducer without ending the promotion, and the view stays the only owner across it.
  {
    const baseline = await engine.call("vegetation-state-baseline", {});
    expect(Number(baseline.cells)).toBeGreaterThan(0);
    const inspect = await engine.call("vegetation-runtime-inspect", { plant });
    expect(inspect.promotion?.state).toBe("promoted");
    expect(inspect.persistent.some((entry) => entry.promotionOrigin !== undefined)).toBe(true);
    await assertOneOwnerPerPlant(macro);
  }

  // The cell republishes under the live view: a confirmed mutation rebuilds every generation the
  // cell carries, exactly as a recook committing new artifacts does. Suppression lives on the
  // world keyed by plant identity rather than inside the published generation, so the new
  // generation comes up already missing the promoted plant's bulk representation.
  {
    const other = (await queryPlants(engine, BOUNDS, 4096)).hits
      .map((hit) => hit.plant.plant)
      .find((id) => id !== plant)!;
    const before = await engine.call("vegetation-runtime-cell", { cell: CELL });
    await engine.call("vegetation-mutate", {
      gesture: "e2e00000000000000000000000cc0001",
      records: [
        {
          header: {
            cell: CELL,
            transaction: "e2e00000000000000000000000cc0001",
            authority: "e2e00000000000000000000000990001",
            logicalTick: "3",
            idempotencyKey: "e2e00000000000000000000000dd0001",
          },
          mutation: { kind: "damage", plant: other, amount: 4000, phenotype: null },
        } satisfies VegetationMutationRecordDto,
      ],
    });
    await settleUntil(
      () => engine.call("vegetation-runtime-cell", { cell: CELL }),
      (cell) => cell.generation !== before.generation,
      "the cell to republish under the promotion",
    );
    await settleUntil(
      async () => (await available()).collision,
      (collision) => Number(collision?.residentBodies ?? 0) === bodiesBefore - 1,
      "the republished cell to come back without the promoted plant's bulk body",
    );
    const inspect = await engine.call("vegetation-runtime-inspect", { plant });
    expect(inspect.promotion?.state).toBe("promoted");
    await assertOneOwnerPerPlant(macro);
  }

  // The cell leaves and re-enters residency underneath a live view. Suppression is keyed by plant
  // identity rather than held in the published generation, so the reloaded cell comes back without
  // the promoted plant's bulk representation — and no other plant loses one.
  {
    await moveViewpoint(4_000);
    await settleUntil(
      async () => (await available()).collision,
      (collision) => Number(collision?.residentBodies ?? 0) === 0,
      "the cell to leave collision residency under a promoted plant",
    );
    // The entity view outlives the cell that published the row it stands for.
    const away = await engine.call("vegetation-runtime-inspect", { plant });
    expect(away.promotion?.state).toBe("promoted");

    await moveViewpoint(44);
    await settleUntil(
      async () => (await available()).collision,
      (collision) => Number(collision?.residentBodies ?? 0) === bodiesBefore - 1,
      "the cell to return without the promoted plant's bulk body",
    );
    await assertOneOwnerPerPlant(macro);
  }

  // Demotion writes the view's state back through the reducer, so the plant carries persistent
  // promotion-origin state afterwards, and its bulk collision returns.
  {
    const requested = await engine.call("vegetation-demote", { plant });
    expect(requested.state.state).toBe("demoting");

    const deadline = Date.now() + 20_000;
    for (;;) {
      const inspect = await engine.call("vegetation-runtime-inspect", { plant });
      // The write-back records promotion-origin state in the cell the view settled in, so the
      // plant carries it on one of its persistent entries.
      if (
        inspect.promotion?.state === "bulk" &&
        inspect.persistent.some((entry) => entry.promotionOrigin !== undefined)
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
    await assertOneOwnerPerPlant(macro);
  }

  // The write-back carries momentum, not only a pose: what the standing view was moving at is what
  // the origin state records, in the reducer's units.
  {
    await engine.call("vegetation-promote", { plant });
    await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) => inspect.promotion?.state === "promoted",
      "the momentum promotion to commit",
    );
    const entities = await engine.call("list-entities", {});
    const view = entities.entities.find((entity) => entity.name.startsWith("Plant "))!;
    const pushed = await engine.call("apply-impulse", {
      entity: view.id,
      impulse: { x: 0, y: 0, z: 5 },
    });
    expect(pushed.velocity.z).toBeGreaterThan(0);

    await engine.call("vegetation-demote", { plant });
    const settled = await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) =>
        inspect.promotion?.state === "bulk" &&
        inspect.persistent.some((entry) => entry.promotionOrigin !== undefined),
      "the momentum write-back",
    );
    // Q15.16 metres per fixed physics tick. A write-back that recorded only a pose leaves the axis
    // the push was on at zero; this one read the live body, so the push is in the record.
    const origin = settled.persistent
      .map((entry) => entry.promotionOrigin)
      .find((entry) => entry !== undefined)!;
    expect(origin.linearVelocityBits[2]).toBeGreaterThan(0);
    await assertOneOwnerPerPlant(macro);
  }

  // The view is where a promoted plant's biology lives: `vegetation-plant-vitals` reads and writes
  // it, and the demotion returns what it settled at. What nobody touched is not written back —
  // repeating the snapshot the promotion copied out would overwrite the row's own progress.
  {
    const before = await engine.call("vegetation-runtime-inspect", { plant });
    const rowHealth = before.resident!.health;

    await engine.call("vegetation-promote", { plant });
    await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) => inspect.promotion?.state === "promoted",
      "the vitals promotion to commit",
    );

    // The view was stamped with the row's biology.
    const stamped = await engine.call("vegetation-plant-vitals", { plant });
    expect(stamped.health).toBe(rowHealth);
    expect(stamped.lifecycle).toBe(before.resident!.lifecycle);

    // Gameplay damages the standing view. `health` is the reducer's unit-interval bits.
    const damaged = Math.max(0, rowHealth - 12_000);
    const written = await engine.call("vegetation-plant-vitals", { plant, health: damaged });
    expect(written.health).toBe(damaged);
    expect(await engine.call("vegetation-plant-vitals", { plant })).toEqual(written);

    await engine.call("vegetation-demote", { plant });
    // The write-back records against the cell the view settled in, which is not always the plant's
    // base cell — a promoted view falls under physics — so the delta list is what carries it.
    const settled = await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) =>
        inspect.promotion?.state === "bulk" &&
        inspect.persistent.some((entry) => entry.health === damaged),
      "the vitals write-back",
    );
    // Only what the view changed travelled: no moisture and no fuel override was recorded at all,
    // so the row keeps whatever it accrued underneath the standing view.
    for (const entry of settled.persistent) {
      expect(entry.moisture ?? null).toBeNull();
      expect(entry.fuel ?? null).toBeNull();
    }
    await assertOneOwnerPerPlant(macro);
  }

  // Another authority claiming the plant takes the view away without a write-back. This is what an
  // editor undo, a recook's delta restore, and a network correction all look like from the play
  // session's side: the plant stops being its to simulate, so the entity goes and the bulk
  // representation comes back in the same pass — never two owners and never none.
  {
    const releasedBefore = Number((await available()).promotion!.releasedTotal);
    await engine.call("vegetation-promote", { plant });
    const promoted = await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) => inspect.promotion?.state === "promoted",
      "the handover promotion to commit",
    );
    await assertOneOwnerPerPlant(macro);

    // A transform override committed by a different authority is a positional claim: it says where
    // the plant is, which is exactly what the standing view was answering.
    const resident = promoted.resident!;
    await engine.call("vegetation-mutate", {
      gesture: "e2e00000000000000000000000ee0001",
      records: [
        {
          header: {
            cell: resident.cell,
            transaction: "e2e00000000000000000000000ee0002",
            authority: "e2e00000000000000000000000ee0003",
            logicalTick: "9",
            idempotencyKey: "e2e00000000000000000000000ee0004",
          },
          mutation: {
            kind: "transform-override",
            plant,
            transform: {
              globalTicks: resident.positionTicks,
              orientation: resident.orientation,
              scaleBits: resident.scaleBits,
            },
          },
        } satisfies VegetationMutationRecordDto,
      ],
    });

    const released = await settleUntil(
      () => engine.call("vegetation-runtime-inspect", { plant }),
      (inspect) => inspect.promotion?.state === "bulk",
      "the view to yield to the claiming authority",
    );
    expect(released.promotion?.state).toBe("bulk");
    const status = await available();
    expect(Number(status.promotion!.releasedTotal)).toBe(releasedBefore + 1);
    expect(Number(status.promotion!.promoted)).toBe(0);
    await assertOneOwnerPerPlant(macro);
  }

  // Events: one typed transition per committed record, and an exact replay emits none.
  {
    const cursor = await engine.call("vegetation-drain-events", {});
    const since = cursor.highWaterSeq;

    // Transaction, authority, and operation keys are canonical 32-digit hexadecimal GUIDs.
    const record: VegetationMutationRecordDto = {
      header: {
        cell: CELL,
        transaction: "e2e00000000000000000000000770001",
        authority: "e2e00000000000000000000000990001",
        logicalTick: "1",
        idempotencyKey: "e2e00000000000000000000000880001",
      },
      mutation: { kind: "damage", plant, amount: 8000, phenotype: null },
    };
    const first = await engine.call("vegetation-mutate", {
      gesture: "e2e00000000000000000000000dd0001",
      records: [record],
    });
    expect(first.applied).toBe(1);
    const afterFirst = await engine.call("vegetation-drain-events", {
      since,
    });
    const damaged = afterFirst.events.filter((event) => event.transition.kind === "damaged");
    expect(damaged.length).toBe(1);
    expect(damaged[0]!.plant).toBe(plant);

    // The identical record is an idempotent replay: accepted, but not observable twice.
    await engine.call("vegetation-mutate", {
      gesture: "e2e00000000000000000000000dd0002",
      records: [record],
    });
    const afterReplay = await engine.call("vegetation-drain-events", {
      since,
    });
    expect(afterReplay.events.filter((event) => event.transition.kind === "damaged").length).toBe(
      1,
    );
  }

  // Felling separates the product from the rooted plant: the plant becomes a stump under its own
  // identity, and a separate product entity appears.
  {
    const before = await engine.call("list-entities", {});
    await engine.call("vegetation-fell", { plant });

    const deadline = Date.now() + 20_000;
    for (;;) {
      const inspect = await engine.call("vegetation-runtime-inspect", { plant });
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
    const after = await engine.call("list-entities", {});
    const products = after.entities.filter((entity) => entity.name.startsWith("Felled "));
    expect(products.length).toBe(1);
    expect(after.entities.length).toBeGreaterThan(before.entities.length);
    // The rooted plant kept its identity; the product is a different entity entirely.
    const felled = await engine.call("vegetation-runtime-inspect", { plant });
    expect(felled.plant).toBe(plant);
  }

  await engine.call("stop");
  await engine.settle(100);
});

// The derived cache is disposable and the persistent state is not. Deleting `cache/vegetation/`
// and cooking the world again rebuilds the same base artifacts — and the plants the simulation
// removed stay removed, because the delta that removed them lives under `state/vegetation/` and is
// re-applied on top of whatever the cook produced.
test("a reload preserves written-back state and a cache recook resurrects nothing", async () => {
  const root = await projectRoot();
  const cache = join(root, "cache", "vegetation");
  expect(existsSync(cache)).toBe(true);

  const resident = (await queryPlants(engine, BOUNDS, 4096)).hits.map((hit) => hit.plant.plant);
  expect(resident.length).toBeGreaterThan(1);
  // The plant the first test demoted carries the write-back; a different one is removed outright,
  // so the reload has both a value to preserve and an absence to preserve.
  let demoted = "";
  for (const candidate of resident) {
    const inspect = await engine.call("vegetation-runtime-inspect", { plant: candidate });
    if (inspect.persistent.some((entry) => entry.promotionOrigin !== undefined)) {
      demoted = candidate;
      break;
    }
  }
  expect(demoted).not.toBe("");
  const removed = resident.find((id) => id !== demoted)!;
  removedPlant = removed;
  const kept = await engine.call("vegetation-runtime-inspect", { plant: demoted });
  const keptHealth = kept.resident!.health;

  await engine.call("vegetation-mutate", {
    gesture: "e2e00000000000000000000000aa0001",
    records: [
      {
        header: {
          cell: CELL,
          transaction: "e2e00000000000000000000000aa0001",
          authority: "e2e00000000000000000000000990001",
          logicalTick: "2",
          idempotencyKey: "e2e00000000000000000000000bb0001",
        },
        mutation: { kind: "tombstone", plant: removed },
      } satisfies VegetationMutationRecordDto,
    ],
  });
  await settleUntil(
    () => queryPlants(engine, BOUNDS, 4096),
    (hits) => !hits.hits.some((hit) => hit.plant.plant === removed),
    "the removed plant to leave the resident rows",
  );

  // The save barrier: persistent state becomes the generation's starting state, in the durable
  // root beside the cache rather than inside it.
  const baseline = await engine.call("vegetation-state-baseline", {});
  expect(Number(baseline.cells)).toBeGreaterThan(0);
  expect(existsSync(join(root, "state", "vegetation"))).toBe(true);

  // Rebind the world so the persistent state is re-read from the durable root rather than carried
  // in memory: disabling the field drops the authority outright, and re-enabling it builds a fresh
  // one that imports the baseline.
  const rebind = async (enabled: boolean) => {
    await engine.call("set-component", {
      entity: world,
      component: "VegetationField",
      json: { map: fixture.map, enabled },
    });
    await settleUntil(
      () => engine.call("vegetation-runtime-status"),
      (status) => (enabled ? status.state === "available" : status.state !== "available"),
      `the runtime to ${enabled ? "bind" : "unbind"}`,
      60_000,
    );
  };
  await rebind(false);
  await rebind(true);

  const rebound = await engine.call("vegetation-runtime-status");
  if (rebound.state !== "available") {
    throw new Error(`vegetation runtime is ${rebound.state}`);
  }
  // The same world came back, which is what lets the baseline bind onto it — a generation that
  // returned under a different identity would silently lose every persistent delta.
  expect(rebound.manifestIdentity).toBe(baseline.manifestIdentity);

  const reloaded = await settleUntil(
    () => queryPlants(engine, BOUNDS, 4096),
    (hits) => hits.hits.length > 0,
    "the reloaded cell to become resident",
    60_000,
  );
  // The base artifact still carries the removed plant — it always did — and the persistent
  // tombstone still takes it out. A reload that read the cook and forgot the delta shows it here.
  expect(reloaded.hits.some((hit) => hit.plant.plant === removed)).toBe(false);
  const gone = await engine.call("vegetation-runtime-inspect", { plant: removed });
  expect(gone.resident ?? null).toBeNull();
  expect(gone.persistent.some((entry) => entry.tombstoned)).toBe(true);

  // And the state the demotion wrote back survived the same round trip.
  const preserved = await engine.call("vegetation-runtime-inspect", { plant: demoted });
  expect(preserved.resident!.health).toBe(keptHealth);
  expect(preserved.persistent.some((entry) => entry.promotionOrigin !== undefined)).toBe(true);

  // Now delete the derived cell artifacts and cook them again from the authored sources. The cook
  // reads nothing but those sources, so the artifact it rebuilds is the one that carried the
  // removed plant all along — it never learned that the simulation took the plant out.
  rmSync(join(cache, "cells"), { recursive: true, force: true });
  expect(existsSync(join(cache, "cells"))).toBe(false);
  await cookCells(engine, fixture.map);
  const baseAfter = await engine.call("vegetation-cell-inspect", { map: fixture.map, cell: CELL });
  // The regenerated base is the thing that would resurrect the plant if the delta stopped
  // layering: it still counts every plant the first cook placed, the removed one included.
  expect(Number(baseAfter.cell.macroPoints)).toBe(resident.length);

  // Bind onto the freshly cooked artifacts and read the world again.
  await rebind(false);
  await rebind(true);
  const recooked = await settleUntil(
    () => queryPlants(engine, BOUNDS, 4096),
    (hits) => hits.hits.length > 0,
    "the recooked cell to become resident",
    60_000,
  );
  expect(recooked.hits.some((hit) => hit.plant.plant === removed)).toBe(false);
  expect(recooked.hits.length).toBe(resident.length - 1);
  const stillGone = await engine.call("vegetation-runtime-inspect", { plant: removed });
  expect(stillGone.resident ?? null).toBeNull();
  expect(stillGone.persistent.some((entry) => entry.tombstoned)).toBe(true);
  // The write-back rode across the recook on the same durable state.
  const stillWritten = await engine.call("vegetation-runtime-inspect", { plant: demoted });
  expect(stillWritten.resident!.health).toBe(keptHealth);
  expect(stillWritten.persistent.some((entry) => entry.promotionOrigin !== undefined)).toBe(true);

  expect(engine.validationErrors()).toEqual([]);
});

// How many owners each system holds right now, without assuming what the total should be: the
// world already carries a tombstone, so the counts are only meaningful against each other.
async function ownerCounts() {
  const status = await available();
  const render = await engine.call("vegetation-render-stats", {});
  const nav = await engine.call("vegetation-nav-contributions", {});
  const entities = await engine.call("list-entities", {});
  return {
    promoted: Number(status.promotion!.promoted),
    views: entities.entities.filter((entity) => entity.name.startsWith("Plant ")).length,
    bulkInstances: render.cells.reduce((total, row) => total + Number(row.plants), 0),
    bodies: Number(status.collision!.residentBodies),
    declaring: new Set(nav.cells.flatMap((cell) => cell.contributions).map((row) => row.plant)).size,
  };
}

// A cook commits a generation under a new identity while a plant stands promoted. The views
// describe rows of a base that is gone, so the promotion authority abandons them: no entity, no
// body, no suppression, and deliberately no write-back — recording a view's state against a world
// nobody observed is worse than losing the view.
test("a generation change under a promotion abandons the view without a write-back", async () => {
  await engine.call("play");
  await engine.settle(200);

  const resident = (
    await settleUntil(
      () => queryPlants(engine, BOUNDS, 4096),
      (hits) => hits.hits.length > 0,
      "the cell to become resident under play",
      60_000,
    )
  ).hits.map((hit) => hit.plant.plant);
  const plant = resident[0]!;
  const recorded = (await engine.call("vegetation-runtime-inspect", { plant })).persistent;

  const bulk = await settleUntil(
    ownerCounts,
    (counts) => counts.bodies > 0 && counts.bulkInstances > 0,
    "the cell to claim collision and render residency under play",
  );
  expect(bulk.promoted).toBe(0);
  expect(bulk.views).toBe(0);

  await engine.call("vegetation-promote", { plant });
  await settleUntil(
    () => engine.call("vegetation-runtime-inspect", { plant }),
    (inspect) => inspect.promotion?.state === "promoted",
    "the promotion to commit",
  );
  const promoted = await settleUntil(
    ownerCounts,
    (counts) => counts.bodies === bulk.bodies - 1,
    "the promoted plant's bulk body to retire",
  );
  expect(promoted).toEqual({
    ...bulk,
    promoted: 1,
    views: 1,
    bulkInstances: bulk.bulkInstances - 1,
    bodies: bulk.bodies - 1,
  });

  // Gameplay damages the standing view, so there is state a write-back would carry — which is what
  // makes its absence below an assertion rather than a truism.
  const live = await engine.call("vegetation-plant-vitals", { plant });
  const damaged = Math.max(0, live.health - 9_000);
  expect(damaged).not.toBe(live.health);
  expect((await engine.call("vegetation-plant-vitals", { plant, health: damaged })).health).toBe(
    damaged,
  );

  // Cooking the same authored world under a different content profile commits a generation with a
  // different identity and the same placement — a recook, with the world it produces held still.
  const bound = (await available()).manifestIdentity;
  const cook = await engine.call("vegetation-cook", {
    map: fixture.map,
    scope: { kind: "cells", cells: [CELL] },
    workers: 1,
    platformProfile: "portable-vulkan-profiled",
  });
  await awaitCook(engine, cook.job);
  await settleUntil(
    () => engine.call("vegetation-runtime-status"),
    (status) => status.state === "available" && status.manifestIdentity !== bound,
    "the new generation to bind",
    60_000,
  );

  const abandoned = await settleUntil(
    ownerCounts,
    (counts) => counts.bodies === bulk.bodies,
    "every plant to return to exactly one bulk owner",
  );
  expect(abandoned).toEqual(bulk);
  const after = await engine.call("vegetation-runtime-inspect", { plant });
  expect(after.promotion?.state ?? "bulk").toBe("bulk");
  // Nothing the view held was recorded: the deltas the plant carries are the ones it already had,
  // and the damage the view took is in none of them.
  expect(after.persistent).toEqual(recorded);
  expect(after.persistent.some((entry) => entry.health === damaged)).toBe(false);
  // The generation changed, and the delta the earlier tests recorded crossed it with the world.
  const carried = await engine.call("vegetation-runtime-inspect", { plant: removedPlant });
  expect(carried.persistent.some((entry) => entry.tombstoned)).toBe(true);
  expect(
    (await queryPlants(engine, BOUNDS, 4096)).hits.some(
      (hit) => hit.plant.plant === removedPlant,
    ),
  ).toBe(false);

  await engine.call("stop");
  await engine.settle(100);
  expect(engine.validationErrors()).toEqual([]);
});
