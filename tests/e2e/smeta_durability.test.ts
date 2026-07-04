// Durable asset metadata: a never-saved import or rename survives a cold catalog scan because an
// asset's name lives in a co-located `.smeta` sidecar, not only in project.json. This reproduces
// the "imported HDR came back named as a bare uuid" bug and its rename variant, across the three
// asset shapes: a standalone texture, a `.smodel` model row, and an extracted sub-asset.
//
// The cold scan is forced by deleting `assets/.cache/catalog.json` and reloading the *unsaved*
// project.json (which knows none of these assets) — so the names can only come from the sidecars.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { makeHdr } from "./imggen.ts";
import type { AssetList } from "@saffron/protocol";

const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

let engine: Engine;
let sources: string;
const projectDir = `/tmp/saffron-e2e-smeta-durable-${process.pid}`;
let textureId = "";
let modelId = "";
let subId = "";

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.loadProject(`${projectDir}/project.json`);

  // Import an HDR texture + a model into the loaded project (both write files under assets/).
  // Source files live outside assets/ so the scan never sees them as foreign inputs.
  sources = mkdtempSync(join(tmpdir(), "saffron-smeta-src-"));
  const hdrPath = join(sources, "sky.hdr");
  writeFileSync(
    hdrPath,
    makeHdr(6, 6, (x, y) => {
      const v = 0.1 + (x + y) * 0.05;
      return [v, v * 0.6, v * 0.3];
    }),
  );
  textureId = (await engine.call<{ texture: string }>("import-texture", { path: hdrPath })).texture;
  modelId = (await engine.call<{ id: string }>("import-model", { path: FIXTURE })).id;

  // Extract an embedded material to a standalone `.smat` (so its name is durable via its own leaf
  // sidecar, not the container META).
  const before = await engine.call<AssetList>("list-assets");
  const embedded = before.assets.find((a) => a.type === "material" && a.container === modelId);
  expect(embedded).toBeDefined();
  subId = embedded!.id;
  await engine.call("extract-subasset", { asset: modelId, subAsset: subId });

  // Rename all three — and deliberately DO NOT save the project.
  await engine.call("rename-asset", { asset: textureId, name: "Sky" });
  await engine.call("rename-asset", { asset: modelId, name: "Hero" });
  await engine.call("rename-asset", { asset: subId, name: "Brass" });
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
  if (sources) rmSync(sources, { recursive: true, force: true });
});

test("a never-saved import + rename survives a cold catalog scan", async () => {
  // Force the cold path: drop the cache (it would otherwise carry the names and hide a regression),
  // then reload the unsaved project.json — the disk (sidecars) is the only source of these names.
  rmSync(`${projectDir}/assets/.cache/catalog.json`, { force: true });
  await engine.loadProject(`${projectDir}/project.json`);
  await engine.settle();

  const assets = await engine.call<AssetList>("list-assets");
  const nameOf = (id: string) => assets.assets.find((a) => a.id === id)?.name;
  expect(nameOf(textureId)).toBe("Sky"); // the reported bug: a bare uuid before the fix
  expect(nameOf(modelId)).toBe("Hero");
  expect(nameOf(subId)).toBe("Brass");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
