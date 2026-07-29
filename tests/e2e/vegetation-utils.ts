// Shared vegetation e2e vocabulary: the generated fixture shape, authored-asset
// installation into a temp dir, and the cook/evaluation wait loops.

import { expect } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import type { VegetationCookStatusDto, VegetationEvaluationStatusDto } from "@saffron/protocol";
import { EngineCallError, type Engine } from "./harness.ts";
import type { Cleaner } from "./test-utils.ts";

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

/// Writes the fixture's plant/biome/map assets (plus map chunk objects) into a fresh
/// temp dir the cleaner removes, returning the three asset paths.
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

/// Writes the fixture's trunk OBJ into the live project's assets folder.
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
