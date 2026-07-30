// Shared vegetation e2e vocabulary: the generated fixture shape, authored-asset
// installation into a temp dir, world bring-up, and the cook/evaluation wait loops.

import { expect } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ImportVegetationAssetResult,
  VegetationCookJobDto,
  VegetationCookStatusDto,
  VegetationEvaluationStatusDto,
  VegetationRuntimeCellResult,
} from "@saffron/protocol";
import { EngineCallError, type Engine } from "./harness.ts";
import { trackEntity, type Cleaner } from "./test-utils.ts";

const FIXTURE_DIR = join(dirname(fileURLToPath(import.meta.url)), "fixtures");

// Integer ticks per metre — the fixed-point world unit the placement pipeline is exact in.
export const TICKS_PER_METER = 4096;

// The level-zero cell at the world origin, which every fixture cooks into.
export const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;

// Region bounds spanning `meters` from the origin on every axis.
export function regionBounds(meters: number) {
  const extent = String(meters * TICKS_PER_METER);
  return {
    minTicks: ["0", "0", "0"],
    maxTicksExclusive: [extent, extent, extent],
  } as const;
}

// The region the canonical (4 m cell) fixtures place into.
export const BOUNDS = regionBounds(64);

// The region the woodland/canopy (64 m cell) fixtures place into.
export const WIDE_BOUNDS = regionBounds(1024);

// Reads a generated fixture by file stem from `fixtures/`.
export function loadFixture(stem: string): VegetationFixture {
  return JSON.parse(readFileSync(join(FIXTURE_DIR, `${stem}.json`), "utf8")) as VegetationFixture;
}

export interface VegetationMapObjectFixture {
  contentHash: string;
  hex: string;
}

export interface VegetationFixture {
  formatVersion: number;
  stress?: string;
  cells?: [number, number, number][];
  plantHex: string;
  biomeHex: string;
  mapHex: string;
  mapObjects: VegetationMapObjectFixture[];
  trunkObjHex: string;
  trunkObjPath: string;
  trunkMtlHex?: string;
  trunkMtlPath?: string;
  plant: string;
  biome: string;
  map: string;
  authoredLayer: string;
  biomeInstance: string;
  expectedPlant: string;
  expectedAccepted: string;
}

// Writes the fixture's plant/biome/map assets (plus map chunk objects) into a fresh
// temp dir the cleaner removes, returning the three asset paths.
export function authoredAssets(
  cleaner: Cleaner,
  fixture: VegetationFixture,
  tag: string,
): Record<"plant" | "biome" | "map", string> {
  const root = mkdtempSync(join(tmpdir(), `saffron-vegetation-${tag}-`));
  cleaner.defer(() => rmSync(root, { recursive: true, force: true }));
  const assets = {
    plant: join(root, "resident.splant"),
    biome: join(root, "resident.sbiome"),
    map: join(root, "resident.svegmap"),
  };
  writeFileSync(assets.plant, Buffer.from(fixture.plantHex, "hex"));
  writeFileSync(assets.biome, Buffer.from(fixture.biomeHex, "hex"));
  writeFileSync(assets.map, Buffer.from(fixture.mapHex, "hex"));
  for (const object of fixture.mapObjects) {
    const objectPath = join(`${assets.map}.data`, "objects", `${object.contentHash}.svegmapc`);
    mkdirSync(dirname(objectPath), { recursive: true });
    writeFileSync(objectPath, Buffer.from(object.hex, "hex"));
  }
  return assets;
}

// Writes the fixture's trunk OBJ into the live project's assets folder.
export async function installTrunkObj(engine: Engine, fixture: VegetationFixture) {
  const status = await engine.call<{ path: string }>("project-status");
  const projectRoot = status.path.endsWith("project.json") ? dirname(status.path) : status.path;
  const trunkPath = join(projectRoot, "assets", fixture.trunkObjPath);
  mkdirSync(dirname(trunkPath), { recursive: true });
  writeFileSync(trunkPath, Buffer.from(fixture.trunkObjHex, "hex"));
  if (fixture.trunkMtlHex && fixture.trunkMtlPath) {
    const mtlPath = join(projectRoot, "assets", fixture.trunkMtlPath);
    writeFileSync(mtlPath, Buffer.from(fixture.trunkMtlHex, "hex"));
  }
}

// Installs the fixture's authored assets + trunk geometry and imports all three into the
// live project, returning the source paths.
export async function importVegetationPackage(
  engine: Engine,
  cleaner: Cleaner,
  fixture: VegetationFixture,
  tag: string,
): Promise<Record<"plant" | "biome" | "map", string>> {
  const sources = authoredAssets(cleaner, fixture, tag);
  await installTrunkObj(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", { path });
  }
  return sources;
}

// Creates the world entity carrying the fixture's enabled `VegetationField`.
export async function bindVegetationField(
  engine: Engine,
  cleaner: Cleaner,
  fixture: VegetationFixture,
  name: string,
): Promise<EntityRef> {
  const world = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name }),
  );
  await engine.call("add-component", { entity: world.id, component: "VegetationField" });
  await engine.call("set-component", {
    entity: world.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });
  return world;
}

// Cooks the named cells of a map on one worker and waits for the job to complete.
export async function cookCells(
  engine: Engine,
  map: string,
  cells: readonly unknown[] = [CELL],
): Promise<VegetationCookStatusDto> {
  const cook = await engine.call<VegetationCookJobDto>("vegetation-cook", {
    map,
    scope: { kind: "cells", cells },
    workers: 1,
  });
  return awaitCook(engine, cook.job);
}

// Waits until the runtime reports `cell` resident with at least the expected macro plants
// (and, when asked, at least one micro field tile).
export async function awaitResidentCell(
  engine: Engine,
  expectedPlants: number,
  options: { microTiles?: boolean; timeoutMs?: number } = {},
): Promise<VegetationRuntimeCellResult> {
  const deadline = Date.now() + (options.timeoutMs ?? 30_000);
  for (;;) {
    let resident: VegetationRuntimeCellResult | undefined;
    try {
      resident = await engine.call<VegetationRuntimeCellResult>("vegetation-runtime-cell", {
        cell: CELL,
      });
    } catch {
      // The cell is not resident yet.
    }
    if (
      resident &&
      Number(resident.macroPlants) >= expectedPlants &&
      (!options.microTiles || Number(resident.microTiles) > 0)
    ) {
      return resident;
    }
    if (Date.now() >= deadline) {
      throw new Error("timeout waiting for the resident vegetation cell");
    }
    await engine.settle(50);
  }
}

export async function awaitEvaluation(engine: Engine, job: string, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await engine.call<VegetationEvaluationStatusDto>(
      "vegetation-evaluation-status",
      { job },
    );
    if (status.state === "completed") {
      expect(status.summary).toBeDefined();
      return status.summary!;
    }
    if (status.state === "failed") {
      if (!status.error) {
        throw new Error("vegetation evaluation failed without a typed failure");
      }
      throw new EngineCallError("vegetation-evaluation-status", status.error);
    }
    if (status.state === "cancelled") {
      throw new Error("vegetation evaluation was cancelled");
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for vegetation evaluation ${job}`);
    }
    await engine.settle(25);
  }
}

export async function awaitCook(
  engine: Engine,
  job: string,
  timeoutMs = 30_000,
): Promise<VegetationCookStatusDto> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await engine.call<VegetationCookStatusDto>("vegetation-cook-status", { job });
    if (status.state === "completed") {
      expect(status.statistics).toBeDefined();
      expect(status.manifest).toBeDefined();
      return status;
    }
    if (status.state === "failed") {
      if (!status.error) {
        throw new Error("vegetation cook failed without a typed failure");
      }
      throw new EngineCallError("vegetation-cook-status", status.error);
    }
    if (status.state === "cancelled" || status.state === "superseded") {
      throw new Error(`vegetation cook ${job} reached unexpected state ${status.state}`);
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for vegetation cook ${job}`);
    }
    await engine.settle(25);
  }
}
