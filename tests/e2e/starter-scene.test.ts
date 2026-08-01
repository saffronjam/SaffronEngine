// A freshly created project ships a real starter scene — a "Camera" and a "Sun" directional
// light, both ordinary editable/deletable entities, with no hidden fallback sun behind them.
// Deleting the Sun leaves the scene with no directional light, and the frame stays
// Vulkan-validation-clean (the "no sun" path feeds a valid direction at zero intensity, so
// nothing normalizes a zero vector into a NaN).

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Components } from "@saffron/protocol";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

async function entities(): Promise<{ id: string; name: string }[]> {
  return (await engine.call("list-entities")).entities;
}

async function hasComponent(id: string, name: keyof Components): Promise<boolean> {
  const info = await engine.call("inspect", { entity: id });
  return info.components[name] !== undefined;
}

test("a fresh project seeds a Camera and a Sun directional light", async () => {
  const list = await entities();
  const sun = list.find((e) => e.name === "Sun");
  const camera = list.find((e) => e.name === "Camera");
  expect(sun).toBeDefined();
  expect(camera).toBeDefined();
  expect(await hasComponent(sun!.id, "DirectionalLight")).toBe(true);
  expect(await hasComponent(camera!.id, "Camera")).toBe(true);
});

test("deleting the Sun leaves no directional light and stays validation-clean", async () => {
  const sun = (await entities()).find((e) => e.name === "Sun");
  expect(sun).toBeDefined();
  await engine.call("destroy-entity", { entity: sun!.id });

  const remaining = await entities();
  expect(remaining.some((e) => e.name === "Sun")).toBe(false);
  for (const e of remaining) {
    expect(await hasComponent(e.id, "DirectionalLight")).toBe(false);
  }

  // Render a few frames with no directional light and confirm the GPU path is clean (no NaN).
  await engine.settle(300);
  expect(engine.validationErrors()).toEqual([]);
});
