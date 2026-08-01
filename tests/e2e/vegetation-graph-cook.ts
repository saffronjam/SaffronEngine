// The cooking half of the vegetation acceptance run: publication of a manifest, cell
// inspection, the runtime going live over the GPU scene, and the authored-chunk brush wire
// that a recook folds in.

import { expect } from "bun:test";
import type {
  GpuSceneMirrorStatsDto,
  VegetationCookStatusDto,
  VegetationMapChunkKeyDto,
  VegetationMapChunkPayloadDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import {
  awaitCook,
  awaitResidentCell,
  CELL,
  cookCells,
  TICKS_PER_METER,
  type VegetationFixture,
  vegetationMap,
} from "./vegetation-utils.ts";

// The camera framing that puts the cooked cell in view; the residency polls nudge it so the
// reactive loop keeps rendering while pages stream and the counters read back.
const OVERVIEW = { position: { x: 32, y: 6, z: 44 }, yaw: 0, pitch: -5 } as const;

export async function cookCanonicalCell(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<VegetationCookStatusDto> {
  const cooked = await cookCells(engine, fixture.map);
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
  return cooked;
}

// The published manifest is what a later read resolves to, artifact hashes and all.
export async function readManifest(
  engine: Engine,
  fixture: VegetationFixture,
  cooked: VegetationCookStatusDto,
): Promise<void> {
  const manifest = await engine.call("vegetation-manifest", {
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
}

// Scheduling never reaches published bytes. The first cook commits the plant source observation
// back to the authored family, so its generation is the one that settles; from there the same scope
// cooked on four workers and then on two lands on the same generation identity, the same artifacts,
// and every node satisfied from the store.
export async function recookAcrossWorkerCounts(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<VegetationCookStatusDto> {
  const cook = async (workers: number) => {
    const started = await engine.call("vegetation-cook", {
      map: fixture.map,
      scope: { kind: "cells", cells: [CELL] },
      workers,
    });
    return awaitCook(engine, started.job);
  };
  const settled = await cook(4);
  const repeated = await cook(2);
  expect(repeated.manifest!.identity).toBe(settled.manifest!.identity);
  expect(repeated.manifest!.cells.map((cell) => cell.artifactHash)).toEqual(
    settled.manifest!.cells.map((cell) => cell.artifactHash),
  );
  expect(repeated.manifest!.plants.map((plant) => plant.artifactHash)).toEqual(
    settled.manifest!.plants.map((plant) => plant.artifactHash),
  );
  expect(repeated.manifest!.dependencies).toEqual(settled.manifest!.dependencies);
  expect(repeated.manifest!.cells[0]!.actual.cacheHit).toBe(true);
  expect(repeated.statistics!.nodes).toBe(settled.statistics!.nodes);
  expect(repeated.statistics!.cacheHits).toBe(settled.statistics!.nodes);
  expect(repeated.statistics!.cacheMisses).toBe("0");
  expect(repeated.statistics!.publishedCells).toBe("1");
  return repeated;
}

export async function inspectCookedCell(
  engine: Engine,
  fixture: VegetationFixture,
  cooked: VegetationCookStatusDto,
): Promise<void> {
  const inspected = await engine.call("vegetation-cell-inspect", {
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
}

// Polls `gpu-scene-stats` while nudging the camera, until `ready` holds or the deadline passes.
async function pollSceneStats(
  engine: Engine,
  ready: (stats: GpuSceneMirrorStatsDto) => boolean,
  timeoutMs: number,
): Promise<GpuSceneMirrorStatsDto> {
  const deadline = Date.now() + timeoutMs;
  let stats = await engine.call("gpu-scene-stats");
  let jiggle = 0;
  while (Date.now() < deadline) {
    jiggle += 1;
    await engine.call("set-camera", {
      ...OVERVIEW,
      position: { ...OVERVIEW.position, y: OVERVIEW.position.y + (jiggle % 2) * 0.01 },
    });
    stats = await engine.call("gpu-scene-stats");
    if (ready(stats)) {
      break;
    }
    await engine.settle(50);
  }
  return stats;
}

// The cooked map goes live: the runtime binds the manifest, streams cell (0,0,0) resident off
// the editor camera's spatial source, and the GPU-scene mirror translates its macro plants into
// persistent-scene instances whose family pages stream resident and whose traversal emits
// draw records.
export async function awaitLiveScene(engine: Engine, expectedPlants: number): Promise<void> {
  await engine.call("set-camera", OVERVIEW);
  await awaitResidentCell(engine, expectedPlants, { microTiles: true });

  const stats = await pollSceneStats(
    engine,
    (s) =>
      s.instances >= expectedPlants &&
      s.pageResidency.resident > 0 &&
      s.visibility.records > 0 &&
      s.visibility.microCandidates > 0,
    30_000,
  );
  expect(stats.instances).toBeGreaterThanOrEqual(expectedPlants);
  expect(stats.meshes).toBeGreaterThanOrEqual(1);
  expect(stats.pageResidency.resident).toBeGreaterThan(0);
  expect(stats.visibility.records).toBeGreaterThan(0);
  // Micro reconstruction is live: blades generated from the resident field tiles, appended to
  // the same record stream.
  expect(stats.visibility.microCandidates).toBeGreaterThan(0);
  expect(stats.visibility.records).toBeGreaterThan(stats.visibility.microCandidates - 1);
  // The predicted tile budget is the cooked density upper bound: the per-frame generated count
  // (view-culled) never exceeds it.
  expect(Number(stats.microPredicted)).toBeGreaterThan(0);
  expect(Number(stats.microPredicted)).toBeGreaterThanOrEqual(stats.visibility.microCandidates);
  expect(stats.visibility.overflowFlags).toBe(0);
}

// Micro reconstruction survives the authored-chunk edits above it.
export async function awaitMicroCandidates(engine: Engine): Promise<void> {
  const stats = await pollSceneStats(engine, (s) => s.visibility.microCandidates > 0, 15_000);
  expect(stats.visibility.microCandidates).toBeGreaterThan(0);
  expect(stats.visibility.overflowFlags).toBe(0);
}

// The per-family/per-cell telemetry matrix: the birch family rows up with its resident plants,
// field tiles, and predicted budget, and the resident cell reports its population.
export async function readRenderStats(engine: Engine, expectedPlants: number): Promise<void> {
  const renderStats = await engine.call("vegetation-render-stats");
  expect(renderStats.families.length).toBeGreaterThan(0);
  const populated = renderStats.families.find((row) => row.instances > 0);
  expect(populated).toBeDefined();
  expect(populated?.fieldTiles).toBeGreaterThan(0);
  expect(Number(populated?.microPredicted)).toBeGreaterThan(0);
  const residentCell = renderStats.cells.find(
    (row) => row.cell.level === 0 && row.cell.coordinates.every((coordinate) => coordinate === "0"),
  );
  expect(residentCell).toBeDefined();
  expect(residentCell?.plants).toBeGreaterThanOrEqual(expectedPlants);
  expect(Number(renderStats.pageFaults)).toBeGreaterThanOrEqual(0);
}

// The anchor-override leg of a chunk payload; a field chunk here means the read answered with the
// wrong chunk.
function anchorOverride(
  payload: VegetationMapChunkPayloadDto,
): Extract<VegetationMapChunkPayloadDto, { kind: "anchor-override" }> {
  if (payload.kind !== "anchor-override") {
    throw new Error(`the chunk carries a ${payload.kind} payload, not an anchor override`);
  }
  return payload;
}

function anchorChunkKey(fixture: VegetationFixture): VegetationMapChunkKeyDto {
  return {
    layer: fixture.authoredLayer,
    tile: { kind: "cell", cell: CELL },
    kind: "anchor-override",
  };
}

function anchorOverridePayload(
  fixture: VegetationFixture,
  anchorId: string,
): VegetationMapChunkPayloadDto {
  const ticks = (meters: number) => String(meters * TICKS_PER_METER);
  return {
    kind: "anchor-override",
    explicitPlants: [
      {
        id: anchorId,
        layer: fixture.authoredLayer,
        family: fixture.plant,
        point: {
          id: anchorId,
          owner: CELL,
          localPosition: [20 * TICKS_PER_METER, 0, 20 * TICKS_PER_METER],
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
  };
}

// The brush-gesture wire: an authored AnchorOverride chunk commits into the map package as one
// optimistic transaction, and the next cook folds the authored anchor into the cell.
export async function commitBrushChunk(
  engine: Engine,
  fixture: VegetationFixture,
  cooked: VegetationCookStatusDto,
): Promise<void> {
  const summary = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  const chunkAnchorId = `5${"d".repeat(31)}`;
  const key = anchorChunkKey(fixture);
  const committed = await engine.call("vegetation-map-chunk-commit", {
    map: fixture.map,
    expectedGeneration: vegetationMap(summary).generation,
    upserts: [{ key, revision: "1", payload: anchorOverridePayload(fixture, chunkAnchorId) }],
    removals: [],
  });
  expect(BigInt(committed.generation)).toBeGreaterThan(BigInt(vegetationMap(summary).generation));

  // The read side of the brush wire: the committed chunk reads back by its logical key with the
  // round-tripped anchor row, and an absent key contributes no row.
  const readBack = await engine.call("vegetation-map-chunk-read", {
    map: fixture.map,
    keys: [key, { layer: fixture.authoredLayer, tile: { kind: "global" }, kind: "field" }],
  });
  expect(readBack.generation).toBe(committed.generation);
  expect(readBack.chunks).toHaveLength(1);
  const chunk = readBack.chunks[0]!;
  expect(chunk.key.kind).toBe("anchor-override");
  expect(chunk.revision).toBe("1");
  const anchors = anchorOverride(chunk.payload);
  expect(anchors.explicitPlants).toHaveLength(1);
  expect(anchors.explicitPlants[0]!.id).toBe(chunkAnchorId);
  expect(anchors.explicitPlants[0]!.family).toBe(fixture.plant);
  expect(anchors.explicitPlants[0]!.point.id).toBe(chunkAnchorId);

  // The committed chunk diverges from what the current manifest consumed: the authored layer
  // reads dirty until the recook below folds it in.
  const dirty = await engine.call("vegetation-asset-summary", {
    asset: fixture.map,
  });
  expect(vegetationMap(dirty).dirtyLayers).toContain(fixture.authoredLayer);

  const recook = await engine.call("vegetation-cook", {
    map: fixture.map,
    scope: { kind: "cells", cells: [CELL] },
    workers: 1,
  });
  const recooked = await awaitCook(engine, recook.job);
  // The committed chunk participates in the cook identity (a fresh manifest), but anchors only
  // become plants through an ExplicitAnchors graph node — this fixture's graph has none, so the
  // macro count stays the procedural base.
  expect(recooked.manifest!.identity).not.toBe(cooked.manifest!.identity);
  expect(Number(recooked.manifest!.cells[0]!.macroCount)).toBe(Number(fixture.expectedAccepted));

  // The edit's dependency region is the cell that owns the chunk, and nothing wider: the cell
  // republished, while the compiled family — which no authored chunk feeds — came back from the
  // store untouched.
  expect(recooked.manifest!.cells[0]!.actual.cacheHit).toBe(false);
  expect(recooked.manifest!.cells[0]!.artifactHash).not.toBe(
    cooked.manifest!.cells[0]!.artifactHash,
  );
  expect(recooked.manifest!.plants[0]!.artifactHash).toBe(cooked.manifest!.plants[0]!.artifactHash);
  expect(recooked.statistics).toMatchObject({ cacheHits: "1", cacheMisses: "1" });

  const clean = await engine.call("vegetation-asset-summary", {
    asset: fixture.map,
  });
  expect(vegetationMap(clean).dirtyLayers).not.toContain(fixture.authoredLayer);

  await assertTopologyDiff(engine, fixture, cooked, recooked, chunkAnchorId);
  await togglePin(engine, fixture, chunkAnchorId);
}

// The topology diff between the two manifests: the committed anchor chunk re-keyed the cell's
// artifact without changing its macro population, so the cell reports zero churn — and the
// authored anchor that no ExplicitAnchors node consumed surfaces as an unresolved conflict.
async function assertTopologyDiff(
  engine: Engine,
  fixture: VegetationFixture,
  cooked: VegetationCookStatusDto,
  recooked: VegetationCookStatusDto,
  chunkAnchorId: string,
): Promise<void> {
  const diff = await engine.call("vegetation-topology-diff", {
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
async function togglePin(
  engine: Engine,
  fixture: VegetationFixture,
  chunkAnchorId: string,
): Promise<void> {
  const key = anchorChunkKey(fixture);
  const before = await engine.call("vegetation-map-chunk-read", {
    map: fixture.map,
    keys: [key],
  });
  const chunk = before.chunks[0]!;
  const unpinned = anchorOverride(chunk.payload);
  expect(unpinned.pins).toHaveLength(0);
  await engine.call("vegetation-map-chunk-commit", {
    map: fixture.map,
    expectedGeneration: before.generation,
    upserts: [
      {
        key,
        revision: (BigInt(chunk.revision) + 1n).toString(),
        payload: { ...unpinned, pins: [chunkAnchorId] },
      },
    ],
    removals: [],
  });
  const pinned = await engine.call("vegetation-map-chunk-read", {
    map: fixture.map,
    keys: [key],
  });
  const repinned = anchorOverride(pinned.chunks[0]!.payload);
  expect(repinned.pins).toEqual([chunkAnchorId]);
  expect(repinned.explicitPlants).toHaveLength(1);
  await engine.call("vegetation-map-chunk-commit", {
    map: fixture.map,
    expectedGeneration: pinned.generation,
    upserts: [
      {
        key,
        revision: (BigInt(pinned.chunks[0]!.revision) + 1n).toString(),
        payload: { ...repinned, pins: [] },
      },
    ],
    removals: [],
  });
}
