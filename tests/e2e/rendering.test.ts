// End-to-end rendering tests: drive a real headless engine over the control plane and
// assert on its responses + the engine's own validation output. Run with `bun test`
// (or `make e2e`), inside the saffron-build toolbox.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { EntityList, RenderStats } from "@saffron/protocol";


let engine: Engine;
beforeAll(async () => {
  // import-model needs a loaded project; SAFFRON_SCRATCH_PROJECT makes one (under the harness temp appdata dir).
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("boots clean: ping answers and no validation errors at startup", async () => {
  const pong = await engine.call<{ pong: boolean }>("ping");
  expect(pong.pong).toBe(true);
  expect(engine.validationErrors()).toEqual([]);
});

test("imports a model and reports a draw", async () => {
  await engine.call("add-entity", { preset: "cube" });
  const entities = await engine.call<EntityList>("list-entities");
  expect(Array.isArray(entities.entities)).toBe(true);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.drawCalls).toBeGreaterThan(0);
});

// setAa clamps the MSAA sample count to the R16G16B16A16_SFLOAT / D32_SFLOAT target formats'
// actual support, not the generic framebuffer limit, so the color/depth images never request a
// count those formats reject on some GPUs (VUID-VkImageCreateInfo-samples-02258).
test("set-aa across every level stays Vulkan-validation-clean", async () => {
  for (const mode of ["msaa2", "msaa4", "msaa8", "msaa2", "off"]) {
    const result = await engine.call<{ aa: string }>("set-aa", { args: [mode] });
    expect(typeof result.aa).toBe("string");
    await engine.settle(200);
  }
  expect(engine.validationErrors()).toEqual([]);
});
