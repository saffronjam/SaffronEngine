// A baked .smodel renders a preview from its embedded mesh chunk — get-thumbnail on the model id (and
// on an embedded mesh sub-id) produces a PNG, so the editor's one model tile is recognizable without
// any loose .smesh on disk.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { pngSize } from "./imggen.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

type Thumb = { base64: string; width: number; height: number; format: string };

let modelId = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  modelId = (await engine.call<{ id: string }>("import-model", { path: FIXTURE })).id;
});
afterAll(async () => {
  await engine?.shutdown();
});

test("get-thumbnail on a model id renders a PNG from the embedded mesh", async () => {
  const t = await engine.getThumbnail<Thumb>("get-thumbnail", { asset: modelId, size: 128 });
  expect(t.format).toBe("png");
  const { width, height } = pngSize(Buffer.from(t.base64, "base64"));
  expect(Math.max(width, height)).toBe(128);
  expect(t.width).toBe(width);
  expect(t.height).toBe(height);
});

test("get-thumbnail on an embedded mesh sub-asset also renders", async () => {
  // The mesh sub-asset lives only inside the .smodel; its thumbnail must resolve through the container.
  const assets = await engine.call<AssetList>("list-assets");
  const meshSub = assets.assets.find((a) => a.type === "mesh" && a.container === modelId);
  expect(meshSub).toBeDefined();
  const t = await engine.getThumbnail<Thumb>("get-thumbnail", { asset: meshSub!.id, size: 128 });
  expect(t.format).toBe("png");
  expect(Math.max(t.width, t.height)).toBe(128);
});

test("get-thumbnail on an embedded material sub-asset also renders", async () => {
  const assets = await engine.call<AssetList>("list-assets");
  const materialSub = assets.assets.find((a) => a.type === "material" && a.container === modelId);
  expect(materialSub).toBeDefined();
  const t = await engine.getThumbnail<Thumb>("get-thumbnail", { asset: materialSub!.id, size: 128 });
  expect(t.format).toBe("png");
  expect(Math.max(t.width, t.height)).toBe(128);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
  expect(engine.log).not.toContain("stream did not contain valid UTF-8");
  expect(engine.log).not.toContain("unresolved");
});
