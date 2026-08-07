// Asset-folder control commands over the control plane: path validation, move-asset, the
// rename cascade onto descendant folders + the assets under them, delete clearing the subtree,
// and rename-asset. Asserts a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-folders-${process.pid}`;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");
let modelId = "";

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  const model = await engine.call("import-model", { path: FIXTURE });
  modelId = model.id;
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

async function folderOf(id: string): Promise<string | undefined> {
  const list = await engine.call("list-assets");
  return list.assets.find((a) => a.id === id)?.folder;
}

test("create-asset-folder validates the path", async () => {
  for (const bad of ["/bad", "bad/", "a//b"]) {
    await expect(engine.call("create-asset-folder", { folder: bad })).rejects.toThrow();
  }
  const list = await engine.call("create-asset-folder", { folder: "a/b/c" });
  expect(list.folders).toContain("a/b/c");
});

test("move-asset places an asset in a folder and rejects an unknown one", async () => {
  // create-asset-folder registers exact paths (no implicit ancestors), so create "a" too —
  // rename-asset-folder later renames the exact "a" folder and cascades onto "a/b".
  await engine.call("create-asset-folder", { folder: "a" });
  await engine.call("create-asset-folder", { folder: "a/b" });
  await engine.call("move-asset", { asset: modelId, folder: "a/b" });
  expect(await folderOf(modelId)).toBe("a/b");
  await expect(
    engine.call("move-asset", { asset: modelId, folder: "does/not/exist" }),
  ).rejects.toThrow();
});

test("rename-asset-folder cascades to descendant folders and the assets under them", async () => {
  const list = await engine.call("rename-asset-folder", { folder: "a", name: "x" });
  expect(list.folders).toContain("x/b");
  expect(list.folders).not.toContain("a/b");
  expect(await folderOf(modelId)).toBe("x/b");
});

test("delete-asset-folder removes the subtree and clears assets under it", async () => {
  const list = await engine.call("delete-asset-folder", { folder: "x" });
  expect(list.folders.some((f) => f === "x" || f.startsWith("x/"))).toBe(false);
  expect(await folderOf(modelId)).toBeUndefined();
});

test("rename-asset changes the catalog name", async () => {
  await engine.call("rename-asset", { asset: modelId, name: "RenamedModel" });
  const list = await engine.call("list-assets");
  expect(list.assets.find((a) => a.id === modelId)?.name).toBe("RenamedModel");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
