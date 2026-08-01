// Scene-screenshot suite for the native material path: every case renders the mapped-material.glb
// fixture on a real scene entity under fixed IBL + a directional light and asserts on the shaded
// pixels. Consolidated onto a single booted host — mapped-material.glb is imported once (the bake is
// the expensive part; `mintTextureId` memoizes it), and each case instantiates its own entity so a
// per-case material mutation never leaks into the next test.
//
// Covers:
//   - a created .smat assigned to an entity takes precedence over its inline glTF material (the asset
//     drives the render);
//   - a slot-0 ORM (metallic-roughness) override reaches the GPU and changes the shaded result, then
//     clears back to the referenced .smat;
//   - a non-foldable node graph splices into the übershader, compiles a per-material PSO on disk, and
//     renders validation-clean on an entity.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import type { EntityRef } from "@saffron/protocol";
import { existsSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner, prepareScene, trackEntity } from "./test-utils.ts";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const suiteCleaner = new Cleaner();
const caseCleaner = new Cleaner();

let mappedAsset: string | undefined;

beforeAll(async () => {
  engine = await bootEngine(suiteCleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    camera: { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 },
  });
  await engine.call("set-ibl", { args: ["on"] }).catch(() => {});
  // The scratch project's starter scene already carries a Sun directional light.
});
afterAll(async () => {
  await suiteCleaner.cleanup();
});
afterEach(async () => {
  await caseCleaner.cleanup();
  await engine.settle(150);
});

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, caseCleaner, `matrender-${tag}`);
}

// mapped-material.glb is imported once (the bake is expensive); each call instantiates a fresh entity
// from the cached asset and registers it for per-test cleanup.
async function mintMappedEntity(): Promise<EntityRef> {
  mappedAsset ??= (await engine.call("import-model", { path: MAPPED })).id;
  return trackEntity(
    caseCleaner,
    engine,
    await engine.call("instantiate-model", { asset: mappedAsset }),
  );
}

// The albedo + packed-ORM texture ids on the mapped entity's referenced `.smat`, plus the slot
// material and count — read from the entity's MaterialSet. The override/normal cases assign these
// real imported textures; keeping the read here dedupes it across cases.
async function mintTextureId(
  entity: string,
): Promise<{ material: string; slots: number; albedo: string; orm: string }> {
  const info = await engine.call("inspect", { entity });
  const slots = info.components.MaterialSet?.slots ?? [];
  const material = slots[0]?.material;
  const smat = await engine.call("material-get", {
    material,
  });
  return { material, slots: slots.length, albedo: smat.albedoTexture, orm: smat.ormTexture };
}

test("a created .smat material assigned to an entity drives the render", async () => {
  const e = await mintMappedEntity();
  await engine.settle(300);
  const gltfShot = await screenshot("asset-gltf");

  // Create a fresh default material (white, no textures) and assign it; it takes precedence over
  // the entity's inline glTF material, so the textured surface becomes the flat default.
  const created = await engine.call("material-create", { name: "TestMat" });
  expect(created.id).toBeDefined();
  expect(created.id).not.toBe("0");

  await engine.call("material-assign", { entity: e.id, material: created.id });
  await engine.settle(300);
  const smatShot = await screenshot("asset-smat");

  expect(smatShot.equals(gltfShot)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});

test("an ORM texture override changes the shaded pixels", async () => {
  const e = await mintMappedEntity();
  await engine.settle(300);

  // The imported metallic-roughness map lives on the slot's referenced `.smat` (packed ORM).
  const tex = await mintTextureId(e.id);
  expect(tex.slots).toBeGreaterThan(0);
  expect(tex.orm).toBeDefined();
  expect(tex.orm).not.toBe("0"); // the imported MR texture is on the referenced .smat
  expect(tex.albedo).not.toBe("0");
  const withSmatOrm = await screenshot("orm-base");

  // Override slot 0's ORM with the albedo texture — its channels encode a very different
  // metallic/roughness than the smooth MR map, so the shaded result must change.
  await engine.call("assign-asset", {
    entity: e.id,
    slot: "metallic-roughness",
    asset: tex.albedo,
  });
  await engine.settle(300);
  const withOverride = await screenshot("orm-override");
  expect(withOverride.equals(withSmatOrm)).toBe(false);

  // Clearing the override drops back to the referenced `.smat` ORM (exercises the clear path).
  await engine.call("assign-asset", { entity: e.id, slot: "metallic-roughness", asset: "0" });
  await engine.settle(300);

  expect(engine.validationErrors()).toEqual([]);
});

test("a codegen material compiles a übershader variant and renders on an entity", async () => {
  const project = await engine.call("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);

  const e = await mintMappedEntity();
  await engine.settle(300);
  const before = await screenshot("cg-before");

  const m = await engine.call("material-create", { name: "SceneCodegen" });
  const graph = {
    nodes: [
      { id: "c1", type: "constant", props: { value: [0, 1, 0, 1] } },
      { id: "c2", type: "constant", props: { value: [1, 1, 1, 1] } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["mul", "a"] },
      { from: ["c2", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  const set = await engine.call("material-set-graph", {
    material: m.id,
    graph,
  });
  expect(set.foldable).toBe(false); // procedural multiply -> codegen path

  // material-set-graph compiled a per-material übershader variant (the splice produced valid Slang).
  const spv = join(root, "assets", "materials", `${m.id}_mesh.spv`);
  expect(existsSync(spv)).toBe(true);

  await engine.call("material-assign", { entity: e.id, material: m.id });
  await engine.settle(400);
  const after = await screenshot("cg-after");

  expect(after.equals(before)).toBe(false); // the codegen surface changed the rendered result
  expect(engine.validationErrors()).toEqual([]); // the spliced übershader variant bound + drew cleanly
});
