// Temporal-upsampling (TAAU) control surface. `get-upscale` reports the render scale, the
// dynamic-resolution state, and the live input/display extents; `set-upscale` is a partial update
// (omitted fields hold). This is the single wire home of the render scale — separate from the
// blend/sharpen tunables on `set-taa-params`.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { GetUpscaleResult, SetUpscaleResult } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.settle(200);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("get-upscale reports a native ratio and a non-degenerate display extent", async () => {
  const { upscale } = await engine.call<GetUpscaleResult>("get-upscale");
  expect(upscale.ratio).toBeGreaterThan(0);
  expect(upscale.ratio).toBeLessThanOrEqual(1);
  expect(upscale.displayWidth).toBeGreaterThan(0);
  expect(upscale.displayHeight).toBeGreaterThan(0);
  // input = round(display × ratio).
  expect(upscale.inputWidth).toBeGreaterThan(0);
  expect(upscale.inputWidth).toBeLessThanOrEqual(upscale.displayWidth);
});

test("set-upscale pins the ratio and shrinks only the input extent; the display holds", async () => {
  const before = (await engine.call<GetUpscaleResult>("get-upscale")).upscale;

  const set = await engine.call<SetUpscaleResult>("set-upscale", { ratio: 0.5 });
  expect(set.upscale.ratio).toBeCloseTo(0.5, 3);
  // The display extent is fixed; the input extent halved.
  expect(set.upscale.displayWidth).toBe(before.displayWidth);
  expect(set.upscale.inputWidth).toBe(Math.round(before.displayWidth * 0.5));

  // Persisted across calls.
  const after = (await engine.call<GetUpscaleResult>("get-upscale")).upscale;
  expect(after.ratio).toBeCloseTo(0.5, 3);
});

test("set-upscale is a partial merge — an omitted field keeps its prior value", async () => {
  await engine.call<SetUpscaleResult>("set-upscale", { ratio: 0.67, dynamic: true });
  // A follow-up with only ratio must not clear the dynamic flag.
  const merged = await engine.call<SetUpscaleResult>("set-upscale", { ratio: 0.75 });
  expect(merged.upscale.ratio).toBeCloseTo(0.75, 3);
  expect(merged.upscale.dynamic).toBe(true);

  // Restore native + static for a clean teardown.
  await engine.call("set-upscale", { ratio: 1.0, dynamic: false });
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
