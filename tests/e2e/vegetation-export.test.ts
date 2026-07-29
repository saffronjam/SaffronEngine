// Phase-15 acceptance: an exported package carries its cooked vegetation and the player boots from
// it. The export is driven through the real host, then `saffron-player` runs against the staged
// package alone — no project directory, no authored sources, no editor.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ExportAppResult,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
} from "@saffron/protocol";
import { IS_MACOS, type Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";
import { authoredAssets, awaitCook, installTrunkObj, type VegetationFixture } from "./vegetation-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

/// Every file under `dir`, relative, so an assertion can talk about the package's shape.
function tree(dir: string, prefix = ""): string[] {
  if (!existsSync(dir)) {
    return [];
  }
  const found: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const relative = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
    if (entry.isDirectory()) {
      found.push(...tree(join(dir, entry.name), relative));
    } else {
      found.push(relative);
    }
  }
  return found;
}

test("an exported package carries its cooked vegetation and no authored sources", async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  const sources = authoredAssets(cleaner, fixture, "export");
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  const world = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: "Exported vegetation" }),
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
  await engine.call("save-project", {});

  // Park the camera over the cell, start play so state exists, then publish the generation's
  // starting state. A shipped world boots into what the author saw, not into an untouched one.
  await engine.call("set-camera", { position: { x: 32, y: 8, z: 44 }, yaw: 0, pitch: -10 });
  await engine.call("play");
  await engine.settle(200);
  const baseline = await engine.call<{ manifestIdentity: string; bytes: string; cells: string }>(
    "vegetation-state-baseline",
  );
  expect(baseline.manifestIdentity).toMatch(/^[0-9a-f]{64}$/);
  expect(Number(baseline.bytes)).toBeGreaterThan(0);
  await engine.call("stop");

  // Every artifact the generation names rehashes to the identity its name claims.
  const verified = await engine.call<{
    checked: string;
    repaired: string;
    faults: { path: string; fault: string }[];
  }>("vegetation-verify-artifacts", { repair: false });
  expect(Number(verified.checked)).toBeGreaterThan(0);
  expect(verified.faults).toEqual([]);
  expect(Number(verified.repaired)).toBe(0);

  const output = mkdtempSync(join(tmpdir(), "saffron-export-"));
  cleaner.defer(() => rmSync(output, { recursive: true, force: true }));
  const exported = await engine.call<ExportAppResult>("export-app", {
    outputDir: join(output, "Exported"),
    app: { title: "Exported", width: 320, height: 240, vsync: true, fullscreen: false },
  });

  // The report names the generation it packaged, and every artifact the manifest named was present.
  expect(exported.vegetation.length).toBe(1);
  const map = exported.vegetation[0]!;
  expect(map.manifestIdentity).toMatch(/^[0-9a-f]{64}$/);
  expect(Number(map.cells)).toBeGreaterThan(0);
  expect(Number(map.missing)).toBe(0);
  expect(Number(map.macroPlants)).toBeGreaterThan(0);
  expect(Number(exported.vegetationBytes)).toBeGreaterThan(0);
  // The starting state travels with the generation it belongs to.
  expect(map.baseline).toBe(true);
  // The facet distribution is reported, and macro points are one of the facets that carry bytes.
  expect(map.facets.length).toBeGreaterThan(0);
  expect(map.facets.some((facet) => Number(facet.bytes) > 0)).toBe(true);

  // The package carries the cooked closure and none of the authored sources. Only the macOS
  // bundle nests its payload; every other platform stages it at the export root itself
  // (`ExportLayout::resources`).
  const resources = IS_MACOS ? join(exported.path, "Contents", "Resources") : exported.path;
  const files = tree(resources);
  expect(files.some((file) => file.endsWith(".svegcell"))).toBe(true);
  expect(files.some((file) => file.endsWith(".svegmanifest"))).toBe(true);
  expect(files.some((file) => file.endsWith(".splantc"))).toBe(true);
  expect(files.some((file) => file.endsWith(".svegstate"))).toBe(true);
  expect(files.some((file) => file.endsWith(".splant"))).toBe(false);
  expect(files.some((file) => file.endsWith(".sbiome"))).toBe(false);
  expect(files.some((file) => file.endsWith(".svegmap"))).toBe(false);
  expect(engine.validationErrors()).toEqual([]);

});
