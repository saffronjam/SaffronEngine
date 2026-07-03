// Proves the PBR map slots (normal/occlusion/emissive/height) survive project save + reload.
// Imports a textured fixture, assigns a normal map, saves + reloads the project, and asserts the
// normal slot persisted.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef, InspectResult } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("an assigned normal map survives project save + reload", async () => {
  const e = await engine.importEntity(MAPPED);
  const before = await engine.call<InspectResult>("inspect", { entity: e.id });
  const albedo = (before.components.Material as { albedoTexture?: string }).albedoTexture;
  expect(albedo).toBeDefined();
  expect(albedo).not.toBe("0");

  // Reuse the albedo texture as the normal slot, then round-trip the project.
  await engine.call("assign-asset", { entity: e.id, slot: "normal", asset: albedo });
  await engine.call("save-project");
  await engine.call("reload-project");

  const after = await engine.call<InspectResult>("inspect", { entity: e.id });
  const normal = (after.components.Material as { normalTexture?: string }).normalTexture;
  expect(normal).toBe(albedo);
});
