// The full .smodel flow end to end: import (no spawn) → scan survives a reload → instantiate many →
// extract a sub-asset → reimport (content-addressed skip) → inspect references → clean. Asserts a
// validation-clean log throughout. This is the integration proof that the pieces compose.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList, EntityRef } from "@saffron/protocol";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-flow-${process.pid}`;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface EntityList {
  entities: { id: string; name: string }[];
}
interface CleanReport {
  candidates: { id: string; category: string }[];
}
interface AssetReferences {
  referencedBy: string[];
  references: string[];
}

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.call("load-project", { path: `${projectDir}/project.json` });
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

let modelId = "";
let materialSub = "";

test("1. import bakes one .smodel asset and spawns no entity", async () => {
  const before = (await engine.call<EntityList>("list-entities")).entities.length;
  modelId = (await engine.call<{ id: string }>("import-model", { path: FIXTURE })).id;
  await engine.settle();
  expect((await engine.call<EntityList>("list-entities")).entities.length).toBe(before);
});

test("2. a reload reconstructs the catalog from disk (the import survives)", async () => {
  await engine.call("load-project", { path: `${projectDir}/project.json` });
  await engine.settle();
  const assets = await engine.call<AssetList>("list-assets");
  expect(assets.assets.some((a) => a.id === modelId)).toBe(true);
});

test("3. one asset instantiates into many independent entities", async () => {
  const a = await engine.call<EntityRef>("instantiate-model", { asset: modelId, name: "A" });
  const b = await engine.call<EntityRef>("instantiate-model", { asset: modelId, name: "B" });
  await engine.settle();
  expect(a.id).not.toBe(b.id);
});

test("4. an embedded material extracts to a standalone file keeping its id", async () => {
  const assets = await engine.call<AssetList>("list-assets");
  const mat = assets.assets.find((x) => x.type === "material" && x.container === modelId);
  expect(mat).toBeDefined();
  materialSub = mat!.id;
  const ref = await engine.call<{ id: string }>("extract-subasset", { asset: modelId, subAsset: materialSub });
  expect(ref.id).toBe(materialSub); // identity preserved through extraction
  await engine.settle();
  const after = await engine.call<AssetList>("list-assets");
  const extracted = after.assets.find((x) => x.id === materialSub);
  expect(extracted?.container).toBeUndefined(); // now standalone
});

test("5. a no-op reimport is content-addressed skipped", async () => {
  const delta = await engine.call<{ skipped: boolean }>("reimport-model", { asset: modelId });
  expect(delta.skipped).toBe(true);
});

test("6. the model is referenced by its live instances and references its sub-assets", async () => {
  const refs = await engine.call<AssetReferences>("asset-references", { asset: modelId });
  expect(refs.referencedBy.length).toBeGreaterThan(0);
  expect(refs.references.length).toBeGreaterThan(0);
});

test("7. clean-assets keeps the in-use model", async () => {
  const report = await engine.call<CleanReport>("clean-assets");
  expect(report.candidates.some((c) => c.id === modelId && c.category === "unused")).toBe(false);
});

test("the engine logged no validation errors across the whole flow", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
