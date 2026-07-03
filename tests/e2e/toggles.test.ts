// Render-feature toggles, validated by READ-BACK: flip each via its set-* command, then
// query render-stats and assert the reported state actually changed — "ok:true" alone proves
// nothing. Most of these recreate GPU pipelines/targets, so the suite also asserts the engine
// stays Vulkan-validation-clean (the oracle that caught the MSAA sample-count bug).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { RenderStats } from "@saffron/protocol";


let engine: Engine;
const stats = () => engine.call<RenderStats & Record<string, unknown>>("render-stats");

beforeAll(async () => {
  // import-model needs a loaded project; SAFFRON_SCRATCH_PROJECT makes one (under the harness temp appdata dir).
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("add-entity", { preset: "cube" }); // geometry, so the passes actually run
});
afterAll(async () => {
  await engine?.shutdown();
});

// Plain on/off toggles whose state render-stats echoes back under `field`. The SSGI / GTAO /
// contact-shadow effects are no longer per-effect toggles — they are driven by the render-quality
// tier (see the `set-render-quality` test below).
const BOOLEAN_TOGGLES = [
  { cmd: "set-shadows", field: "shadows" },
  { cmd: "set-ibl", field: "ibl" },
  { cmd: "set-clustered", field: "clustered" },
  { cmd: "set-depth-prepass", field: "depthPrepass" },
];

for (const { cmd, field } of BOOLEAN_TOGGLES) {
  test(`${cmd} round-trips through render-stats`, async () => {
    await engine.call(cmd, { args: [1] });
    expect((await stats())[field]).toBe(true);
    await engine.call(cmd, { args: [0] });
    expect((await stats())[field]).toBe(false);
  });
}

test("set-render-quality drives the SSGI/GTAO/contact stack via render-stats", async () => {
  await engine.call("set-render-quality", { args: ["low"] });
  let s = await stats();
  expect(s.quality).toBe("low");
  expect(s.ssgi).toBe(false);
  expect(s.ssao).toBe(false);
  expect(s.contactShadows).toBe(false);

  await engine.call("set-render-quality", { args: ["ultra"] });
  s = await stats();
  expect(s.quality).toBe("ultra");
  expect(s.ssgi).toBe(true);
  expect(s.ssao).toBe(true);
  expect(s.contactShadows).toBe(true);
});

test("set-gi ddgi|off round-trips through render-stats.ddgi", async () => {
  await engine.call("set-gi", { args: ["ddgi"] });
  expect((await stats()).ddgi).toBe(true);
  await engine.call("set-gi", { args: ["off"] });
  expect((await stats()).ddgi).toBe(false);
});

test("set-exposure is reflected in render-stats.exposureEv", async () => {
  const r = await engine.call<{ exposureEv: number }>("set-exposure", { args: [1.5] });
  expect(r.exposureEv).toBeCloseTo(1.5, 3);
  expect((await stats()).exposureEv).toBeCloseTo(1.5, 3);
  await engine.call("set-exposure", { args: [0] });
});

test("ray-tracing toggles round-trip when the device supports RT", async () => {
  const s = await stats();
  if (!s.rtSupported) {
    return; // llvmpipe without ray_query / real GPU without RT — nothing to assert
  }
  await engine.call("set-rt-shadows", { args: [1] });
  expect((await stats()).rtShadows).toBe(true);
  await engine.call("set-restir", { args: [1] });
  expect((await stats()).restir).toBe(true);
  await engine.call("set-rt-shadows", { args: [0] });
  await engine.call("set-restir", { args: [0] });
});

// Runs last: by now every toggle has recreated its GPU state at least once.
test("exercising every toggle left no Vulkan validation errors", async () => {
  await engine.settle(400);
  expect(engine.validationErrors()).toEqual([]);
});
