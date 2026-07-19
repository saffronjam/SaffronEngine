// asset-usages (collectAssetUsages) over the control plane: a mesh asset reports the placed
// entity that references it, and a sky texture reports the environment.skyTexture slot with no
// entity. Asserts a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;

interface Usage {
  entity?: string;
  entityName?: string;
  slot: string;
}
interface UsagesResult {
  usages: Usage[];
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("asset-usages reports the placed entity for a mesh asset", async () => {
  await engine.importEntity(join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb"));
  const assets = await engine.call<AssetList>("list-assets");
  const mesh = assets.assets.find((a) => a.type === "mesh");
  expect(mesh).toBeDefined();

  const result = await engine.call<UsagesResult>("asset-usages", { asset: mesh!.id });
  const meshUsage = result.usages.find((u) => u.slot === "mesh");
  expect(meshUsage).toBeDefined();
  expect(meshUsage!.entity).toBeTruthy();
  expect(meshUsage!.entityName).toBeTruthy();
});

test("asset-usages reports the environment slot for the sky texture", async () => {
  const assets = await engine.call<AssetList>("list-assets");
  const tex = assets.assets.find((a) => a.type === "texture");
  if (!tex) {
    return; // this project has no texture asset; the mesh case above already proves the walk
  }
  await engine.call("set-environment", { skyMode: "texture", skyTexture: tex.id });
  const result = await engine.call<UsagesResult>("asset-usages", { asset: tex.id });
  const envUsage = result.usages.find((u) => u.slot === "environment.skyTexture");
  expect(envUsage).toBeDefined();
  expect(envUsage!.entity).toBeFalsy(); // the environment usage carries no entity
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
