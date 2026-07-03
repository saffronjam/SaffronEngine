// Asset control-plane behaviour:
//   - probe-asset reports on-disk metadata (size, vertex/triangle counts, mtime);
//   - assign-asset with the "0" none sentinel clears a slot instead of erroring.
// Boots with SAFFRON_SCRATCH_PROJECT so the cube preset (which imports a model and
// so needs a loaded project) populates the asset catalog.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { AssetList, AssetMetadataDto, EntityRef, InspectResult } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

const DECIMAL_U64 = /^[0-9]+$/;

test("probe-asset returns on-disk metadata for a mesh", async () => {
  await engine.call<EntityRef>("add-entity", { args: ["cube"] });
  const assets = await engine.call<AssetList>("list-assets");
  const mesh = assets.assets.find((a) => a.type === "mesh");
  expect(mesh).toBeDefined();
  // The list entry carries the file's creation time so the browser can sort by it.
  expect(mesh!.createdAt).toBeGreaterThan(0);

  const meta = await engine.call<AssetMetadataDto>("probe-asset", { asset: mesh!.id });
  expect(meta.id).toBe(mesh!.id);
  expect(meta.type).toBe("mesh");
  expect(meta.sizeBytes).toBeGreaterThan(0);
  expect(meta.vertexCount ?? 0).toBeGreaterThan(0);
  expect(meta.triangleCount ?? 0).toBeGreaterThan(0);
  expect(meta.createdAt).toBeGreaterThan(0);
});

test("assign-asset clears the mesh slot on the none sentinel", async () => {
  const cube = await engine.call<EntityRef>("add-entity", { args: ["cube"] });
  const before = await engine.call<InspectResult>("inspect", { entity: cube.id });
  const meshBefore = (before.components.Mesh as { mesh?: string } | undefined)?.mesh;
  expect(meshBefore).toMatch(DECIMAL_U64);
  expect(meshBefore).not.toBe("0");

  await engine.call("assign-asset", { entity: cube.id, slot: "mesh", asset: "0" });

  const after = await engine.call<InspectResult>("inspect", { entity: cube.id });
  const meshAfter = (after.components.Mesh as { mesh?: string } | undefined)?.mesh;
  expect(meshAfter).toBe("0");
});
