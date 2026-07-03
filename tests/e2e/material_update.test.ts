// material-update edits a material asset's factors in place (the editor's live-edit path).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("material-update edits a material's factors and persists them", async () => {
  const created = await engine.call<{ id: string }>("material-create", { name: "Upd" });
  await engine.call("material-update", { material: created.id, roughness: 0.25, metallic: 0.5 });
  const m = await engine.call<{ roughness: number; metallic: number }>("material-get", {
    material: created.id,
  });
  expect(m.roughness).toBeCloseTo(0.25, 3);
  expect(m.metallic).toBeCloseTo(0.5, 3);
});
