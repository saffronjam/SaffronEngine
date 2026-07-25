// The vegetation stress matrix through the real host: each checked-in fixture (meadow
// micro-density, mixed multi-cell woodland with negative cells, extreme scale, and a
// rapid cell traversal) imports, cooks, streams resident, and renders records with no
// overflow — all while the frame stays Vulkan-validation-clean (the harness asserts a
// clean log at shutdown).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  GpuSceneMirrorStatsDto,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
  VegetationRuntimeCellResult,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const STRESS_FIXTURES = [
  "vegetation-stress-meadow.json",
  "vegetation-stress-woodland.json",
  "vegetation-stress-scale.json",
  "vegetation-stress-traversal.json",
];

const cleaner = new Cleaner();
let engine: Engine;
let world: EntityRef | undefined;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

function cellRef(cell: [number, number, number]) {
  return {
    coordinates: [String(cell[0]), String(cell[1]), String(cell[2])],
    level: 0,
  } as const;
}

for (const file of STRESS_FIXTURES) {
  test(`stress fixture ${file} cooks, streams, and renders clean`, async () => {
    const fixture = JSON.parse(
      readFileSync(join(HERE, "fixtures", file), "utf8"),
    ) as VegetationFixture;
    expect(fixture.stress).toBeDefined();
    const cells = fixture.cells ?? [];
    expect(cells.length).toBeGreaterThan(0);

    const sources = authoredAssets(cleaner, fixture, fixture.stress!);
    await installTrunkObj(engine, fixture);
    for (const path of [sources.plant, sources.biome, sources.map]) {
      await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
    }

    // The scene owns one VegetationField; the matrix rebinds it per fixture.
    if (!world) {
      world = trackEntity(
        cleaner,
        engine,
        await engine.call<EntityRef>("create-entity", { name: "Stress vegetation" }),
      );
      await engine.call("add-component", { entity: world.id, component: "VegetationField" });
    }
    await engine.call("set-component", {
      entity: world.id,
      component: "VegetationField",
      json: { map: fixture.map, enabled: true },
    });

    const cook = await engine.call<VegetationCookJobDto>("vegetation-cook", {
      map: fixture.map,
      scope: { kind: "cells", cells: cells.map(cellRef) },
      workers: 1,
    });
    const cooked = await awaitCook(engine, cook.job);
    expect(cooked.statistics!.publishedCells).toBe(String(cells.length));

    // Every fixture cell streams resident under a camera parked over it — including
    // the woodland's negative cells — and the visibility chain emits records with
    // no overflow. The traversal fixture sweeps the cells rapidly first.
    if (fixture.stress === "traversal") {
      for (let sweep = 0; sweep < 3; sweep += 1) {
        for (const cell of cells) {
          await engine.call("set-camera", {
            position: { x: cell[0] * 64 + 32, y: 8, z: cell[2] * 64 + 44 },
            yaw: 0,
            pitch: -10,
          });
          await engine.settle(60);
        }
      }
    }
    for (const cell of cells) {
      await engine.call("set-camera", {
        position: { x: cell[0] * 64 + 32, y: 8, z: cell[2] * 64 + 44 },
        yaw: 0,
        pitch: -10,
      });
      const deadline = Date.now() + 30_000;
      for (;;) {
        let resident = 0;
        let microTiles = 0;
        try {
          const status = await engine.call<VegetationRuntimeCellResult>(
            "vegetation-runtime-cell",
            { cell: cellRef(cell) },
          );
          resident = Number(status.macroPlants);
          microTiles = Number(status.microTiles);
        } catch {
          // The cell is not resident yet.
        }
        if (resident > 0 && microTiles > 0) {
          break;
        }
        if (Date.now() >= deadline) {
          throw new Error(`timeout waiting for stress cell ${cell} of ${file}`);
        }
        await engine.settle(50);
      }
    }

    {
      const deadline = Date.now() + 20_000;
      let stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
      let jiggle = 0;
      while (Date.now() < deadline) {
        jiggle += 1;
        await engine.call("set-camera", {
          position: { x: 32, y: 8 + (jiggle % 2) * 0.01, z: 44 },
          yaw: 0,
          pitch: -10,
        });
        stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
        if (stats.visibility.records > 0 && stats.visibility.microCandidates > 0) {
          break;
        }
        await engine.settle(50);
      }
      expect(stats.visibility.records).toBeGreaterThan(0);
      expect(stats.visibility.microCandidates).toBeGreaterThan(0);
      expect(stats.visibility.overflowFlags).toBe(0);
      expect(Number(stats.microPredicted)).toBeGreaterThanOrEqual(
        stats.visibility.microCandidates,
      );
    }

    // Park the field so the next fixture streams alone.
    await engine.call("set-component", {
      entity: world!.id,
      component: "VegetationField",
      json: { map: fixture.map, enabled: false },
    });
    await engine.settle(200);
  });
}
