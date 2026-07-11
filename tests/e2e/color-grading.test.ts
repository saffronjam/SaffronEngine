// Color-grading control-plane end-to-end: drive a real headless engine, set the scene-linear grade
// folded into the tonemap pass over the wire, and assert the `set-color-grading` echo, the
// `render-stats` read-back, and a Vulkan-validation-clean log across the graded tonemap pass.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import type {
  BakeLookResult,
  ImportLutResult,
  RenderStats,
  SetColorGradingParams,
  SetColorGradingResult,
} from "@saffron/protocol";

/// The neutral identity grade — every write spreads this so a test overrides only the fields it
/// exercises (the wire also fills missing fields from the same neutral default).
const NEUTRAL: SetColorGradingParams = {
  temperature: 6500,
  tint: 0,
  contrast: 1,
  pivot: 0.18,
  saturation: 1,
  slope: [1, 1, 1],
  offset: [0, 0, 0],
  power: [1, 1, 1],
  shadows: { slope: [1, 1, 1], offset: [0, 0, 0], power: [1, 1, 1], saturation: 1, contrast: 1 },
  midtones: { slope: [1, 1, 1], offset: [0, 0, 0], power: [1, 1, 1], saturation: 1, contrast: 1 },
  highlights: { slope: [1, 1, 1], offset: [0, 0, 0], power: [1, 1, 1], saturation: 1, contrast: 1 },
  shadowsMax: 0.09,
  highlightsMin: 0.5,
  channelMixer: [1, 0, 0, 0, 1, 0, 0, 0, 1],
  splitTone: { shadow: [0.5, 0.5, 0.5], highlight: [0.5, 0.5, 0.5], balance: 0 },
  creativeLutAsset: "0",
  creativeLutIntensity: 0,
};

/// Writes an `N³` `.cube` creative look (a mild warm shift) to a temp file and returns its path.
function writeCube(size: number): string {
  const dir = mkdtempSync(join(tmpdir(), "sa-cube-"));
  const path = join(dir, `warm${size}.cube`);
  let text = `TITLE "warm"\nLUT_3D_SIZE ${size}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n`;
  for (let b = 0; b < size; b++) {
    for (let g = 0; g < size; g++) {
      for (let r = 0; r < size; r++) {
        const rr = Math.min(1, (r / (size - 1)) * 1.1);
        const gg = g / (size - 1);
        const bb = (b / (size - 1)) * 0.9;
        text += `${rr.toFixed(5)} ${gg.toFixed(5)} ${bb.toFixed(5)}\n`;
      }
    }
  }
  writeFileSync(path, text);
  return path;
}

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Size the scene view small so the tonemap compute dispatch stays cheap on the software rasterizer
  // the headless (weston) surface may fall back to.
  await engine.call("set-viewport-size", { view: "scene", width: 480, height: 270 });
  // A bright cube so the grade has real HDR radiance to act on before the view transform.
  await engine.call("add-entity", { preset: "cube" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("the default grade is neutral in render-stats", async () => {
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.colorGrading.temperature).toBeCloseTo(6500, 1);
  expect(stats.colorGrading.tint).toBe(0);
  expect(stats.colorGrading.contrast).toBe(1);
  expect(stats.colorGrading.pivot).toBeCloseTo(0.18, 5);
  expect(stats.colorGrading.saturation).toBe(1);
  expect(stats.colorGrading.slope).toEqual([1, 1, 1]);
  expect(stats.colorGrading.offset).toEqual([0, 0, 0]);
  expect(stats.colorGrading.power).toEqual([1, 1, 1]);
  // The per-range / mixer / split defaults are identity too.
  expect(stats.colorGrading.shadows.power).toEqual([1, 1, 1]);
  expect(stats.colorGrading.shadowsMax).toBeCloseTo(0.09, 5);
  expect(stats.colorGrading.highlightsMin).toBeCloseTo(0.5, 5);
  expect(stats.colorGrading.channelMixer).toEqual([1, 0, 0, 0, 1, 0, 0, 0, 1]);
  expect(stats.colorGrading.splitTone.shadow).toEqual([0.5, 0.5, 0.5]);
  expect(stats.colorGrading.splitTone.balance).toBe(0);
});

test("set-color-grading echoes the applied grade", async () => {
  const params: SetColorGradingParams = {
    ...NEUTRAL,
    temperature: 5000,
    tint: 0.1,
    contrast: 1.2,
    saturation: 0.9,
    slope: [1.05, 1, 0.95],
    offset: [0, 0.01, 0.02],
    power: [1, 1.05, 1.1],
  };
  const res = await engine.call<SetColorGradingResult>("set-color-grading", params);
  expect(res.temperature).toBeCloseTo(5000, 1);
  expect(res.tint).toBeCloseTo(0.1, 5);
  expect(res.contrast).toBeCloseTo(1.2, 5);
  expect(res.saturation).toBeCloseTo(0.9, 5);
  expect(res.slope[0]).toBeCloseTo(1.05, 5);
  expect(res.power[2]).toBeCloseTo(1.1, 5);
});

test("render-stats reflects the applied grade + runs validation-clean", async () => {
  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.colorGrading.temperature).toBeCloseTo(5000, 1);
  expect(stats.colorGrading.contrast).toBeCloseTo(1.2, 5);
  expect(stats.colorGrading.saturation).toBeCloseTo(0.9, 5);
  expect(engine.validationErrors()).toEqual([]);
});

// The per-range Shadows/Midtones/Highlights CDL, the 3x3 channel mixer, and split-toning ride the same
// grade UBO. Write a full highlights block + a channel-mixer row + a split tone and assert every field
// round-trips through the echo and `render-stats`, validation-clean.
test("per-range, channel mixer, and split-tone round-trip through render-stats", async () => {
  const params: SetColorGradingParams = {
    ...NEUTRAL,
    highlights: {
      slope: [0.9, 1, 1.1],
      offset: [0, 0.02, 0],
      power: [1.1, 1, 0.9],
      saturation: 1.2,
      contrast: 1.15,
    },
    shadowsMax: 0.12,
    highlightsMin: 0.55,
    channelMixer: [1, 0.1, 0, 0, 1, 0, 0.05, 0, 1],
    splitTone: { shadow: [0.4, 0.45, 0.6], highlight: [0.6, 0.55, 0.4], balance: 0.1 },
  };
  const res = await engine.call<SetColorGradingResult>("set-color-grading", params);
  expect(res.highlights.slope[0]).toBeCloseTo(0.9, 5);
  expect(res.highlights.power[2]).toBeCloseTo(0.9, 5);
  expect(res.highlights.saturation).toBeCloseTo(1.2, 5);
  expect(res.shadowsMax).toBeCloseTo(0.12, 5);
  expect(res.channelMixer[1]).toBeCloseTo(0.1, 5);
  expect(res.splitTone.shadow[2]).toBeCloseTo(0.6, 5);
  expect(res.splitTone.balance).toBeCloseTo(0.1, 5);

  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.colorGrading.highlights.slope[0]).toBeCloseTo(0.9, 5);
  expect(stats.colorGrading.highlights.contrast).toBeCloseTo(1.15, 5);
  expect(stats.colorGrading.highlightsMin).toBeCloseTo(0.55, 5);
  expect(stats.colorGrading.channelMixer[6]).toBeCloseTo(0.05, 5);
  expect(stats.colorGrading.splitTone.highlight[0]).toBeCloseTo(0.6, 5);
  expect(engine.validationErrors()).toEqual([]);
});

// The graded tonemap pass folds white balance + contrast + saturation + ASC-CDL in front of the view
// transform, driven by a per-view dynamic-offset UBO; assert it stays validation-clean while the
// grade is scrubbed and returned to neutral.
describe("the grade stays validation-clean across parameter changes", () => {
  const TEMPS = [3200, 6500, 9000];
  for (const temperature of TEMPS) {
    test(`temperature=${temperature}`, async () => {
      await engine.call<SetColorGradingResult>("set-color-grading", {
        ...NEUTRAL,
        temperature,
        contrast: 1.1,
      });
      await engine.settle(200);
      expect(engine.validationErrors()).toEqual([]);
    });
  }

  test("returning to the neutral grade is validation-clean", async () => {
    const res = await engine.call<SetColorGradingResult>("set-color-grading", { ...NEUTRAL });
    expect(res.temperature).toBeCloseTo(6500, 1);
    await engine.settle(200);
    const stats = await engine.call<RenderStats>("render-stats");
    expect(stats.colorGrading.contrast).toBe(1);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// A partial `sa`-style call folds provided keys onto the neutral serde default.
test("a partial grade leaves the unspecified fields neutral", async () => {
  const res = await engine.call<SetColorGradingResult>("set-color-grading", { temperature: 5500 });
  expect(res.temperature).toBeCloseTo(5500, 1);
  expect(res.contrast).toBe(1);
  expect(res.saturation).toBe(1);
  expect(res.slope).toEqual([1, 1, 1]);
  // The per-range / mixer / split fields fall back to their neutral defaults too.
  expect(res.highlights.power).toEqual([1, 1, 1]);
  expect(res.channelMixer).toEqual([1, 0, 0, 0, 1, 0, 0, 0, 1]);
  expect(res.splitTone.balance).toBe(0);
});

test("set-color-grading rejects an out-of-range pivot", async () => {
  await expect(engine.call("set-color-grading", { ...NEUTRAL, pivot: 0 })).rejects.toThrow();
});

// The display-space creative `.cube` LUT slot rides the same `set-color-grading` command: import a
// 17³ look, apply it at intensity 0.8, and assert the resolved size/intensity read-back on
// `render-stats.creativeLut`, then that intensity 0 reports the neutral look — validation-clean.
test("a creative .cube LUT imports, applies, and reads back its size/intensity", async () => {
  const path = writeCube(17);
  const imported = await engine.call<ImportLutResult>("import-lut", { path });
  expect(imported.lut).not.toBe("0");

  await engine.call<SetColorGradingResult>("set-color-grading", {
    ...NEUTRAL,
    creativeLutAsset: imported.lut,
    creativeLutIntensity: 0.8,
  });
  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.creativeLut).not.toBeNull();
  expect(stats.creativeLut?.asset).toBe(imported.lut);
  expect(stats.creativeLut?.size).toBe(17);
  expect(stats.creativeLut?.intensity).toBeCloseTo(0.8, 5);
  expect(engine.validationErrors()).toEqual([]);

  // Intensity 0 is the neutral (the LUT stays bound, one code path); the look reverts.
  await engine.call<SetColorGradingResult>("set-color-grading", {
    ...NEUTRAL,
    creativeLutAsset: imported.lut,
    creativeLutIntensity: 0,
  });
  await engine.settle(200);
  const neutral = await engine.call<RenderStats>("render-stats");
  expect(neutral.creativeLut?.intensity).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});

// The editor Post panel's ToneCurve bakes its per-channel display-space spline into a red-fastest
// `.cube` (per-channel then master) and rides the Phase-5 creative-LUT slot: no new command, no engine
// change. Reproduce that exact output — a master curve that lifts the mids, sampled 17³ red-fastest —
// then import + apply it through `set-color-grading` and assert the read-back, validation-clean.
function writeToneCurveCube(size: number): string {
  const dir = mkdtempSync(join(tmpdir(), "sa-tonecurve-"));
  const path = join(dir, `tone-curve${size}.cube`);
  // A mild S-lift: lift the low-mids, so the sampled table is a monotone non-identity ramp.
  const curve = (t: number): number => Math.min(1, Math.max(0, Math.pow(t, 0.8)));
  let text = `TITLE "tone-curve"\nLUT_3D_SIZE ${size}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n`;
  for (let b = 0; b < size; b++) {
    for (let g = 0; g < size; g++) {
      for (let r = 0; r < size; r++) {
        text += `${curve(r / (size - 1)).toFixed(5)} ${curve(g / (size - 1)).toFixed(5)} ${curve(
          b / (size - 1),
        ).toFixed(5)}\n`;
      }
    }
  }
  writeFileSync(path, text);
  return path;
}

test("a tone-curve-baked .cube imports and applies through the creative-LUT slot", async () => {
  const path = writeToneCurveCube(17);
  const imported = await engine.call<ImportLutResult>("import-lut", { path });
  expect(imported.lut).not.toBe("0");

  await engine.call<SetColorGradingResult>("set-color-grading", {
    ...NEUTRAL,
    creativeLutAsset: imported.lut,
    creativeLutIntensity: 1,
  });
  await engine.settle(300);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.creativeLut?.asset).toBe(imported.lut);
  expect(stats.creativeLut?.size).toBe(17);
  expect(stats.creativeLut?.intensity).toBeCloseTo(1, 5);
  expect(engine.validationErrors()).toEqual([]);

  // Clear the slot so the tone-curve look does not leak into the bake test below.
  await engine.call<SetColorGradingResult>("set-color-grading", { ...NEUTRAL });
});

// The bake folds grade + view transform + creative LUT into one 33³ log2-shaper `.slut` on the GPU,
// reads it back, and registers it — validation-clean.
test("bake-look writes a 33³ .slut and stays validation-clean", async () => {
  const baked = await engine.call<BakeLookResult>("bake-look", { name: "E2E Baked" });
  expect(baked.size).toBe(33);
  expect(baked.path).toMatch(/\.slut$/);
  expect(baked.asset).not.toBe("0");
  await engine.settle(200);
  expect(engine.validationErrors()).toEqual([]);
});
