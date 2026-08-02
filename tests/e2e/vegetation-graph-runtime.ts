// The live-world half of the vegetation acceptance run: viewport picking across the macro and
// micro vocabularies, phenology resolution, rebuild-from-artifacts stability, and the typed
// mutations that remove and plant a macro plant.

import { expect } from "bun:test";
import type { EntityRef, PickResult, PlantId } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import {
  BOUNDS,
  CELL,
  TICKS_PER_METER,
  type VegetationFixture,
  awaitResidentCell,
  queryPlants,
} from "./vegetation-utils.ts";

// Aimed straight at a trunk from inside the cooked cell.
const TRUNK_VIEW = { position: { x: 16, y: 4, z: 30 }, yaw: 0, pitch: 0 } as const;

// A pick the assertions have already proven carries a stable plant identity.
export interface PickedPlant extends PickResult {
  plant: PlantId;
}

interface TrunkRect {
  minX: number;
  maxX: number;
  minZ: number;
  maxZ: number;
}

// The one viewport pick merges the vegetation vocabulary: aimed at a trunk, the GPU selection-ID
// readback answers with the macro plant, returned by stable identity — the readback names a
// GPU-scene slot and the mirror translates it back, so a slot never reaches the wire.
export async function pickMacroPlant(engine: Engine): Promise<PickedPlant> {
  await engine.call("set-camera", TRUNK_VIEW);
  await engine.settle(200);
  const picked = await engine.call("pick", { u: 0.5, v: 0.5 });
  expect(picked.hit).toBe(true);
  expect(picked.kind).toBe("vegetation");
  expect(picked.plant).toMatch(/^[0-9a-f]{32}$/);
  return { ...picked, plant: picked.plant! };
}

// The runtime query supplies the trunk bounds so the micro scan only samples spots whose ray
// misses every plant.
export async function readTrunkRects(engine: Engine): Promise<TrunkRect[]> {
  const runtimePlants = await queryPlants(engine, BOUNDS, 256);
  expect(runtimePlants.hits.length).toBeGreaterThan(0);
  return runtimePlants.hits.map(({ plant }) => ({
    minX: Number(plant.bounds.minTicks[0]) / TICKS_PER_METER,
    maxX: Number(plant.bounds.maxTicksExclusive[0]) / TICKS_PER_METER,
    minZ: Number(plant.bounds.minTicks[2]) / TICKS_PER_METER,
    maxZ: Number(plant.bounds.maxTicksExclusive[2]) / TICKS_PER_METER,
  }));
}

// Phenology: the rendered phenotype resolves from typed lifecycle + season. The canonical family
// has no seasonal phenotype, so the resolution is the cooked identity in June and stays so after
// an autumn date scrub, which must also keep frames validation-clean through the season-keyed
// cell rebuild.
export async function assertPhenologyHoldsAcrossSeasons(engine: Engine): Promise<void> {
  const query = async () => queryPlants(engine, BOUNDS, 8);
  for (const hit of (await query()).hits) {
    expect(hit.plant.renderedPhenotype).toBe(hit.plant.phenotype);
  }
  await engine.call("set-time-of-day", { json: { month: 10, day: 15 } });
  await engine.settle(200);
  for (const hit of (await query()).hits) {
    expect(hit.plant.renderedPhenotype).toBe(hit.plant.phenotype);
  }
  await engine.call("set-time-of-day", { json: { month: 6, day: 15 } });
}

// Micro density follows the community blend, so the dense texels ring the trunks: sample just
// outside each trunk's bounds first, then a coarse grid.
function microSampleSpots(trunkRects: TrunkRect[]): { x: number; z: number }[] {
  const spots: { x: number; z: number }[] = [];
  for (const rect of trunkRects) {
    const centerX = (rect.minX + rect.maxX) / 2;
    const centerZ = (rect.minZ + rect.maxZ) / 2;
    const ring = Math.max(rect.maxX - rect.minX, rect.maxZ - rect.minZ) / 2 + 1.0;
    for (const [dx, dz] of [
      [1, 0],
      [-1, 0],
      [0, 1],
      [0, -1],
      [1, 1],
      [-1, 1],
      [1, -1],
      [-1, -1],
    ]) {
      spots.push({ x: centerX + dx * ring, z: centerZ + dz * ring });
    }
  }
  for (let x = 2; x < 64; x += 4) {
    for (let z = 2; z < 64; z += 4) {
      spots.push({ x, z });
    }
  }
  return spots;
}

// Aimed straight down at vegetated ground away from every macro trunk, the pick answers with
// whatever the frame drew at that pixel. Micro blades are cosmetic: a blade that covers the pixel
// answers as a nonpersistent point with no identity, and bare ground between blades answers
// nothing at all. What must never happen there is a macro plant identity — the paint tools treat
// a returned PlantId as a selected, saved object. `vegetation-micro-pick.test.ts` carries the
// other half of that vocabulary, over a field dense enough for a ray to land on a blade.
export async function pickGroundClearOfTrunks(
  engine: Engine,
  trunkRects: TrunkRect[],
): Promise<void> {
  const clearOfTrunks = (x: number, z: number) =>
    trunkRects.every(
      (rect) =>
        x < rect.minX - 0.5 || x > rect.maxX + 0.5 || z < rect.minZ - 0.5 || z > rect.maxZ + 0.5,
    );
  let attempts = 0;
  for (const spot of microSampleSpots(trunkRects)) {
    if (attempts >= 24) {
      break;
    }
    if (
      spot.x < 1 ||
      spot.x >= 63 ||
      spot.z < 1 ||
      spot.z >= 63 ||
      !clearOfTrunks(spot.x, spot.z)
    ) {
      continue;
    }
    attempts += 1;
    await engine.call("set-camera", {
      position: { x: spot.x, y: 12, z: spot.z },
      yaw: 0,
      pitch: -89,
    });
    await engine.settle(100);
    const picked = await engine.call("pick", { u: 0.5, v: 0.5 });
    expect(picked.kind === "vegetation").toBe(false);
    expect(picked.plant).toBeUndefined();
    if (picked.kind !== "micro-vegetation") {
      continue;
    }
    const microPoint = picked.position ?? [];
    expect(microPoint).toHaveLength(3);
    expect(microPoint[0]).toBeGreaterThanOrEqual(0);
    expect(microPoint[0]).toBeLessThan(64);
    expect(microPoint[2]).toBeGreaterThanOrEqual(0);
    expect(microPoint[2]).toBeLessThan(64);
  }
  expect(attempts).toBeGreaterThan(0);
}

// Reload stability: disable and re-enable the field, forcing the runtime world to rebuild from
// the cooked artifacts. Macro selection returns the identical stable PlantId, the inspect
// resolves it, and micro blades reconstruct.
export async function reloadField(
  engine: Engine,
  fixture: VegetationFixture,
  world: EntityRef,
  expectedPlants: number,
  picked: PickedPlant,
): Promise<PickedPlant> {
  for (const enabled of [false, true]) {
    await engine.call("set-component", {
      entity: world.id,
      component: "VegetationField",
      json: { map: fixture.map, enabled },
    });
    if (!enabled) {
      await engine.settle(300);
    }
  }
  await awaitResidentCell(engine, expectedPlants, { microTiles: true });

  await engine.call("set-camera", TRUNK_VIEW);
  await engine.settle(200);
  const hit = await engine.call("pick", { u: 0.5, v: 0.5 });
  expect(hit.kind).toBe("vegetation");
  expect(hit.plant).toBe(picked.plant);
  const repicked: PickedPlant = { ...hit, plant: hit.plant! };
  const reinspected = await engine.call("vegetation-runtime-inspect", {
    plant: repicked.plant,
  });
  expect(reinspected.plant).toBe(repicked.plant);
  expect(reinspected.resident).toBeTruthy();
  return repicked;
}

// Waits for the cell's macro population to reach `expected`, or fails with `what`.
async function awaitMacroCount(engine: Engine, expected: number, what: string): Promise<void> {
  const deadline = Date.now() + 15_000;
  for (;;) {
    const after = await engine.call("vegetation-runtime-cell", {
      cell: CELL,
    });
    if (Number(after.macroPlants) === expected) {
      return;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for ${what}`);
    }
    await engine.settle(50);
  }
}

// Editing a generated plant writes a typed mutation through the reducer: the tombstone removes
// the picked plant persistently — the runtime row disappears and the same viewport ray no longer
// returns its identity. Returns the cell's macro population from before the removal.
export async function tombstonePickedPlant(engine: Engine, picked: PickedPlant): Promise<number> {
  const before = await engine.call("vegetation-runtime-cell", {
    cell: CELL,
  });
  const mutate = await engine.call("vegetation-mutate", {
    gesture: "e2e00000000000000000000000c70001",
    records: [
      {
        header: {
          cell: CELL,
          transaction: "1".padStart(32, "0"),
          authority: "e".padStart(32, "0"),
          logicalTick: "1",
          idempotencyKey: "a".padStart(32, "0"),
        },
        mutation: { kind: "tombstone", plant: picked.plant },
      },
    ],
  });
  expect(mutate.applied).toBe(1);
  const baseline = Number(before.macroPlants);
  await awaitMacroCount(engine, baseline - 1, "the tombstoned plant to leave the cell");

  const tombstoned = await engine.call("vegetation-runtime-inspect", {
    plant: picked.plant,
  });
  expect(tombstoned.resident ?? null).toBeNull();
  await engine.settle(200);
  const postPick = await engine.call("pick", { u: 0.5, v: 0.5 });
  expect(postPick.plant === picked.plant).toBe(false);
  return baseline;
}

// Anchor planting (the editor Single/Anchor tool's exact payload): an explicit-namespace
// identity anchors at a ground position and becomes a resident macro plant.
export async function plantAnchor(
  engine: Engine,
  fixture: VegetationFixture,
  baselinePlants: number,
): Promise<void> {
  const anchorId = `4${"c".repeat(31)}`;
  const ticks = (meters: number) => String(meters * TICKS_PER_METER);
  const anchor = await engine.call("vegetation-mutate", {
    gesture: "e2e00000000000000000000000c70002",
    records: [
      {
        header: {
          cell: CELL,
          transaction: "2".padStart(32, "0"),
          authority: "e".padStart(32, "0"),
          logicalTick: "2",
          idempotencyKey: "b".padStart(32, "0"),
        },
        mutation: {
          kind: "anchor-addition",
          point: {
            id: anchorId,
            owner: CELL,
            localPosition: [40 * TICKS_PER_METER, 0, 40 * TICKS_PER_METER],
            orientation: [0, 0, 0, 32767],
            scaleBits: [65536, 65536, 65536],
            bounds: {
              minTicks: [ticks(32), ticks(-1), ticks(32)],
              maxTicksExclusive: [ticks(48), ticks(16), ticks(48)],
            },
            family: fixture.plant,
            variation: 0,
            lifecycle: "mature",
            phenotype: 0,
            representationClass: 0,
            deterministicKey: "d".padStart(32, "0"),
            candidate: "0",
            ecologyTick: "1",
            health: 65535,
            moisture: 32768,
            fuel: 32768,
            phenology: 0,
            flags: 1,
            interactionPolicy: "interactive",
            provenance: 0,
            surfaceProjectionBits: [0, 0, 0],
          },
        },
      },
    ],
  });
  expect(anchor.applied).toBe(1);
  await awaitMacroCount(engine, baselinePlants, "the anchored plant to join the cell");

  const anchored = await engine.call("vegetation-runtime-inspect", {
    plant: anchorId,
  });
  expect(anchored.resident).toBeTruthy();
  expect(anchored.resident?.family).toBe(fixture.plant);
}
