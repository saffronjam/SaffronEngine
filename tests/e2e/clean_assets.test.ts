// Deliberate cleanup: clean-assets classifies a model used by the scene as kept and an un-instantiated
// import as Unused, and delete-unused removes it only after an explicit confirm (refusing otherwise),
// then rescans. Never automatic — the report is dry-run and deletion is gated.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
// Distinct source files: identical sources share stable sub-ids (by source-name), so two imports of the
// same file would collide in the catalog. A used model and an unused one must come from different files.
const USED_FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");
const UNUSED_FIXTURE = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");

interface CleanReport {
  candidates: { id: string; path: string; category: string; bytes: number; reason: string }[];
  reclaimableBytes: number;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

let usedModel = "";
let unusedModel = "";

test("clean-assets flags an un-instantiated import as Unused, the scene's model as kept", async () => {
  usedModel = (await engine.call<{ id: string }>("import-model", { path: USED_FIXTURE })).id;
  await engine.call("instantiate-model", { asset: usedModel, name: "Used" });
  unusedModel = (await engine.call<{ id: string }>("import-model", { path: UNUSED_FIXTURE })).id;
  await engine.settle();

  const report = await engine.call<CleanReport>("clean-assets");
  const unused = report.candidates.find((c) => c.id === unusedModel);
  expect(unused).toBeDefined();
  expect(unused?.category).toBe("unused");
  // The instantiated model is reachable, so it is never a candidate.
  expect(report.candidates.some((c) => c.id === usedModel)).toBe(false);
});

test("delete-unused refuses without an explicit confirm", async () => {
  await expect(engine.call("delete-unused", { ids: [unusedModel], confirm: false })).rejects.toThrow();
});

test("delete-unused removes the unused model on confirm, and it is gone from the catalog", async () => {
  const result = await engine.call<{ deleted: number }>("delete-unused", { ids: [unusedModel], confirm: true });
  expect(result.deleted).toBeGreaterThanOrEqual(1);
  await engine.settle();

  const assets = await engine.call<AssetList>("list-assets");
  expect(assets.assets.some((a) => a.id === unusedModel)).toBe(false);
  // The used model survives.
  expect(assets.assets.some((a) => a.id === usedModel)).toBe(true);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
