// material-list enumerates the project's material assets (for the editor's material browser).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("material-list returns created material assets", async () => {
  const created = await engine.call<{ id: string }>("material-create", { name: "Listed" });
  const list = await engine.call<{ materials: { id: string; name: string }[] }>("material-list", {});
  expect(list.materials.some((m) => m.id === created.id)).toBe(true);
  expect(list.materials.some((m) => m.name === "Listed")).toBe(true);
});
