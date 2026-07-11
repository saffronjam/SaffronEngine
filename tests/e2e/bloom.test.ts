// Bloom control-plane end-to-end: drive a real headless engine, toggle the pre-tonemap bloom
// pyramid over the wire, and assert the `set-bloom` echo, the `render-stats` read-back, and a
// Vulkan-validation-clean log across the whole down/up/composite chain.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { RenderStats, SetBloomParams, SetBloomResult } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Size the scene view small so the bloom pyramid's compute dispatches stay cheap on the software
  // rasterizer the headless (weston) surface falls back to — the base pipeline plus ~13 bloom
  // dispatches at the default swapchain extent would otherwise starve the control drain on llvmpipe.
  await engine.call("set-viewport-size", { view: "scene", width: 480, height: 270 });
  // A bright cube so the pyramid has non-trivial HDR energy to spread (the validation oracle does
  // not depend on content, but this exercises the composite lerp on real radiance).
  await engine.call("add-entity", { preset: "cube" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("bloom is off by default in render-stats", async () => {
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.bloomEnabled).toBe(false);
});

test("set-bloom echoes the applied state", async () => {
  const params: SetBloomParams = {
    enabled: true,
    intensity: 0.08,
    scatter: 0.005,
    tint: [1, 1, 1],
    threshold: 0,
  };
  const res = await engine.call<SetBloomResult>("set-bloom", params);
  expect(res.enabled).toBe(true);
  expect(res.intensity).toBeCloseTo(0.08, 5);
  expect(res.scatter).toBeCloseTo(0.005, 5);
  expect(res.threshold).toBe(0);
});

test("render-stats reflects the enabled bloom + runs validation-clean", async () => {
  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.bloomEnabled).toBe(true);
  expect(stats.bloomIntensity).toBeCloseTo(0.08, 5);
  expect(engine.validationErrors()).toEqual([]);
});

// The whole pyramid (13-tap Karis downsample chain, tent upsample-add, composite into `color`)
// is a new set of transient-image compute passes and layout transitions; assert it stays
// validation-clean while the intensity is scrubbed and bloom is toggled back off.
describe("bloom stays validation-clean across parameter changes", () => {
  const INTENSITIES = [0.02, 0.15, 0.4];
  for (const intensity of INTENSITIES) {
    test(`intensity=${intensity}`, async () => {
      await engine.call<SetBloomResult>("set-bloom", {
        enabled: true,
        intensity,
        scatter: 0.008,
        tint: [1, 0.8, 0.6],
        threshold: 0,
      });
      await engine.settle(200);
      expect(engine.validationErrors()).toEqual([]);
    });
  }

  test("disabling bloom returns to a clean frame", async () => {
    const res = await engine.call<SetBloomResult>("set-bloom", {
      enabled: false,
      intensity: 0.05,
      scatter: 0.005,
      tint: [1, 1, 1],
      threshold: 0,
    });
    expect(res.enabled).toBe(false);
    await engine.settle(200);
    const stats = await engine.call<RenderStats>("render-stats");
    expect(stats.bloomEnabled).toBe(false);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// Phase 2 art direction: the lens-dirt mask (intensity + tint), the anamorphic streak (a
// horizontally-squeezed blur added over the radial bloom), and the per-mip tint stack all ride the
// one `set-bloom` command. Drive them together, assert the echo + `render-stats` read-back, and that
// the extra streak ping-pong passes + dirt composite stay validation-clean.
test("set-bloom applies dirt + anamorphic + per-mip tint and echoes them", async () => {
  const params: SetBloomParams = {
    enabled: true,
    intensity: 0.1,
    scatter: 0.008,
    tint: [1, 1, 1],
    threshold: 0,
    dirtIntensity: 0.6,
    dirtTint: [1, 0.9, 0.8],
    anamorphic: { enabled: true, ratio: 2, tint: [0.6, 0.8, 1], intensity: 0.4 },
    perMipTint: [
      [1, 0.6, 0.6],
      [0.6, 0.6, 1],
    ],
  };
  const res = await engine.call<SetBloomResult>("set-bloom", params);
  expect(res.dirtIntensity).toBeCloseTo(0.6, 5);
  expect(res.anamorphic.enabled).toBe(true);
  expect(res.anamorphic.ratio).toBeCloseTo(2, 5);
  expect(res.anamorphic.intensity).toBeCloseTo(0.4, 5);
  expect(res.perMipTint.length).toBe(2);

  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.bloomDirtIntensity).toBeCloseTo(0.6, 5);
  expect(stats.bloomAnamorphic.enabled).toBe(true);
  expect(stats.bloomAnamorphic.intensity).toBeCloseTo(0.4, 5);
  expect(stats.bloomPerMipTint.length).toBe(2);
  expect(engine.validationErrors()).toEqual([]);
});

test("disabling the anamorphic streak stays validation-clean", async () => {
  await engine.call<SetBloomResult>("set-bloom", {
    enabled: true,
    intensity: 0.08,
    scatter: 0.005,
    tint: [1, 1, 1],
    threshold: 0,
    anamorphic: { enabled: false, ratio: 2, tint: [0.6, 0.8, 1], intensity: 0 },
  });
  await engine.settle(200);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.bloomAnamorphic.enabled).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});

test("set-bloom rejects out-of-range parameters", async () => {
  await expect(
    engine.call("set-bloom", {
      enabled: true,
      intensity: -1,
      scatter: 0.005,
      tint: [1, 1, 1],
      threshold: 0,
    }),
  ).rejects.toThrow();
});
