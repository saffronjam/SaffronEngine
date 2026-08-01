// The authoring half of the vegetation acceptance run: canonical package import, the
// optimistic map-layer transaction, biome compilation, and the preflight → evaluate →
// explain cycle over the compiled graph.

import { expect } from "bun:test";
import type {
  EntityRef,
  VegetationCompileBiomeResult,
  VegetationEvaluationJobDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, trackEntity } from "./test-utils.ts";
import {
  awaitEvaluation,
  BOUNDS,
  CELL,
  installPlantSources,
  type VegetationFixture,
  vegetationMap,
} from "./vegetation-utils.ts";

// The nodes the fixture's graph executes on the GPU, in declaration order.
const RESIDENT_OPERATORS = new Set(["noise", "curve", "clamp", "field-importance"]);

// The fixture authors node ids as zero-padded ordinals.
function guid(value: number): string {
  return value.toString(16).padStart(32, "0");
}

export async function importPackage(
  engine: Engine,
  fixture: VegetationFixture,
  sources: Record<"plant" | "biome" | "map", string>,
): Promise<void> {
  // The plant recipe references its trunk geometry by project-relative path; write the
  // fixture's OBJ into the live project's assets before anything resolves it.
  await installPlantSources(engine, fixture);

  const plant = await engine.call("import-vegetation-asset", {
    path: sources.plant,
  });
  const biome = await engine.call("import-vegetation-asset", {
    path: sources.biome,
  });
  const map = await engine.call("import-vegetation-asset", {
    path: sources.map,
  });
  expect(plant).toMatchObject({ id: fixture.plant, type: "plant" });
  expect(biome).toMatchObject({ id: fixture.biome, type: "biome" });
  expect(map).toMatchObject({ id: fixture.map, type: "vegetation-map" });
}

// Authored-map layer edits are one optimistic wire transaction: mute the grove layer
// (revision + 1 against the current generation), then restore it — the summary reflects
// each commit and the root generation advances.
export async function commitLayerTransaction(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<void> {
  const before = await engine.call("vegetation-asset-summary", {
    asset: fixture.map,
  });
  expect(before.layers).toHaveLength(1);
  const layer = before.layers[0]!;
  expect(layer.id).toBe(fixture.authoredLayer);
  expect(layer.muted).toBe(false);
  const commitLayer = (muted: boolean, revision: bigint) => ({
    ...layer,
    muted,
    revision: revision.toString(),
  });
  const revision = BigInt(layer.revision);
  const mutedCommit = await engine.call("vegetation-map-layer-commit", {
    map: fixture.map,
    expectedGeneration: vegetationMap(before).generation,
    upserts: [commitLayer(true, revision + 1n)],
    removals: [],
  });
  const mid = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  expect(vegetationMap(mid).generation).toBe(mutedCommit.generation);
  expect(mid.layers[0]!.muted).toBe(true);
  const restored = await engine.call("vegetation-map-layer-commit", {
    map: fixture.map,
    expectedGeneration: mutedCommit.generation,
    upserts: [commitLayer(false, revision + 2n)],
    removals: [],
  });
  expect(BigInt(restored.generation)).toBe(BigInt(mutedCommit.generation) + 1n);
  const after = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  expect(after.layers[0]!.muted).toBe(false);
}

export async function bindWorld(
  engine: Engine,
  cleaner: Cleaner,
  fixture: VegetationFixture,
): Promise<EntityRef> {
  const world = trackEntity(
    cleaner,
    engine,
    await engine.call("create-entity", { name: "Vegetation world" }),
  );
  await engine.call("add-component", { entity: world.id, component: "VegetationField" });
  await engine.call("set-component", {
    entity: world.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });
  return world;
}

export async function compileBiome(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<VegetationCompileBiomeResult> {
  const compiled = await engine.call("vegetation-compile-biome", {
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
  return compiled;
}

async function preflightRegion(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<VegetationEvaluationJobDto> {
  return engine.call("vegetation-preflight-region", {
    map: fixture.map,
    biomeInstance: fixture.biomeInstance,
    bounds: BOUNDS,
    level: 0,
    ecologyTick: "17",
    workers: 1,
  });
}

// Preflights the region, reconnects to the prepared job, runs it, and checks the summary +
// per-node accounting against the preflight prediction.
export async function evaluateRegion(
  engine: Engine,
  fixture: VegetationFixture,
  compiled: VegetationCompileBiomeResult,
): Promise<{ job: string }> {
  const prepared = await preflightRegion(engine, fixture);
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

  const reconnected = await engine.call("vegetation-evaluation-status", { job: prepared.job });
  expect(reconnected.state).toBe("prepared");
  expect(reconnected.preflight).toEqual(prepared.preflight);
  expect(reconnected.summary).toBeNull();
  expect(reconnected.error).toBeNull();

  const started = await engine.call("vegetation-start-evaluation", {
    job: prepared.job,
  });
  expect(started.state).toBe("running");
  expect(started.preflight).toEqual(prepared.preflight);
  await expect(engine.call("vegetation-start-evaluation", { job: prepared.job })).rejects.toThrow(
    /not prepared/,
  );

  const summary = await awaitEvaluation(engine, started.job);
  expect(summary).toMatchObject({
    cells: "1",
    globalStages: "0",
    candidates: fixture.expectedAccepted,
    accepted: fixture.expectedAccepted,
    rejected: "0",
  });
  expect(BigInt(summary.microTiles)).toBeGreaterThan(0n);
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

  return { job: started.job };
}

// The provenance chain of one accepted plant: production, retention, and acceptance each
// name their node and carry the same candidate handle forward.
export async function explainAcceptedPlant(
  engine: Engine,
  fixture: VegetationFixture,
  job: string,
): Promise<void> {
  const explanation = await engine.call("vegetation-explain-point", {
    job,
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
}

// A prepared job that is cancelled instead of started can never run afterwards.
export async function cancelPreparedEvaluation(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<void> {
  const prepared = await preflightRegion(engine, fixture);
  const cancelled = await engine.call("vegetation-cancel-evaluation", { job: prepared.job });
  expect(cancelled.state).toBe("cancelled");
  expect(cancelled.preflight).toEqual(prepared.preflight);
  await expect(
    engine.call("vegetation-start-evaluation", { job: prepared.job }),
  ).rejects.toThrow(/not prepared/);
}
