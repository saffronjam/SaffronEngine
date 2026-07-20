// Phase-3 vegetation graph acceptance through the real host, native asset import, one compiled
// evaluator, the resident Slang execution path, and retained provenance explanation.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  ImportVegetationAssetResult,
  ProvenanceExplanationDto,
  VegetationCompileBiomeResult,
  VegetationEvaluationJobDto,
  VegetationEvaluationStatusDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";

interface VegetationFixture {
  formatVersion: number;
  plantHex: string;
  biomeHex: string;
  mapHex: string;
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
      throw new Error(`vegetation evaluation failed: ${status.error ?? "unknown error"}`);
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

test("authored biome evaluation reports one resident GPU group and complete provenance", async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  expect(fixture.formatVersion).toBe(1);
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

  const started = await engine.call<VegetationEvaluationJobDto>("vegetation-evaluate-region", {
    map: fixture.map,
    biomeInstance: fixture.biomeInstance,
    bounds: BOUNDS,
    level: 0,
    ecologyTick: "17",
    workers: 1,
  });
  expect(started.cells).toBe("1");
  expect(started.state).toBe("running");

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
  expect(BigInt(group.transferBytes)).toBeGreaterThan(BigInt(group.outputBytes));
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

  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);
