// The reference inspector: model-info reports a container's sub-assets + byte footprint, and
// asset-references walks the live dependency graph — an instantiated entity shows up as a referrer of
// the model, and the model references its embedded sub-assets.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface ModelInfo {
  id: string;
  name: string;
  materialCount: number;
  nodeCount: number;
  totalBytes: number;
  subAssets: { id: string; name: string; type: string; bytes: number }[];
}
interface AssetReferences {
  referencedBy: string[];
  references: string[];
  footprint: number;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

let modelId = "";

test("model-info reports the container's sub-assets and footprint", async () => {
  const ref = await engine.call<{ id: string }>("import-model", { path: FIXTURE });
  modelId = ref.id;
  await engine.settle();

  const info = await engine.call<ModelInfo>("model-info", { asset: modelId });
  expect(info.id).toBe(modelId);
  expect(info.subAssets.length).toBeGreaterThan(0);
  expect(info.materialCount).toBeGreaterThanOrEqual(1);
  expect(info.totalBytes).toBeGreaterThan(0);
  expect(info.subAssets.some((s) => s.type === "mesh")).toBe(true);
});

test("asset-references shows the model referencing its sub-assets", async () => {
  const refs = await engine.call<AssetReferences>("asset-references", { asset: modelId });
  expect(refs.references.length).toBeGreaterThan(0); // container -> sub-assets
  expect(refs.footprint).toBeGreaterThan(0);
});

test("an instantiated entity becomes a referrer of the model", async () => {
  await engine.call<EntityRef>("instantiate-model", { asset: modelId, name: "Inst" });
  await engine.settle();
  const refs = await engine.call<AssetReferences>("asset-references", { asset: modelId });
  expect(refs.referencedBy.length).toBeGreaterThan(0); // the ModelInstance entity
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
