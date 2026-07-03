// material-update can set a material's texture slots (so the editor can assign normal/orm/emissive/
// height maps to a material asset, not just scalar factors).

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

test("material-update assigns a texture slot to a material asset", async () => {
  const e = await engine.importEntity(MAPPED);
  const info = await engine.call<InspectResult>("inspect", { entity: e.id });
  const tex = (info.components.Material as { albedoTexture?: string }).albedoTexture;
  expect(tex).toBeDefined();
  expect(tex).not.toBe("0");

  const m = await engine.call<{ id: string }>("material-create", { name: "TexMat" });
  await engine.call("material-update", { material: m.id, normalTexture: tex });
  const got = await engine.call<{ normalTexture: string }>("material-get", { material: m.id });
  expect(got.normalTexture).toBe(tex);
});
