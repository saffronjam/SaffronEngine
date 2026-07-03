// The decoupled import flow: `import-model` bakes a glTF into one `.smodel` asset + catalog
// rows WITHOUT spawning an entity, and `instantiate-model` expands that stored asset into the scene on
// demand — one asset, many independent instances. This is the "import once, instance many" contract the
// editor's drag-to-instantiate (and reimport) builds on.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList, EntityRef } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface EntityList {
  entities: { id: string; name: string }[];
}
interface ModelAssetRef {
  id: string;
  name: string;
  type: string;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

let modelId = "";

test("import-model bakes a model asset and does NOT spawn an entity", async () => {
  const before = (await engine.call<EntityList>("list-entities")).entities.length;

  const ref = await engine.call<ModelAssetRef>("import-model", { path: FIXTURE });
  await engine.settle();
  expect(ref.type).toBe("model");
  expect(ref.id).not.toBe("0");
  modelId = ref.id;

  // No entity was created by the import.
  const after = (await engine.call<EntityList>("list-entities")).entities.length;
  expect(after).toBe(before);

  // The model asset is in the catalog, alongside its embedded sub-assets.
  const assets = await engine.call<AssetList>("list-assets");
  expect(assets.assets.some((a) => a.id === modelId)).toBe(true);
});

test("instantiate-model expands the asset into the scene, twice, as independent entities", async () => {
  const before = (await engine.call<EntityList>("list-entities")).entities.length;

  const first = await engine.call<EntityRef>("instantiate-model", { asset: modelId, name: "Crate A" });
  const second = await engine.call<EntityRef>("instantiate-model", { asset: modelId, name: "Crate B" });
  await engine.settle();
  expect(first.id).not.toBe(second.id);

  const after = (await engine.call<EntityList>("list-entities")).entities.length;
  expect(after).toBeGreaterThan(before);
});

test("a never-saved import survives a reload via the filesystem scan (orphan-proof)", async () => {
  // The model was imported but never save-project'd, so project.json does not list it. A reload reads
  // that stale project.json and the scan rediscovers the .smodel on disk.
  await engine.reloadProject();
  await engine.settle();
  const assets = await engine.call<AssetList>("list-assets");
  expect(assets.assets.some((a) => a.id === modelId)).toBe(true);
});

test("scan-assets is reachable and reports a delta", async () => {
  const delta = await engine.call<{ added: number; removed: number }>("scan-assets");
  expect(delta.added).toBeGreaterThanOrEqual(0);
  expect(delta.removed).toBeGreaterThanOrEqual(0);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
