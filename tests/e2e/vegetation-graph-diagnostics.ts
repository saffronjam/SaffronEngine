// The diagnostic surfaces of the vegetation acceptance run: the family preview, the explicit
// surface-ray query the brush projects with, rejection reporting, the debug overlays, and the
// family thumbnail.

import { expect } from "bun:test";
import type { Engine } from "./harness.ts";
import { CELL, vegetationMap, type VegetationFixture } from "./vegetation-utils.ts";

// A plant family previews as its compiled renderable form: enter frames a real subject
// (positive distance) and reports the authored combination domain; a combination scrub
// re-renders validation-clean.
export async function previewPlantFamily(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<void> {
  const preview = await engine.call("enter-asset-preview", { asset: fixture.plant });
  expect(preview.rootEntity).not.toBe("0");
  expect(preview.distance).toBeGreaterThan(0);
  await engine.settle(300);
  // This family authors no combination masks, so the scrub resolves to the first authored
  // combination — the options path itself must still apply cleanly.
  const combination = preview.plantCombinations?.[0] ?? { variation: 0, phenotype: 0 };
  await engine.call("set-asset-preview-options", {
    variation: combination.variation,
    phenotype: combination.phenotype,
  });
  await engine.settle(300);
  await engine.call("exit-asset-preview", {});
}

// The explicit surface-ray query (the brush's straight-down projection): a cast from above a
// spawned cube hits its top face with an upward geometric normal; an upward cast from the same
// origin misses.
export async function castSurfaceRay(engine: Engine): Promise<void> {
  const cube = await engine.call("add-entity", { preset: "cube" });
  await engine.call("set-transform", {
    entity: cube.id,
    translation: { x: 500, y: 0, z: 500 },
  });
  await engine.settle(100);
  const cast = await engine.call("query-surface-ray", {
    originM: [500, 100, 500],
    direction: [0, -1, 0],
  });
  expect(cast.hit).toBe(true);
  expect(cast.position![1]).toBeLessThan(100);
  expect(cast.normal![1]).toBeGreaterThan(0.5);
  const miss = await engine.call("query-surface-ray", {
    originM: [500, 100, 500],
    direction: [0, 1, 0],
  });
  expect(miss.hit).toBe(false);
  await engine.call("destroy-entity", { entity: cube.id });
}

// Rejected candidates carry their sampled world position in the rejection facet: the read
// reports counts and per-row position/reason/ordinal, and the viewport diagnostics agree with
// the cook statistics — this manifest covers exactly the one cooked cell, so the summary's
// per-reason totals sum to the cell's rejected-row count.
export async function readRejections(engine: Engine, fixture: VegetationFixture): Promise<void> {
  const rejections = await engine.call("vegetation-rejections", {
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

  const stats = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  const statTotal = (vegetationMap(stats).latestCook?.rejections ?? []).reduce(
    (sum, row) => sum + Number(row.count),
    0,
  );
  expect(statTotal).toBe(Number(rejections.totalRejected));
}

// The vegetation debug overlays: resident cell boxes + lifecycle-colored plant bounds render
// validation-clean over the live world and echo their flags.
export async function toggleDebugOverlays(engine: Engine): Promise<void> {
  const flags = (value: boolean) => ({
    vegetationCells: value,
    vegetationBounds: value,
    vegetationRejections: value,
    vegetationHeatmap: value,
  });
  const overlays = await engine.call("set-debug-overlays", flags(true));
  expect(overlays).toMatchObject(flags(true));
  await engine.settle(300);
  const cleared = await engine.call("set-debug-overlays", flags(false));
  expect(cleared).toMatchObject(flags(false));
}

// The plant family thumbnail renders its compiled form through the main graph (content-hash
// cached, like a mesh/model tile).
export async function renderFamilyThumbnail(
  engine: Engine,
  fixture: VegetationFixture,
): Promise<void> {
  const thumb = await engine.getThumbnail("get-thumbnail", {
    asset: fixture.plant,
    size: 96,
  });
  expect(thumb.format).toBe("png");
  expect(thumb.base64.startsWith("iVBORw0KGgo")).toBe(true);
}
