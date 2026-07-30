// The same scene renders the same picture on the discrete GPU and on the software rasterizer.
//
// Every other pixel test compares one adapter against itself, which says nothing about
// portability: a shader leaning on one vendor's rounding, a pass reading memory the driver happens
// to zero, a subgroup path with no software equivalent. This boots two hosts differing only in
// `VK_ICD_FILENAMES`, so scene, camera, and settle are identical by construction.
//
// `softwareGpu` is read back from `render-stats` rather than assumed, and the comparison is skipped
// when the machine offers one adapter — if the loader ignored the override, both hosts would run
// the same driver and every comparison would pass for the wrong reason.
//
// The tolerance is measured, not guessed: two rasterizers differ in sample positions,
// interpolation precision, and filtered-texel rounding, and this scene measures ~0.16 mean
// absolute per-channel difference on 0-255. A real portability defect moves whole channel values.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference, regionMean } from "./image.ts";

// The Mesa software rasterizer's ICD inside the build toolbox. Absent on macOS, where MoltenVK is
// the only adapter and the comparison correctly does not run.
const LLVMPIPE_ICD = "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json";

// A pose showing lit ground, a lit object, and sky — three shading paths rather than one.
const CAMERA = { position: { x: 0, y: 2, z: 6 }, yaw: 0, pitch: -15 } as const;

// Mean absolute per-channel difference (0-255) the two adapters may differ by. See the
// calibration note above: this scene measures ~0.16 between them.
const ADAPTER_TOLERANCE = 1.5;

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};
const stats: Record<string, RenderStatsDto> = {};
let bothAdaptersPresent = false;

// Boots a host on the named adapter, builds the scene, and captures one settled frame.
async function captureOn(label: string, env: Record<string, string>): Promise<Buffer> {
  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1", ...env });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 320, height: 180, camera: CAMERA });
  await engine.call("add-entity", { preset: "plane" });
  const cube = await engine.call<{ id: string }>("add-entity", { preset: "cube" });
  await engine.call("set-component", {
    entity: cube.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 1, z: 0 },
      scale: { x: 1, y: 1, z: 1 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  // A still field: this is a portability comparison, not a wind one, and the two hosts advance
  // their clocks independently.
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1500);
  const frame = await captureViewport(engine, cleaner, `adapter-${label}`);
  stats[label] = await engine.call<RenderStatsDto>("render-stats");
  // A portability difference often shows as a validation error on one adapter and silence on the
  // other, so each host is asserted clean on its own rather than only compared to the other.
  expect(engine.validationErrors()).toEqual([]);
  return frame;
}

beforeAll(async () => {
  frames.native = await captureOn("native", {});
  frames.software = await captureOn("software", { VK_ICD_FILENAMES: LLVMPIPE_ICD });
  // The override may be ignored, or the machine may offer one adapter. Either way the comparison
  // is between a host and itself, which proves nothing about portability.
  bothAdaptersPresent = stats.native.softwareGpu !== stats.software.softwareGpu;
}, 240_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the two hosts really are different adapters", () => {
  if (!bothAdaptersPresent) {
    // Reported rather than silently green: a machine with one adapter cannot run this comparison,
    // and pretending otherwise is how a portability suite stops meaning anything.
    console.warn(
      `cross-adapter parity skipped: both hosts reported softwareGpu=${stats.native.softwareGpu}`,
    );
    return;
  }
  expect(stats.software.softwareGpu).toBe(true);
  expect(stats.native.softwareGpu).toBe(false);
});

test("both adapters render the scene at the same extent", () => {
  const native = decodeRgb8Png(frames.native);
  const software = decodeRgb8Png(frames.software);
  expect(native.width).toBe(software.width);
  expect(native.height).toBe(software.height);
});

test("the whole frame agrees within the measured tolerance", () => {
  if (!bothAdaptersPresent) {
    return;
  }
  const native = decodeRgb8Png(frames.native);
  const software = decodeRgb8Png(frames.software);
  expect(meanAbsoluteDifference(native, software)).toBeLessThanOrEqual(ADAPTER_TOLERANCE);
});

test("each shading region agrees, not just the frame average", () => {
  if (!bothAdaptersPresent) {
    return;
  }
  // A whole-frame mean hides a localized defect: a wrong object against a large correct sky
  // averages down to nothing. Scoring the regions separately is what catches one pass diverging.
  const native = decodeRgb8Png(frames.native);
  const software = decodeRgb8Png(frames.software);
  const regions: Array<[string, { x: number; y: number; width: number; height: number }]> = [
    ["sky", { x: 0, y: 0, width: 320, height: 40 }],
    ["object", { x: 120, y: 60, width: 80, height: 70 }],
    ["ground", { x: 0, y: 150, width: 320, height: 30 }],
  ];
  for (const [name, region] of regions) {
    const delta = Math.abs(regionMean(native, region) - regionMean(software, region));
    expect({ region: name, agrees: delta <= ADAPTER_TOLERANCE }).toEqual({
      region: name,
      agrees: true,
    });
  }
});
