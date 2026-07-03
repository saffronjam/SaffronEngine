// material-get reads back a material asset's fields (for the editor's material inspector).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("material-get returns a created material's fields", async () => {
  const created = await engine.call<{ id: string }>("material-create", { name: "Get" });
  const m = await engine.call<{ id: string; metallic: number; roughness: number; blend: string }>(
    "material-get",
    { material: created.id },
  );
  expect(m.id).toBe(created.id);
  expect(m.roughness).toBe(1);
  expect(m.metallic).toBe(0);
  expect(m.blend).toBe("opaque");
});
