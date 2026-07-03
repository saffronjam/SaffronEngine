// The store-import host contract: `import-model` accepts optional attribution, bakes the
// model exactly as a local import, and persists the license/author/source onto the catalog
// row in project.json so attribution travels with the asset. The connector HTTP itself is
// editor-side and not exercised here — this targets the control-plane command.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-store-${process.pid}`;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

const ATTRIBUTION = {
  licenseId: "cc-by",
  requiresAttribution: true,
  licenseUrl: "https://creativecommons.org/licenses/by/4.0/",
  author: "Test Author",
  sourceUrl: "https://example.com/a/test-model",
  storeId: "polyhaven",
};

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

test("import-model with attribution bakes a catalog asset", async () => {
  modelId = (
    await engine.call<{ id: string }>("import-model", {
      path: FIXTURE,
      attribution: ATTRIBUTION,
    })
  ).id;
  await engine.settle();
  const assets = await engine.call<AssetList>("list-assets");
  expect(assets.assets.some((a) => a.id === modelId)).toBe(true);
});

test("a plain import-model (no attribution) still works", async () => {
  const id = (await engine.call<{ id: string }>("import-model", { path: FIXTURE })).id;
  expect(id).toBeTruthy();
});

test("list-assets surfaces the attribution for the credits view", async () => {
  const list = await engine.call<AssetList>("list-assets");
  const row = list.assets.find((a) => a.id === modelId) as
    | { attribution?: typeof ATTRIBUTION }
    | undefined;
  expect(row?.attribution?.licenseId).toBe("cc-by");
  expect(row?.attribution?.requiresAttribution).toBe(true);
  expect(row?.attribution?.author).toBe("Test Author");
  expect(row?.attribution?.sourceUrl).toBe(ATTRIBUTION.sourceUrl);
});

test("the attribution is persisted onto the catalog row in project.json", async () => {
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.settle();
  const doc = JSON.parse(readFileSync(`${projectDir}/project.json`, "utf8")) as {
    assets: { id: string; attribution?: typeof ATTRIBUTION }[];
  };
  const row = doc.assets.find((a) => a.id === modelId);
  expect(row?.attribution?.licenseId).toBe("cc-by");
  expect(row?.attribution?.requiresAttribution).toBe(true);
  expect(row?.attribution?.author).toBe("Test Author");
  expect(row?.attribution?.storeId).toBe("polyhaven");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
