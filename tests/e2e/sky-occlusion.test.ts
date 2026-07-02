// set-sky-occlusion over the control plane: the toggle gates the Global SDF reflection-occlusion
// cone (the one remaining per-pixel SDF consumer — it occludes the reflected skybox under overhangs;
// indirect diffuse occlusion is DDGI ray-miss + contact GTAO). Validated by READ-BACK — the set-*
// command echoes its state and render-stats reports the same `skyOcclusion` flag ("ok:true" alone
// proves nothing) — and the engine stays Vulkan-validation-clean across the flip (the lighting set's
// fragment GDF tap is gated by the enable bit).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { RenderStats } from "@saffron/protocol";

let engine: Engine;
const stats = () => engine.call<RenderStats & Record<string, unknown>>("render-stats");

beforeAll(async () => {
  // import-model needs a loaded project; auto-create an empty one (under the gitignored appdata/).
  engine = await Engine.boot({ SAFFRON_AUTO_EMPTY_PROJECT: "1" });
  await engine.call("add-entity", { preset: "cube" }); // geometry, so the lit pass actually runs
});
afterAll(async () => {
  await engine?.shutdown();
});

test("set-sky-occlusion echoes the state it is given", async () => {
  const on = await engine.call<{ skyOcclusion: boolean }>("set-sky-occlusion", { args: [1] });
  expect(on.skyOcclusion).toBe(true);
  const off = await engine.call<{ skyOcclusion: boolean }>("set-sky-occlusion", { args: [0] });
  expect(off.skyOcclusion).toBe(false);
});

test("set-sky-occlusion round-trips through render-stats", async () => {
  await engine.call("set-sky-occlusion", { args: [1] });
  expect((await stats()).skyOcclusion).toBe(true);
  await engine.call("set-sky-occlusion", { args: [0] });
  expect((await stats()).skyOcclusion).toBe(false);
});

// Runs last: by now the toggle has flipped the GDF reflection-occlusion enable bit several times.
test("toggling sky occlusion left no Vulkan validation errors", async () => {
  await engine.settle(400);
  expect(engine.validationErrors()).toEqual([]);
});
