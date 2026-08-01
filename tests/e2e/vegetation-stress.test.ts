// The vegetation stress matrix through the real host: each checked-in fixture (meadow
// micro-density, mixed multi-cell woodland with negative cells, extreme scale, a rapid
// cell traversal, and the three leaf-content rows) imports, cooks, streams resident, and
// renders records with no overflow — all while the frame stays Vulkan-validation-clean
// (the harness asserts a clean log at shutdown). The leaf rows additionally render their
// family alone through the asset-preview view, where one family's draws are the only ones
// on the cut.
//
// Every row owns its engine: a fixture is a whole world, and one host streaming a second
// world after the first would report the pair's residency rather than the fixture's.

import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  PlantAssetSummaryDto,
  PlantAtlasResult,
  VegetationAssetSummaryResult,
  WorldCellDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";
import { authoredAssets, awaitCook, installPlantSources, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const STRESS_FIXTURES = [
  "vegetation-stress-meadow.json",
  "vegetation-stress-woodland.json",
  "vegetation-stress-scale.json",
  "vegetation-stress-traversal.json",
  "vegetation-stress-broadleaf.json",
  "vegetation-stress-serration.json",
  "vegetation-stress-needles.json",
];

// The rows whose family carries authored foliage: modelled broad blades, the same blades
// with the serrated edge left to an alpha cutout, and dense needle slivers.
const LEAF_ROWS = ["broadleaf", "serration", "needles"];

function cellRef(cell: [number, number, number]): WorldCellDto {
  return {
    coordinates: [String(cell[0]), String(cell[1]), String(cell[2])],
    level: 0,
  };
}

// The `.splant` leg of a catalog summary reply; any other kind is a broken test setup.
function plantSummary(result: VegetationAssetSummaryResult): PlantAssetSummaryDto {
  if (result.summary.kind !== "plant") {
    throw new Error(`the catalog summarises this asset as ${result.summary.kind}, not a plant`);
  }
  return result.summary.asset;
}

// The leaf rows differ only in where a blade's silhouette lives, and the cooked family says
// which: a masked slot's coverage is packed into the family atlas, and a slot whose silhouette
// is modelled contributes nothing to pack. `plant-atlas` refuses a family that packed none, so
// the serration row must return one and the two geometry-first rows must not.
async function assertCoverageAtlas(engine: Engine, fixture: VegetationFixture) {
  const atlas = await engine
    .call("plant-atlas", { plant: fixture.plant, level: 0 })
    .then((result) => result, (error: Error) => error);
  if (fixture.stress === "serration") {
    expect(atlas).not.toBeInstanceOf(Error);
    const packed = atlas as PlantAtlasResult;
    expect(packed.placements.length).toBeGreaterThan(0);
    expect(packed.placements.every((slot) => slot.width > 0 && slot.height > 0)).toBe(true);
  } else {
    // The refusal is read for its reason: any other failure is a broken row, not a family
    // whose silhouette is modelled.
    expect(String(atlas)).toContain("cooked no packed atlas");
  }
}

// Renders the fixture's family alone in the isolated asset-preview scene: the authored
// trunk/leaf split survives the cook, and the family reaches the cut on its own, with no
// vegetation field bound and so no micro blade among its records.
async function previewFamily(engine: Engine, fixture: VegetationFixture) {
  const summary = plantSummary(
    await engine.call("vegetation-asset-summary", { asset: fixture.plant }),
  );
  expect(summary.validation.valid).toBe(true);
  expect(summary.partCount).toBe(2);
  expect(summary.materialSlots.length).toBe(2);

  await engine.call("enter-asset-preview", { asset: fixture.plant });
  let stats = await engine.call("gpu-scene-stats");
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline && stats.visibility.records === 0) {
    await engine.settle(50);
    stats = await engine.call("gpu-scene-stats");
  }
  expect(stats.visibility.records).toBeGreaterThan(0);
  expect(stats.visibility.overflowFlags).toBe(0);
  expect(stats.visibility.microCandidates).toBe(0);
  await engine.call("exit-asset-preview");
}

for (const file of STRESS_FIXTURES) {
  test(`stress fixture ${file} cooks, streams, and renders clean`, async () => {
    const fixture = JSON.parse(
      readFileSync(join(HERE, "fixtures", file), "utf8"),
    ) as VegetationFixture;
    expect(fixture.stress).toBeDefined();
    const cells = fixture.cells ?? [];
    expect(cells.length).toBeGreaterThan(0);

    const cleaner = new Cleaner();
    try {
      const engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
      const sources = authoredAssets(cleaner, fixture, fixture.stress!);
      await installPlantSources(engine, fixture);
      for (const path of [sources.plant, sources.biome, sources.map]) {
        await engine.call("import-vegetation-asset", { path });
      }

      const world = trackEntity(
        cleaner,
        engine,
        await engine.call("create-entity", { name: "Stress vegetation" }),
      );
      await engine.call("add-component", { entity: world.id, component: "VegetationField" });
      await engine.call("set-component", {
        entity: world.id,
        component: "VegetationField",
        json: { map: fixture.map, enabled: true },
      });

      const cook = await engine.call("vegetation-cook", {
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
            const status = await engine.call("vegetation-runtime-cell", { cell: cellRef(cell) });
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
        let stats = await engine.call("gpu-scene-stats");
        let jiggle = 0;
        while (Date.now() < deadline) {
          jiggle += 1;
          await engine.call("set-camera", {
            position: { x: 32, y: 8 + (jiggle % 2) * 0.01, z: 44 },
            yaw: 0,
            pitch: -10,
          });
          stats = await engine.call("gpu-scene-stats");
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

      // Park the field so the preview scene streams the family alone.
      await engine.call("set-component", {
        entity: world.id,
        component: "VegetationField",
        json: { map: fixture.map, enabled: false },
      });
      await engine.settle(200);

      if (LEAF_ROWS.includes(fixture.stress!)) {
        await assertCoverageAtlas(engine, fixture);
        await previewFamily(engine, fixture);
      }
    } finally {
      await cleaner.cleanup();
    }
  });
}
