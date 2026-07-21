// Vegetation acceptance through the real host: canonical package import, compiled evaluation,
// retained provenance, asynchronous cooking, manifest publication, and cell inspection.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  ImportVegetationAssetResult,
  ProvenanceExplanationDto,
  VegetationCellInspectResult,
  VegetationCompileBiomeResult,
  VegetationEvaluationJobDto,
  VegetationEvaluationStatusDto,
  VegetationCookJobDto,
  VegetationCookStatusDto,
  VegetationManifestResult,
} from "@saffron/protocol";
import { EngineCallError, type Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";

interface VegetationMapObjectFixture {
  contentHash: string;
  hex: string;
}

interface VegetationFixture {
  formatVersion: number;
  plantHex: string;
  biomeHex: string;
  mapHex: string;
  mapObjects: VegetationMapObjectFixture[];
  plant: string;
  biome: string;
  map: string;
  authoredLayer: string;
  biomeInstance: string;
  expectedPlant: string;
  expectedAccepted: string;
}

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = join(HERE, "fixtures", "vegetation-phase3.json");
const CELL = { coordinates: ["0", "0", "0"], level: 0 } as const;
const BOUNDS = {
  minTicks: ["0", "0", "0"],
  maxTicksExclusive: ["262144", "262144", "262144"],
} as const;
const RESIDENT_OPERATORS = new Set(["noise", "curve", "clamp", "field-importance"]);

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

function guid(value: number): string {
  return value.toString(16).padStart(32, "0");
}

function authoredAssets(fixture: VegetationFixture): Record<"plant" | "biome" | "map", string> {
  const root = mkdtempSync(join(tmpdir(), "saffron-vegetation-phase3-"));
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

async function awaitEvaluation(job: string, timeoutMs = 30_000) {
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

async function awaitCook(job: string, timeoutMs = 30_000): Promise<VegetationCookStatusDto> {
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

test("canonical vegetation package evaluates, explains, cooks, and inspects one cell", async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  expect(fixture.formatVersion).toBe(2);
  const sources = authoredAssets(fixture);

  const plant = await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", {
    path: sources.plant,
  });
  const biome = await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", {
    path: sources.biome,
  });
  const map = await engine.call<ImportVegetationAssetResult>("import-vegetation-asset", {
    path: sources.map,
  });
  expect(plant).toMatchObject({ id: fixture.plant, type: "plant" });
  expect(biome).toMatchObject({ id: fixture.biome, type: "biome" });
  expect(map).toMatchObject({ id: fixture.map, type: "vegetation-map" });

  const vegetationWorld = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: "Vegetation world" }),
  );
  await engine.call("add-component", {
    entity: vegetationWorld.id,
    component: "VegetationField",
  });
  await engine.call("set-component", {
    entity: vegetationWorld.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });

  const compiled = await engine.call<VegetationCompileBiomeResult>("vegetation-compile-biome", {
    target: {
      scope: "instance",
      map: fixture.map,
      biomeInstance: fixture.biomeInstance,
    },
  });
  expect(compiled.biome).toBe(fixture.biome);
  expect(compiled.biomeInstance).toBe(fixture.biomeInstance);
  expect(compiled.graphIdentity).toMatch(/^[0-9a-f]{64}$/);
  expect(compiled.requiredHaloBits).toBe(0);
  expect(compiled.dependencies).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ kind: "asset", identity: fixture.plant }),
      expect.objectContaining({ kind: "map-layer", identity: fixture.authoredLayer }),
    ]),
  );

  const prepared = await engine.call<VegetationEvaluationJobDto>("vegetation-preflight-region", {
    map: fixture.map,
    biomeInstance: fixture.biomeInstance,
    bounds: BOUNDS,
    level: 0,
    ecologyTick: "17",
    workers: 1,
  });
  expect(prepared.state).toBe("prepared");
  expect(prepared.preflight.outputCells).toBe("1");
  expect(prepared.preflight.globalStageTiles).toBe("0");
  expect(prepared.preflight.workerCount).toBe(1);
  for (const value of [
    prepared.preflight.outputCells,
    prepared.preflight.globalStageTiles,
    prepared.preflight.inputTiles,
    prepared.preflight.retainedInputBytes,
    prepared.preflight.generatedInputBytes,
    prepared.preflight.candidateCount,
    prepared.preflight.acceptedCount,
    prepared.preflight.microSamples,
    prepared.preflight.preflightPeakBytes,
    prepared.preflight.executionPeakBytes,
    prepared.preflight.memoryBytes,
    prepared.preflight.transferBytes,
    prepared.preflight.timeLimitMs,
  ]) {
    expect(value).toMatch(/^[0-9]+$/);
  }
  expect(prepared.preflight.limits).toEqual(compiled.limits);
  expect(prepared.preflight.candidateCount).toBe(fixture.expectedAccepted);
  expect(prepared.preflight.acceptedCount).toBe(fixture.expectedAccepted);
  const preflightPeak = BigInt(prepared.preflight.preflightPeakBytes);
  const executionPeak = BigInt(prepared.preflight.executionPeakBytes);
  expect(preflightPeak).toBeGreaterThan(0n);
  expect(executionPeak).toBeGreaterThan(0n);
  expect(BigInt(prepared.preflight.memoryBytes)).toBe(
    preflightPeak > executionPeak ? preflightPeak : executionPeak,
  );
  expect(BigInt(prepared.preflight.transferBytes)).toBeGreaterThan(0n);

  const reconnected = await engine.call<VegetationEvaluationStatusDto>(
    "vegetation-evaluation-status",
    { job: prepared.job },
  );
  expect(reconnected.state).toBe("prepared");
  expect(reconnected.preflight).toEqual(prepared.preflight);
  expect(reconnected.summary).toBeNull();
  expect(reconnected.error).toBeNull();

  const started = await engine.call<VegetationEvaluationJobDto>("vegetation-start-evaluation", {
    job: prepared.job,
  });
  expect(started.state).toBe("running");
  expect(started.preflight).toEqual(prepared.preflight);
  await expect(engine.call("vegetation-start-evaluation", { job: prepared.job })).rejects.toThrow(
    /not prepared/,
  );

  const summary = await awaitEvaluation(started.job);
  expect(summary).toMatchObject({
    cells: "1",
    globalStages: "0",
    candidates: fixture.expectedAccepted,
    accepted: fixture.expectedAccepted,
    rejected: "0",
    microTiles: "0",
  });
  expect(summary.canonicalHash).toMatch(/^[0-9a-f]{64}$/);
  expect(BigInt(summary.candidates)).toBeLessThanOrEqual(BigInt(prepared.preflight.candidateCount));
  expect(BigInt(summary.accepted)).toBeLessThanOrEqual(BigInt(prepared.preflight.acceptedCount));

  const residentNodes = summary.nodes.filter((node) => RESIDENT_OPERATORS.has(node.operator));
  expect(residentNodes.map((node) => node.node)).toEqual([guid(3), guid(4), guid(5), guid(6)]);
  for (const node of summary.nodes) {
    expect(node.symbol.length).toBeGreaterThan(0);
    for (const value of [
      node.inputCandidates,
      node.outputCandidates,
      node.outputBytes,
      node.predictedTransferBytes,
      node.elapsedMicros,
    ]) {
      expect(value).toMatch(/^[0-9]+$/);
    }
    if (RESIDENT_OPERATORS.has(node.operator)) {
      expect(node.executionDomain).toBe("slang-compute");
      expect(node.elapsedMicros).toBe("0");
    } else {
      expect(node.executionDomain).not.toBe("slang-compute");
    }
  }

  expect(summary.gpuGroups).toHaveLength(1);
  const group = summary.gpuGroups[0]!;
  expect(group.nodes).toEqual([3, 4, 5, 6].map((node) => ({ modulePath: [], node: guid(node) })));
  expect(group.invocationCount).toBe(fixture.expectedAccepted);
  expect(BigInt(group.outputBytes)).toBeGreaterThan(0n);
  expect(group.transferBytes).toBe(prepared.preflight.transferBytes);
  expect(group.elapsedMicros).toMatch(/^[0-9]+$/);

  const explanation = await engine.call<ProvenanceExplanationDto>("vegetation-explain-point", {
    job: started.job,
    cell: CELL,
    subject: { kind: "plant", plant: fixture.expectedPlant },
  });
  expect(explanation.record).toMatchObject({
    map: fixture.map,
    layer: fixture.biomeInstance,
    biome: fixture.biome,
    family: fixture.plant,
    plant: fixture.expectedPlant,
  });
  expect(explanation.rejectionReason).toBeNull();

  const produced = explanation.decisions.find(
    (decision) => decision.operator === "stratified-coverage",
  );
  const retained = explanation.decisions.find(
    (decision) => decision.operator === "field-importance",
  );
  const accepted = explanation.decisions.find((decision) => decision.operator === "macro-output");
  expect(produced).toMatchObject({ node: guid(2), outcome: "produced", parents: [] });
  expect(retained).toMatchObject({
    node: guid(6),
    outcome: "retained",
    parents: [produced!.handle],
  });
  expect(accepted).toMatchObject({
    node: guid(8),
    outcome: "accepted",
    parents: [retained!.handle],
  });
  expect(explanation.record.decision).toBe(accepted!.handle);
  expect(explanation.record.candidate).toBe(produced!.candidate);
  expect(retained!.candidate).toBe(produced!.candidate);
  expect(accepted!.candidate).toBe(produced!.candidate);

  const cook = await engine.call<VegetationCookJobDto>("vegetation-cook", {
    map: fixture.map,
    scope: { kind: "cells", cells: [CELL] },
    workers: 1,
  });
  expect(["queued", "running"]).toContain(cook.state);
  expect(cook.scope).toEqual({ kind: "cells", cells: [CELL] });
  const cooked = await awaitCook(cook.job);
  expect(cooked.progress.completedNodes).toBe(cooked.progress.totalNodes);
  expect(BigInt(cooked.progress.totalNodes)).toBeGreaterThan(0n);
  expect(cooked.statistics).toMatchObject({ publishedCells: "1" });
  expect(cooked.manifest!.map).toBe(fixture.map);
  expect(cooked.manifest!.plants).toEqual([
    expect.objectContaining({ family: fixture.plant, tags: ["1"] }),
  ]);
  expect(cooked.manifest!.cells).toHaveLength(1);
  expect(cooked.manifest!.cells[0]).toMatchObject({
    cell: CELL,
    macroCount: fixture.expectedAccepted,
    microCount: "0",
  });
  expect(cooked.manifest!.identity).toMatch(/^[0-9a-f]{64}$/);

  const manifest = await engine.call<VegetationManifestResult>("vegetation-manifest", {
    map: fixture.map,
  });
  expect(manifest.manifest.identity).toBe(cooked.manifest!.identity);
  expect(manifest.manifest.cells[0]!.artifactHash).toBe(cooked.manifest!.cells[0]!.artifactHash);
  expect(manifest.manifest.dependencies).toHaveLength(cooked.manifest!.dependencies.length);
  expect(manifest.manifest.dependencies).toEqual(
    expect.arrayContaining(cooked.manifest!.dependencies),
  );
  expect(manifest.manifest.cells[0]!.actual).toEqual({
    elapsedMicros: "0",
    peakMemoryBytes: "0",
    inputBytes: "0",
    outputBytes: "0",
    rejectionCount: "0",
    cacheHit: false,
  });
  expect(manifest.latestCook).toEqual(cooked.statistics!);

  const inspected = await engine.call<VegetationCellInspectResult>("vegetation-cell-inspect", {
    map: fixture.map,
    cell: CELL,
  });
  expect(inspected.cell).toMatchObject({
    map: fixture.map,
    manifest: cooked.manifest!.identity,
    cell: CELL,
    macroPoints: fixture.expectedAccepted,
    microSamples: "0",
  });
  expect(inspected.cell.contentHash).toBe(cooked.manifest!.cells[0]!.artifactHash);
  expect(inspected.cell.sections.map((section) => section.kind)).toEqual([
    "macro-points",
    "micro-fields",
    "provenance",
    "rejection-diagnostics",
    "surface-attachments",
    "surface-dependencies",
    "render-references",
    "render-bounds",
    "collision-inputs",
    "navigation-contributions",
    "ecology-boundary",
    "ecology-checkpoint",
  ]);

  const cancelledPrepared = await engine.call<VegetationEvaluationJobDto>(
    "vegetation-preflight-region",
    {
      map: fixture.map,
      biomeInstance: fixture.biomeInstance,
      bounds: BOUNDS,
      level: 0,
      ecologyTick: "17",
      workers: 1,
    },
  );
  const cancelled = await engine.call<VegetationEvaluationStatusDto>(
    "vegetation-cancel-evaluation",
    { job: cancelledPrepared.job },
  );
  expect(cancelled.state).toBe("cancelled");
  expect(cancelled.preflight).toEqual(cancelledPrepared.preflight);
  await expect(
    engine.call("vegetation-start-evaluation", { job: cancelledPrepared.job }),
  ).rejects.toThrow(/not prepared/);

  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);
