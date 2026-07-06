// Preview-render suite for the material system: the offscreen studio-sphere preview (the editor's
// material preview pane + cached thumbnails) plus the one scene-screenshot case that shares the same
// codegen/override plumbing (normal-map perturbation). Consolidated onto a single booted host.
//
// Covers:
//   - preview-render returns a PNG that reflects the material's base color (white vs red differ);
//   - a foldable constant graph folds to the same preview as a direct base-color material;
//   - a non-foldable multiply graph codegen-renders in the preview;
//   - a procedural uv/frac graph codegen-renders in the preview;
//   - get-thumbnail renders a material preview PNG;
//   - a normal map assigned to an entity perturbs the shaded scene result.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef, InspectResult } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const shots: string[] = [];
const placed: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Only the normal-map case renders the scene; IBL + a light are harmless to the preview cases,
  // which render their own studio-lit sphere offscreen.
  await engine.call("set-ibl", { args: ["on"] }).catch(() => {});
  await engine.call("add-entity", { args: ["directional-light"] }).catch(() => {});
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});
afterEach(async () => {
  while (placed.length > 0) {
    const id = placed.pop()!;
    await engine.call("destroy-entity", { entity: id }).catch(() => {});
  }
  await engine.settle(150);
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-matprev-${process.pid}-${tag}.png`;
  shots.push(path);
  rmSync(path, { force: true }); // never read a stale frame from a reused tag
  await engine.call("screenshot", { target: "viewport", path });
  const deadline = Date.now() + 10_000;
  while (!existsSync(path)) {
    if (Date.now() > deadline) {
      throw new Error(`screenshot ${tag} never landed`);
    }
    await engine.settle(100);
  }
  await engine.settle(200);
  return readFileSync(path);
}

test("preview-render returns a PNG that reflects the material's color", async () => {
  const a = await engine.call<{ id: string }>("material-create", { name: "PrevA" });
  const b = await engine.call<{ id: string }>("material-create", { name: "PrevB" });
  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await engine.call<{ png: string }>("preview-render", { material: a.id, size: 128 });
  const pb = await engine.call<{ png: string }>("preview-render", { material: b.id, size: 128 });

  expect(pa.png.length).toBeGreaterThan(100);
  expect(pa.png.startsWith("iVBORw0KGgo")).toBe(true); // PNG magic, base64
  expect(pa.png).not.toBe(pb.png); // white vs red sphere
  expect(engine.validationErrors()).toEqual([]);
});

test("a foldable node graph drives the material like direct factors", async () => {
  const a = await engine.call<{ id: string }>("material-create", { name: "GraphA" });
  const b = await engine.call<{ id: string }>("material-create", { name: "DirectB" });

  const graph = {
    nodes: [
      { id: "c", type: "constant", props: { value: [1, 0, 0, 1] } },
      { id: "out", type: "materialOutput" },
    ],
    edges: [{ from: ["c", "rgba"], to: ["out", "baseColor"] }],
  };
  const set = await engine.call<{ id: string; foldable: boolean }>("material-set-graph", {
    material: a.id,
    graph,
  });
  expect(set.foldable).toBe(true);

  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await engine.call<{ png: string }>("preview-render", { material: a.id, size: 128 });
  const pb = await engine.call<{ png: string }>("preview-render", { material: b.id, size: 128 });

  expect(pa.png).toBe(pb.png); // the graph folds to the same red material
  expect(engine.validationErrors()).toEqual([]);
});

test("a procedural graph renders via codegen in the preview", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "CodegenPrev" });
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

  const prev = await engine.call<{ png: string }>("preview-render", { material: m.id, size: 128 });
  expect(prev.png.startsWith("iVBORw0KGgo")).toBe(true); // valid PNG from the codegen'd pipeline
  expect(prev.png.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});

test("a procedural uv/frac graph codegen-renders in the preview", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Procedural" });
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

  const prev = await engine.call<{ png: string }>("preview-render", { material: m.id, size: 128 });
  expect(prev.png.startsWith("iVBORw0KGgo")).toBe(true);
  expect(prev.png.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});

test("get-thumbnail renders a material preview PNG", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Thumb" });
  const thumb = await engine.getThumbnail<{ base64: string; format: string }>("get-thumbnail", {
    asset: m.id,
    size: 96,
  });
  expect(thumb.format).toBe("png");
  expect(thumb.base64.startsWith("iVBORw0KGgo")).toBe(true);
  expect(engine.validationErrors()).toEqual([]);
});

test("an assigned normal map perturbs the shaded result", async () => {
  const asset = (await engine.call<{ id: string }>("import-model", { path: MAPPED })).id;
  const e = await engine.call<EntityRef>("instantiate-model", { asset });
  placed.push(e.id);
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);

  // Reuse the fixture's own albedo texture (from the imported model's referenced `.smat`) as a
  // (deliberately non-flat) normal map.
  const info = await engine.call<InspectResult>("inspect", { entity: e.id });
  const slots = (info.components.MaterialSet as { slots?: { material: string }[] }).slots ?? [];
  expect(slots.length).toBeGreaterThan(0);
  const albedo = (
    await engine.call<{ albedoTexture: string }>("material-get", { material: slots[0].material })
  ).albedoTexture;
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
