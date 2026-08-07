// Preview suite for the material system: the offscreen studio-sphere thumbnail (the editor's
// material preview pane + cached Assets tiles) plus the one scene-screenshot case that shares the
// same codegen/override plumbing (normal-map perturbation). Consolidated onto a single booted host.
//
// Covers:
//   - get-thumbnail returns a PNG that reflects the material's base color (white vs red differ);
//   - a foldable constant graph folds to the same preview as a direct base-color material;
//   - a non-foldable multiply graph codegen-renders in the preview;
//   - a procedural uv/frac graph codegen-renders in the preview;
//   - a mixed-size backlog drains one tile at a time;
//   - view-asset renders the same material at its larger default;
//   - a normal map assigned to an entity perturbs the shaded scene result.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner, prepareScene, trackEntity } from "./test-utils.ts";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const suiteCleaner = new Cleaner();
const caseCleaner = new Cleaner();

beforeAll(async () => {
  engine = await bootEngine(suiteCleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    camera: { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 },
  });
  // Keep the material-equivalence captures on deterministic direct + image-based lighting. The
  // stochastic screen-space and distance-field GI passes are covered by their own render suites.
  await engine.call("set-ibl", { args: ["on"] }).catch(() => {});
  await engine.call("set-render-quality", { args: ["low"] });
  await engine.call("set-gi", { args: ["off"] });
  await engine.call("set-sky-occlusion", { args: [0] });
});
afterAll(async () => {
  await suiteCleaner.cleanup();
});
afterEach(async () => {
  await caseCleaner.cleanup();
  await engine.settle(150);
});

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, caseCleaner, `matprev-${tag}`);
}

/// The material preview the editor's pane and an Assets tile both take: one command, one
/// content-addressed cache, one converge loop.
function preview(material: string, size: number) {
  return engine.getThumbnail("get-thumbnail", { asset: material, size });
}

test("get-thumbnail returns a PNG that reflects the material's color", async () => {
  const a = await engine.call("material-create", { name: "PrevA" });
  const b = await engine.call("material-create", { name: "PrevB" });
  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await preview(a.id, 128);
  const pb = await preview(b.id, 128);

  expect(pa.format).toBe("png");
  expect(pa.width).toBe(128);
  expect(pa.height).toBe(128);
  expect(pa.base64.length).toBeGreaterThan(100);
  expect(pa.base64.startsWith("iVBORw0KGgo")).toBe(true); // PNG magic, base64
  expect(pa.base64).not.toBe(pb.base64); // white vs red sphere
  expect(engine.validationErrors()).toEqual([]);
});

test("a foldable node graph drives the material like direct factors", async () => {
  const a = await engine.call("material-create", { name: "GraphA" });
  const b = await engine.call("material-create", { name: "DirectB" });

  const graph = {
    nodes: [
      { id: "c", type: "constant", props: { value: [1, 0, 0, 1] } },
      { id: "out", type: "materialOutput" },
    ],
    edges: [{ from: ["c", "rgba"], to: ["out", "baseColor"] }],
  };
  const set = await engine.call("material-set-graph", {
    material: a.id,
    graph,
  });
  expect(set.foldable).toBe(true);

  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await preview(a.id, 128);
  const pb = await preview(b.id, 128);

  expect(pa.base64).toBe(pb.base64); // the graph folds to the same red material
  expect(engine.validationErrors()).toEqual([]);
});

test("a procedural graph renders via codegen in the preview", async () => {
  const m = await engine.call("material-create", { name: "CodegenPrev" });
  const graph = {
    nodes: [
      { id: "c1", type: "constant", props: { value: [1, 0, 0, 1] } },
      { id: "c2", type: "constant", props: { value: [0.5, 0.5, 0.5, 1] } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["mul", "a"] },
      { from: ["c2", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });

  const prev = await preview(m.id, 128);
  expect(prev.base64.startsWith("iVBORw0KGgo")).toBe(true); // valid PNG from the codegen'd pipeline
  expect(prev.base64.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});

test("a procedural uv/frac graph codegen-renders in the preview", async () => {
  const m = await engine.call("material-create", { name: "Procedural" });
  const graph = {
    nodes: [
      { id: "uv", type: "uv" },
      { id: "s", type: "constant", props: { value: [8, 8, 8, 1] } },
      { id: "mul", type: "multiply" },
      { id: "f", type: "frac" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["uv", "out"], to: ["mul", "a"] },
      { from: ["s", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["f", "a"] },
      { from: ["f", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });

  const prev = await preview(m.id, 128);
  expect(prev.base64.startsWith("iVBORw0KGgo")).toBe(true);
  expect(prev.base64.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});

test("a mixed-size preview backlog drains one tile at a time", async () => {
  // One tile is in flight at a time and advances one frame per tick, so a backlog only clears if
  // every tile is started, converged, cached and retired in turn — and the single thumbnail view
  // is resized between tiles. A tile that never starts, never finishes, or leaves its in-flight
  // marker behind leaves the reply `pending` until this times out.
  const ids: string[] = [];
  for (let i = 0; i < 4; i++) {
    const m = await engine.call("material-create", { name: `Backlog${i}` });
    await engine.call("material-update", {
      material: m.id,
      baseColor: { x: i / 4, y: 1 - i / 4, z: 0.5, w: 1 },
    });
    ids.push(m.id);
  }
  for (const id of ids) {
    const big = await preview(id, 128);
    expect(big.base64.startsWith("iVBORw0KGgo")).toBe(true);
    const small = await preview(id, 64);
    expect(small.base64.startsWith("iVBORw0KGgo")).toBe(true);
    expect(small.width).toBe(64);
  }
  await engine.settle(300);

  const faults = engine.log
    .split("\n")
    .filter((line) => /has been in flight|ERROR_DEVICE_LOST|preview thumbnail render:/.test(line));
  expect(faults).toEqual([]);
  expect(engine.validationErrors()).toEqual([]);
});

test("view-asset renders the same material at its larger default", async () => {
  const m = await engine.call("material-create", { name: "Thumb" });
  const view = await engine.getThumbnail("view-asset", { asset: m.id });
  expect(view.format).toBe("png");
  expect(view.width).toBe(512);
  expect(view.base64.startsWith("iVBORw0KGgo")).toBe(true);
  expect(engine.validationErrors()).toEqual([]);
});

test("an assigned normal map perturbs the shaded result", async () => {
  const asset = (await engine.call("import-model", { path: MAPPED })).id;
  const e = trackEntity(caseCleaner, engine, await engine.call("instantiate-model", { asset }));
  await engine.settle(300);

  // Reuse the fixture's own albedo texture (from the imported model's referenced `.smat`) as a
  // (deliberately non-flat) normal map.
  const info = await engine.call("inspect", { entity: e.id });
  const slots = info.components.MaterialSet?.slots ?? [];
  expect(slots.length).toBeGreaterThan(0);
  const albedo = (await engine.call("material-get", { material: slots[0].material })).albedoTexture;
  expect(albedo).toBeDefined();
  expect(albedo).not.toBe("0");

  const flat = await screenshot("normal-flat");

  await engine.call("assign-asset", { entity: e.id, slot: "normal", asset: albedo });
  await engine.settle(300);
  const perturbed = await screenshot("normal-perturbed");

  // The perturbed normals must change the directional-light shading.
  expect(perturbed.equals(flat)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});
