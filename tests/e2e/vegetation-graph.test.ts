// Vegetation acceptance through the real host: canonical package import, compiled evaluation,
// retained provenance, asynchronous cooking, manifest publication, cell inspection, and the live
// runtime the editor drives. One ordered run over one booted engine — the steps live in the
// `vegetation-graph-*` modules beside this file.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import { authoredAssets, loadFixture, type VegetationFixture } from "./vegetation-utils.ts";
import {
  cancelPreparedEvaluation,
  bindWorld,
  commitLayerTransaction,
  compileBiome,
  evaluateRegion,
  explainAcceptedPlant,
  importPackage,
} from "./vegetation-graph-package.ts";
import {
  awaitLiveScene,
  awaitMicroCandidates,
  commitBrushChunk,
  cookCanonicalCell,
  inspectCookedCell,
  readManifest,
  readRenderStats,
  recookAcrossWorkerCounts,
} from "./vegetation-graph-cook.ts";
import {
  assertPhenologyHoldsAcrossSeasons,
  pickGroundClearOfTrunks,
  pickMacroPlant,
  plantAnchor,
  readTrunkRects,
  reloadField,
  tombstonePickedPlant,
} from "./vegetation-graph-runtime.ts";
import {
  castSurfaceRay,
  previewPlantFamily,
  readRejections,
  renderFamilyThumbnail,
  toggleDebugOverlays,
} from "./vegetation-graph-diagnostics.ts";

const cleaner = new Cleaner();
let engine: Engine;
let fixture: VegetationFixture;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  fixture = loadFixture("vegetation-phase3");
  expect(fixture.formatVersion).toBe(4);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("canonical vegetation package evaluates, explains, cooks, and inspects one cell", async () => {
  const sources = authoredAssets(cleaner, fixture, "phase3");
  await importPackage(engine, fixture, sources);
  await commitLayerTransaction(engine, fixture);

  const world = await bindWorld(engine, cleaner, fixture);
  const compiled = await compileBiome(engine, fixture);
  const { job } = await evaluateRegion(engine, fixture, compiled);
  await explainAcceptedPlant(engine, fixture, job);

  const cooked = await cookCanonicalCell(engine, fixture);
  await readManifest(engine, fixture, cooked);
  await inspectCookedCell(engine, fixture, cooked);
  const settled = await recookAcrossWorkerCounts(engine, fixture);

  const expectedPlants = Number(fixture.expectedAccepted);
  await awaitLiveScene(engine, expectedPlants);
  await readRenderStats(engine, expectedPlants);

  const picked = await pickMacroPlant(engine);
  const trunkRects = await readTrunkRects(engine);
  await assertPhenologyHoldsAcrossSeasons(engine);
  await pickGroundClearOfTrunks(engine, trunkRects);

  const repicked = await reloadField(engine, fixture, world, expectedPlants, picked);
  const baselinePlants = await tombstonePickedPlant(engine, repicked);
  await plantAnchor(engine, fixture, baselinePlants);

  await commitBrushChunk(engine, fixture, settled);
  await awaitMicroCandidates(engine);
  await cancelPreparedEvaluation(engine, fixture);

  await previewPlantFamily(engine, fixture);
  await castSurfaceRay(engine);
  await readRejections(engine, fixture);
  await toggleDebugOverlays(engine);
  await renderFamilyThumbnail(engine, fixture);

  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
}, 120_000);
