// Vegetation acceptance through the real host: canonical package import, compiled evaluation,
// retained provenance, asynchronous cooking, manifest publication, and cell inspection.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EntityRef,
  GpuSceneMirrorStatsDto,
  ImportVegetationAssetResult,
  ProvenanceExplanationDto,
  VegetationCellInspectResult,
  VegetationCompileBiomeResult,
  VegetationEvaluationJobDto,
  VegetationCookJobDto,
  VegetationManifestResult,
  VegetationRuntimeCellResult,
} from "@saffron/protocol";
import { EngineCallError, type Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";
import {
  authoredAssets as installAuthoredAssets,
  awaitCook as awaitCookWith,
  awaitEvaluation as awaitEvaluationWith,
  installTrunkObj,
  type VegetationFixture,
} from "./vegetation-utils.ts";

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
  return installAuthoredAssets(cleaner, fixture, "phase3");
}

async function awaitEvaluation(job: string, timeoutMs = 30_000) {
  return awaitEvaluationWith(engine, job, timeoutMs);
}

async function awaitCook(job: string, timeoutMs = 30_000) {
  return awaitCookWith(engine, job, timeoutMs);
}


test("canonical vegetation package evaluates, explains, cooks, and inspects one cell", async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as VegetationFixture;
  expect(fixture.formatVersion).toBe(3);
  const sources = authoredAssets(fixture);

  // The plant recipe references its trunk geometry by project-relative path; write the
  // fixture's OBJ into the live project's assets before anything resolves it.
  await installTrunkObj(engine, fixture);

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

  // Authored-map layer edits are one optimistic wire transaction: mute the grove
  // layer (revision + 1 against the current generation), then restore it — the
  // summary reflects each commit and the root generation advances.
  {
    interface MapSummary {
      summary: { kind: string; asset: { generation: string } };
      layers: { id: string; muted: boolean; revision: string; [key: string]: unknown }[];
    }
    const before = await engine.call<MapSummary>("vegetation-asset-summary", {
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
    const mutedCommit = await engine.call<{ generation: string }>(
      "vegetation-map-layer-commit",
      {
        map: fixture.map,
        expectedGeneration: before.summary.asset.generation,
        upserts: [commitLayer(true, revision + 1n)],
        removals: [],
      },
    );
    const mid = await engine.call<MapSummary>("vegetation-asset-summary", {
      asset: fixture.map,
    });
    expect(mid.summary.asset.generation).toBe(mutedCommit.generation);
    expect(mid.layers[0]!.muted).toBe(true);
    const restored = await engine.call<{ generation: string }>("vegetation-map-layer-commit", {
      map: fixture.map,
      expectedGeneration: mutedCommit.generation,
      upserts: [commitLayer(false, revision + 2n)],
      removals: [],
    });
    expect(BigInt(restored.generation)).toBe(BigInt(mutedCommit.generation) + 1n);
    const after = await engine.call<MapSummary>("vegetation-asset-summary", {
      asset: fixture.map,
    });
    expect(after.layers[0]!.muted).toBe(false);
  }

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
  });
  expect(BigInt(cooked.manifest!.cells[0]!.microCount)).toBeGreaterThan(0n);
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
  });
  expect(BigInt(inspected.cell.microSamples)).toBeGreaterThan(0n);
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

  // The cooked map goes live: the runtime binds the manifest, streams cell (0,0,0)
  // resident off the editor camera's spatial source, and the GPU-scene mirror
  // translates its macro plants into persistent-scene instances whose family pages
  // stream resident and whose traversal emits draw records.
  await engine.call("set-camera", {
    position: { x: 32, y: 6, z: 44 },
    yaw: 0,
    pitch: -5,
  });
  const expectedPlants = Number(fixture.expectedAccepted);
  {
    const deadline = Date.now() + 30_000;
    for (;;) {
      let resident = 0;
      let microTiles = 0;
      try {
        const residentCell = await engine.call<VegetationRuntimeCellResult>(
          "vegetation-runtime-cell",
          { cell: CELL },
        );
        resident = Number(residentCell.macroPlants);
        microTiles = Number(residentCell.microTiles);
      } catch {
        // The cell is not resident yet.
      }
      if (resident >= expectedPlants && microTiles > 0) {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the resident vegetation cell");
      }
      await engine.settle(50);
    }
  }
  {
    const deadline = Date.now() + 30_000;
    let stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
    let jiggle = 0;
    while (Date.now() < deadline) {
      // Nudge the camera each poll so the reactive loop keeps rendering frames while
      // the pages stream and the visibility counters read back.
      jiggle += 1;
      await engine.call("set-camera", {
        position: { x: 32, y: 6 + (jiggle % 2) * 0.01, z: 44 },
        yaw: 0,
        pitch: -5,
      });
      stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
      if (
        stats.instances >= expectedPlants &&
        stats.pageResidency.resident > 0 &&
        stats.visibility.records > 0 &&
        stats.visibility.microCandidates > 0
      ) {
        break;
      }
      await engine.settle(50);
    }
    expect(stats.instances).toBeGreaterThanOrEqual(expectedPlants);
    expect(stats.meshes).toBeGreaterThanOrEqual(1);
    expect(stats.pageResidency.resident).toBeGreaterThan(0);
    expect(stats.visibility.records).toBeGreaterThan(0);
    // Micro reconstruction is live: blades generated from the resident field
    // tiles, appended to the same record stream.
    expect(stats.visibility.microCandidates).toBeGreaterThan(0);
    expect(stats.visibility.records).toBeGreaterThan(stats.visibility.microCandidates - 1);
    // The predicted tile budget is the cooked density upper bound: the per-frame
    // generated count (view-culled) never exceeds it.
    expect(Number(stats.microPredicted)).toBeGreaterThan(0);
    expect(Number(stats.microPredicted)).toBeGreaterThanOrEqual(stats.visibility.microCandidates);
    expect(stats.visibility.overflowFlags).toBe(0);
  }

  // The per-family/per-cell telemetry matrix: the birch family rows up with its
  // resident plants, field tiles, and predicted budget, and the resident cell
  // reports its population.
  {
    const renderStats = await engine.call<{
      families: {
        family: string;
        instances: number;
        fieldTiles: number;
        microPredicted: number;
      }[];
      cells: {
        cell: { coordinates: string[]; level: number };
        plants: number;
        fieldTiles: number;
      }[];
      pageFaults: string;
    }>("vegetation-render-stats");
    expect(renderStats.families.length).toBeGreaterThan(0);
    const populated = renderStats.families.find((row) => row.instances > 0);
    expect(populated).toBeDefined();
    expect(populated?.fieldTiles).toBeGreaterThan(0);
    expect(Number(populated?.microPredicted)).toBeGreaterThan(0);
    const residentCell = renderStats.cells.find(
      (row) =>
        row.cell.level === 0 && row.cell.coordinates.every((coordinate) => coordinate === "0"),
    );
    expect(residentCell).toBeDefined();
    expect(residentCell?.plants).toBeGreaterThanOrEqual(expectedPlants);
    expect(Number(renderStats.pageFaults)).toBeGreaterThanOrEqual(0);
  }

  // The one viewport pick merges the vegetation vocabulary: aimed at a trunk, the
  // nearest hit is the macro plant, returned by stable identity (resolved through the
  // CPU cell snapshot, never a GPU slot).
  await engine.call("set-camera", {
    position: { x: 16, y: 4, z: 30 },
    yaw: 0,
    pitch: 0,
  });
  await engine.settle(200);
  const picked = await engine.call<{
    hit: boolean;
    kind?: string;
    plant?: string;
  }>("pick", { u: 0.5, v: 0.5 });
  expect(picked.hit).toBe(true);
  expect(picked.kind).toBe("vegetation");
  expect(picked.plant).toMatch(/^[0-9a-f]{32}$/);

  // Aimed straight down at vegetated ground away from every macro trunk, the same
  // pick falls through to the micro field: a nonpersistent paint-feedback point on
  // the cell floor, carrying a position but no identity. The runtime query supplies
  // the trunk bounds so the scan only samples spots whose ray misses every plant.
  const runtimePlants = await engine.call<{
    hits: { plant: { bounds: { minTicks: string[]; maxTicksExclusive: string[] } } }[];
  }>("vegetation-runtime-query", {
    query: { kind: "bounds", bounds: BOUNDS },
    limit: 256,
  });
  expect(runtimePlants.hits.length).toBeGreaterThan(0);

  // Phenology: the rendered phenotype resolves from typed lifecycle + season. The
  // canonical family has no seasonal phenotype, so the resolution is the cooked
  // identity in June and stays so after an autumn date scrub (which must also keep
  // frames validation-clean through the season-keyed cell rebuild).
  const phenotyped = await engine.call<{
    hits: { plant: { phenotype: number; renderedPhenotype: number } }[];
  }>("vegetation-runtime-query", {
    query: { kind: "bounds", bounds: BOUNDS },
    limit: 8,
  });
  for (const hit of phenotyped.hits) {
    expect(hit.plant.renderedPhenotype).toBe(hit.plant.phenotype);
  }
  await engine.call("set-time-of-day", { json: { month: 10, day: 15 } });
  await engine.settle(200);
  const autumn = await engine.call<{
    hits: { plant: { phenotype: number; renderedPhenotype: number } }[];
  }>("vegetation-runtime-query", {
    query: { kind: "bounds", bounds: BOUNDS },
    limit: 8,
  });
  for (const hit of autumn.hits) {
    expect(hit.plant.renderedPhenotype).toBe(hit.plant.phenotype);
  }
  await engine.call("set-time-of-day", { json: { month: 6, day: 15 } });

  const TICKS_PER_METER = 4096;
  const trunkRects = runtimePlants.hits.map(({ plant }) => ({
    minX: Number(plant.bounds.minTicks[0]) / TICKS_PER_METER,
    maxX: Number(plant.bounds.maxTicksExclusive[0]) / TICKS_PER_METER,
    minZ: Number(plant.bounds.minTicks[2]) / TICKS_PER_METER,
    maxZ: Number(plant.bounds.maxTicksExclusive[2]) / TICKS_PER_METER,
  }));
  const clearOfTrunks = (x: number, z: number) =>
    trunkRects.every(
      (rect) =>
        x < rect.minX - 0.5 || x > rect.maxX + 0.5 || z < rect.minZ - 0.5 || z > rect.maxZ + 0.5,
    );
  // Micro density follows the community blend, so the dense texels ring the
  // trunks: sample just outside each trunk's bounds first, then a coarse grid.
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
  let microPick: { hit: boolean; kind?: string; plant?: string; position?: number[] } = {
    hit: false,
  };
  let attempts = 0;
  for (const spot of spots) {
    if (attempts >= 60) {
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
    microPick = await engine.call("pick", { u: 0.5, v: 0.5 });
    if (microPick.kind === "micro-vegetation") {
      break;
    }
  }
  expect(microPick.hit).toBe(true);
  expect(microPick.kind).toBe("micro-vegetation");
  expect(microPick.plant).toBeUndefined();
  const microPoint = microPick.position ?? [];
  expect(microPoint).toHaveLength(3);
  expect(microPoint[1]).toBeCloseTo(0, 3);
  expect(microPoint[0]).toBeGreaterThanOrEqual(0);
  expect(microPoint[0]).toBeLessThan(64);
  expect(microPoint[2]).toBeGreaterThanOrEqual(0);
  expect(microPoint[2]).toBeLessThan(64);

  // Reload stability: disable and re-enable the field, forcing the runtime world
  // to rebuild from the cooked artifacts. Macro selection returns the identical
  // stable PlantId, the inspect resolves it, and micro blades reconstruct.
  await engine.call("set-component", {
    entity: vegetationWorld.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: false },
  });
  await engine.settle(300);
  await engine.call("set-component", {
    entity: vegetationWorld.id,
    component: "VegetationField",
    json: { map: fixture.map, enabled: true },
  });
  {
    const deadline = Date.now() + 30_000;
    for (;;) {
      let resident = 0;
      let microTiles = 0;
      try {
        const residentCell = await engine.call<VegetationRuntimeCellResult>(
          "vegetation-runtime-cell",
          { cell: CELL },
        );
        resident = Number(residentCell.macroPlants);
        microTiles = Number(residentCell.microTiles);
      } catch {
        // The cell is not resident yet.
      }
      if (resident >= expectedPlants && microTiles > 0) {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the reloaded vegetation cell");
      }
      await engine.settle(50);
    }
  }
  await engine.call("set-camera", {
    position: { x: 16, y: 4, z: 30 },
    yaw: 0,
    pitch: 0,
  });
  await engine.settle(200);
  const repicked = await engine.call<{ hit: boolean; kind?: string; plant?: string }>("pick", {
    u: 0.5,
    v: 0.5,
  });
  expect(repicked.kind).toBe("vegetation");
  expect(repicked.plant).toBe(picked.plant);
  const reinspected = await engine.call<{
    plant: string;
    resident?: { family: string } | null;
  }>("vegetation-runtime-inspect", { plant: repicked.plant });
  expect(reinspected.plant).toBe(repicked.plant);
  expect(reinspected.resident).toBeTruthy();

  // Editing a generated plant writes a typed mutation through the reducer: the
  // tombstone removes the picked plant persistently — the runtime row disappears
  // and the same viewport ray no longer returns its identity.
  const beforeTombstone = await engine.call<VegetationRuntimeCellResult>(
    "vegetation-runtime-cell",
    { cell: CELL },
  );
  const mutate = await engine.call<{ applied: number }>("vegetation-mutate", {
    records: [
      {
        header: {
          cell: CELL,
          transaction: "1".padStart(32, "0"),
          authority: "e".padStart(32, "0"),
          logicalTick: "1",
          idempotencyKey: "a".padStart(32, "0"),
          baseRevision: null,
        },
        mutation: { kind: "tombstone", plant: repicked.plant },
      },
    ],
  });
  expect(mutate.applied).toBe(1);
  {
    const deadline = Date.now() + 15_000;
    for (;;) {
      const after = await engine.call<VegetationRuntimeCellResult>("vegetation-runtime-cell", {
        cell: CELL,
      });
      if (Number(after.macroPlants) === Number(beforeTombstone.macroPlants) - 1) {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the tombstoned plant to leave the cell");
      }
      await engine.settle(50);
    }
  }
  const tombstoned = await engine.call<{
    plant: string;
    resident?: { family: string } | null;
  }>("vegetation-runtime-inspect", { plant: repicked.plant });
  expect(tombstoned.resident ?? null).toBeNull();
  await engine.settle(200);
  const postPick = await engine.call<{ hit: boolean; kind?: string; plant?: string }>("pick", {
    u: 0.5,
    v: 0.5,
  });
  expect(postPick.plant === repicked.plant).toBe(false);

  // Anchor planting (the editor Single/Anchor tool's exact payload): an
  // explicit-namespace identity anchors at a ground position and becomes a
  // resident macro plant.
  const anchorId = `4${"c".repeat(31)}`;
  const anchorTicks = (meters: number) => String(meters * 4096);
  const anchor = await engine.call<{ applied: number }>("vegetation-mutate", {
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
            localPosition: [40 * 4096, 0, 40 * 4096],
            orientation: [0, 0, 0, 32767],
            scaleBits: [65536, 65536, 65536],
            bounds: {
              minTicks: [anchorTicks(32), anchorTicks(-1), anchorTicks(32)],
              maxTicksExclusive: [anchorTicks(48), anchorTicks(16), anchorTicks(48)],
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
  {
    const deadline = Date.now() + 15_000;
    for (;;) {
      const after = await engine.call<VegetationRuntimeCellResult>("vegetation-runtime-cell", {
        cell: CELL,
      });
      if (Number(after.macroPlants) === Number(beforeTombstone.macroPlants)) {
        break;
      }
      if (Date.now() >= deadline) {
        throw new Error("timeout waiting for the anchored plant to join the cell");
      }
      await engine.settle(50);
    }
  }
  const anchored = await engine.call<{
    plant: string;
    resident?: { family: string } | null;
  }>("vegetation-runtime-inspect", { plant: anchorId });
  expect(anchored.resident).toBeTruthy();
  expect(anchored.resident?.family).toBe(fixture.plant);

  // The brush-gesture wire: an authored AnchorOverride chunk commits into the map
  // package as one optimistic transaction, and the next cook folds the authored
  // anchor into the cell — macroCount rises above the procedural base.
  {
    interface MapSummary {
      summary: { kind: string; asset: { generation: string } };
    }
    const summary = await engine.call<MapSummary>("vegetation-asset-summary", {
      asset: fixture.map,
    });
    const chunkAnchorId = `5${"d".repeat(31)}`;
    const ticks = (meters: number) => String(meters * 4096);
    const committed = await engine.call<{ generation: string }>("vegetation-map-chunk-commit", {
      map: fixture.map,
      expectedGeneration: summary.summary.asset.generation,
      upserts: [
        {
          key: {
            layer: fixture.authoredLayer,
            tile: { kind: "cell", cell: CELL },
            kind: "anchor-override",
          },
          revision: "1",
          payload: {
            kind: "anchor-override",
            explicitPlants: [
              {
                id: chunkAnchorId,
                layer: fixture.authoredLayer,
                family: fixture.plant,
                point: {
                  id: chunkAnchorId,
                  owner: CELL,
                  localPosition: [20 * 4096, 0, 20 * 4096],
                  orientation: [0, 0, 0, 32767],
                  scaleBits: [65536, 65536, 65536],
                  bounds: {
                    minTicks: [ticks(12), ticks(-1), ticks(12)],
                    maxTicksExclusive: [ticks(28), ticks(16), ticks(28)],
                  },
                  family: fixture.plant,
                  variation: 0,
                  lifecycle: "mature",
                  phenotype: 0,
                  representationClass: 0,
                  deterministicKey: "f".padStart(32, "0"),
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
            ],
            pins: [],
            transformOverrides: [],
            stateOverrides: [],
          },
        },
      ],
      removals: [],
    });
    expect(BigInt(committed.generation)).toBeGreaterThan(
      BigInt(summary.summary.asset.generation),
    );
    // The read side of the brush wire: the committed chunk reads back by its logical
    // key with the round-tripped anchor row, and an absent key contributes no row.
    {
      interface ChunkRead {
        generation: string;
        chunks: {
          key: { layer: string; kind: string };
          revision: string;
          payload: {
            kind: string;
            explicitPlants: { id: string; family: string; point: { id: string } }[];
          };
        }[];
      }
      const readBack = await engine.call<ChunkRead>("vegetation-map-chunk-read", {
        map: fixture.map,
        keys: [
          {
            layer: fixture.authoredLayer,
            tile: { kind: "cell", cell: CELL },
            kind: "anchor-override",
          },
          {
            layer: fixture.authoredLayer,
            tile: { kind: "global" },
            kind: "field",
          },
        ],
      });
      expect(readBack.generation).toBe(committed.generation);
      expect(readBack.chunks).toHaveLength(1);
      const chunk = readBack.chunks[0]!;
      expect(chunk.key.kind).toBe("anchor-override");
      expect(chunk.revision).toBe("1");
      expect(chunk.payload.kind).toBe("anchor-override");
      expect(chunk.payload.explicitPlants).toHaveLength(1);
      expect(chunk.payload.explicitPlants[0]!.id).toBe(chunkAnchorId);
      expect(chunk.payload.explicitPlants[0]!.family).toBe(fixture.plant);
      expect(chunk.payload.explicitPlants[0]!.point.id).toBe(chunkAnchorId);
    }
    // The committed chunk diverges from what the current manifest consumed: the
    // authored layer reads dirty until the recook below folds it in.
    {
      interface DirtySummary {
        summary: { kind: string; asset: { dirtyLayers: string[] } };
      }
      const dirty = await engine.call<DirtySummary>("vegetation-asset-summary", {
        asset: fixture.map,
      });
      expect(dirty.summary.asset.dirtyLayers).toContain(fixture.authoredLayer);
    }
    const recook = await engine.call<VegetationCookJobDto>("vegetation-cook", {
      map: fixture.map,
      scope: { kind: "cells", cells: [CELL] },
      workers: 1,
    });
    const recooked = await awaitCook(recook.job);
    // The committed chunk participates in the cook identity (a fresh manifest), but
    // anchors only become plants through an ExplicitAnchors graph node — this
    // fixture's graph has none, so the macro count stays the procedural base.
    expect(recooked.manifest!.identity).not.toBe(cooked.manifest!.identity);
    expect(Number(recooked.manifest!.cells[0]!.macroCount)).toBe(
      Number(fixture.expectedAccepted),
    );
    // The recook consumed the committed chunk: the authored layer reads clean.
    {
      interface DirtySummary {
        summary: { kind: string; asset: { dirtyLayers: string[] } };
      }
      const clean = await engine.call<DirtySummary>("vegetation-asset-summary", {
        asset: fixture.map,
      });
      expect(clean.summary.asset.dirtyLayers).not.toContain(fixture.authoredLayer);
    }
    // The topology diff between the two manifests: the committed anchor chunk
    // re-keyed the cell's artifact without changing its macro population, so the
    // cell reports zero churn — and the authored anchor that no ExplicitAnchors
    // node consumed surfaces as an unresolved conflict.
    {
      interface TopologyDiff {
        from: string;
        to: string;
        cells: {
          added: string;
          removed: string;
          moved: string;
          conflicts: { kind: string; plant: string; layer: string }[];
        }[];
      }
      const diff = await engine.call<TopologyDiff>("vegetation-topology-diff", {
        map: fixture.map,
        from: cooked.manifest!.identity,
        to: recooked.manifest!.identity,
        cells: [CELL],
      });
      expect(diff.from).toBe(cooked.manifest!.identity);
      expect(diff.to).toBe(recooked.manifest!.identity);
      expect(diff.cells).toHaveLength(1);
      const cellDiff = diff.cells[0]!;
      expect(cellDiff.added).toBe("0");
      expect(cellDiff.removed).toBe("0");
      expect(cellDiff.moved).toBe("0");
      expect(
        cellDiff.conflicts.some(
          (conflict) => conflict.kind === "anchor" && conflict.plant === chunkAnchorId,
        ),
      ).toBe(true);
    }
    // The pin gesture's wire: toggling a plant id in the chunk's pins list is one
    // read-modify-write transaction that preserves the rest of the payload.
    {
      interface PinRead {
        generation: string;
        chunks: {
          revision: string;
          payload: { kind: string; explicitPlants: unknown[]; pins: string[] };
        }[];
      }
      const key = {
        layer: fixture.authoredLayer,
        tile: { kind: "cell", cell: CELL },
        kind: "anchor-override",
      };
      const before = await engine.call<PinRead>("vegetation-map-chunk-read", {
        map: fixture.map,
        keys: [key],
      });
      const chunk = before.chunks[0]!;
      expect(chunk.payload.pins).toHaveLength(0);
      await engine.call("vegetation-map-chunk-commit", {
        map: fixture.map,
        expectedGeneration: before.generation,
        upserts: [
          {
            key,
            revision: (BigInt(chunk.revision) + 1n).toString(),
            payload: { ...chunk.payload, pins: [chunkAnchorId] },
          },
        ],
        removals: [],
      });
      const pinned = await engine.call<PinRead>("vegetation-map-chunk-read", {
        map: fixture.map,
        keys: [key],
      });
      expect(pinned.chunks[0]!.payload.pins).toEqual([chunkAnchorId]);
      expect(pinned.chunks[0]!.payload.explicitPlants).toHaveLength(1);
      await engine.call("vegetation-map-chunk-commit", {
        map: fixture.map,
        expectedGeneration: pinned.generation,
        upserts: [
          {
            key,
            revision: (BigInt(pinned.chunks[0]!.revision) + 1n).toString(),
            payload: { ...pinned.chunks[0]!.payload, pins: [] },
          },
        ],
        removals: [],
      });
    }
  }
  {
    const deadline = Date.now() + 15_000;
    let jiggle = 0;
    let stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
    while (Date.now() < deadline) {
      jiggle += 1;
      await engine.call("set-camera", {
        position: { x: 32, y: 6 + (jiggle % 2) * 0.01, z: 44 },
        yaw: 0,
        pitch: -5,
      });
      stats = await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
      if (stats.visibility.microCandidates > 0) {
        break;
      }
      await engine.settle(50);
    }
    expect(stats.visibility.microCandidates).toBeGreaterThan(0);
    expect(stats.visibility.overflowFlags).toBe(0);
  }

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

  // A plant family previews as its compiled renderable form: enter frames a real
  // subject (positive distance) and reports the authored combination domain; a
  // combination scrub re-renders validation-clean.
  {
    const preview = await engine.call<{
      rootEntity: string;
      distance: number;
      plantCombinations?: { variation: number; phenotype: number }[];
    }>("enter-asset-preview", { asset: fixture.plant });
    expect(preview.rootEntity).not.toBe("0");
    expect(preview.distance).toBeGreaterThan(0);
    await engine.settle(300);
    // This family authors no combination masks, so the scrub resolves to the first
    // authored combination — the options path itself must still apply cleanly.
    const combination = preview.plantCombinations?.[0] ?? { variation: 0, phenotype: 0 };
    await engine.call("set-asset-preview-options", {
      variation: combination.variation,
      phenotype: combination.phenotype,
    });
    await engine.settle(300);
    await engine.call("exit-asset-preview", {});
  }

  // The explicit surface-ray query (the brush's straight-down projection): a cast
  // from above a spawned cube hits its top face with an upward geometric normal; an
  // upward cast from the same origin misses.
  {
    const cube = await engine.call<{ id: string }>("add-entity", { preset: "cube" });
    await engine.call("set-transform", {
      entity: cube.id,
      translation: { x: 500, y: 0, z: 500 },
    });
    await engine.settle(100);
    const cast = await engine.call<{
      hit: boolean;
      position?: [number, number, number];
      normal?: [number, number, number];
    }>("query-surface-ray", { originM: [500, 100, 500], direction: [0, -1, 0] });
    expect(cast.hit).toBe(true);
    expect(cast.position![1]).toBeLessThan(100);
    expect(cast.normal![1]).toBeGreaterThan(0.5);
    const miss = await engine.call<{ hit: boolean }>("query-surface-ray", {
      originM: [500, 100, 500],
      direction: [0, 1, 0],
    });
    expect(miss.hit).toBe(false);
    await engine.call("destroy-entity", { entity: cube.id });
  }

  // Rejected candidates carry their sampled world position in the rejection facet:
  // the read reports counts and per-row position/reason/ordinal.
  {
    interface Rejections {
      candidates: string;
      accepted: string;
      totalRejected: string;
      rows: { reason: string; positionTicks: [string, string, string]; ordinal: string }[];
    }
    const rejections = await engine.call<Rejections>("vegetation-rejections", {
      map: fixture.map,
      cell: CELL,
    });
    expect(Number(rejections.candidates)).toBeGreaterThan(0);
    expect(Number(rejections.accepted)).toBe(Number(fixture.expectedAccepted));
    expect(rejections.rows.length).toBeLessThanOrEqual(Number(rejections.totalRejected));
    for (const row of rejections.rows) {
      expect(typeof row.reason).toBe("string");
      for (const ticks of row.positionTicks) {
        BigInt(ticks);
      }
    }
    // The viewport diagnostics agree with the sa cook statistics: this manifest
    // covers exactly the one cooked cell, so the summary's per-reason rejection
    // totals sum to the cell's rejected-row count.
    interface CookStats {
      summary: {
        kind: string;
        asset: { latestCook?: { rejections: { reason: string; count: string }[] } };
      };
    }
    const stats = await engine.call<CookStats>("vegetation-asset-summary", {
      asset: fixture.map,
    });
    const statTotal = (stats.summary.asset.latestCook?.rejections ?? []).reduce(
      (sum, row) => sum + Number(row.count),
      0,
    );
    expect(statTotal).toBe(Number(rejections.totalRejected));
  }

  // The vegetation debug overlays: resident cell boxes + lifecycle-colored plant
  // bounds render validation-clean over the live world and echo their flags.
  {
    interface OverlayFlags {
      vegetationCells: boolean;
      vegetationBounds: boolean;
      vegetationRejections: boolean;
      vegetationHeatmap: boolean;
    }
    const overlays = await engine.call<OverlayFlags>("set-debug-overlays", {
      vegetationCells: true,
      vegetationBounds: true,
      vegetationRejections: true,
      vegetationHeatmap: true,
    });
    expect(overlays.vegetationCells).toBe(true);
    expect(overlays.vegetationBounds).toBe(true);
    expect(overlays.vegetationRejections).toBe(true);
    expect(overlays.vegetationHeatmap).toBe(true);
    await engine.settle(300);
    const cleared = await engine.call<OverlayFlags>("set-debug-overlays", {
      vegetationCells: false,
      vegetationBounds: false,
      vegetationRejections: false,
      vegetationHeatmap: false,
    });
    expect(cleared.vegetationCells).toBe(false);
    expect(cleared.vegetationBounds).toBe(false);
    expect(cleared.vegetationRejections).toBe(false);
    expect(cleared.vegetationHeatmap).toBe(false);
  }

  // The plant family thumbnail renders its compiled form through the main graph
  // (content-hash cached, like a mesh/model tile).
  {
    const thumb = await engine.getThumbnail<{ base64: string; format: string }>("get-thumbnail", {
      asset: fixture.plant,
      size: 96,
    });
    expect(thumb.format).toBe("png");
    expect(thumb.base64.startsWith("iVBORw0KGgo")).toBe(true);
  }

  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);
