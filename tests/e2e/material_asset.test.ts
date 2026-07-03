// Proves the native .smat material path end to end: material-create writes a material asset,
// material-assign attaches a MaterialAssetComponent, and resolveEntityMaterials' precedence picks
// the asset over the entity's inline (glTF-imported) material. The fixture imports textured, so
// assigning a fresh default material (white, no textures) must change the shaded result — proving
// the asset actually drives the render. The validation log must stay clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const shots: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-ibl", { args: ["on"] }).catch(() => {});
  await engine.call("add-entity", { args: ["directional-light"] }).catch(() => {});
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-smat-${process.pid}-${tag}.png`;
  shots.push(path);
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

test("a created .smat material assigned to an entity drives the render", async () => {
  const e = await engine.importEntity(MAPPED);
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);
  const gltfShot = await screenshot("gltf");

  // Create a fresh default material (white, no textures) and assign it; it takes precedence over
  // the entity's inline glTF material, so the textured surface becomes the flat default.
  const created = await engine.call<{ id: string }>("material-create", { name: "TestMat" });
  expect(created.id).toBeDefined();
  expect(created.id).not.toBe("0");

  await engine.call("material-assign", { entity: e.id, material: created.id });
  await engine.settle(300);
  const smatShot = await screenshot("smat");

  expect(smatShot.equals(gltfShot)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});
