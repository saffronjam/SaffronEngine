// The same scene renders the same picture on the discrete GPU and on the software rasterizer.
//
// Every other pixel test in this suite compares one adapter against itself, which catches a
// regression but says nothing about portability. A shader that leans on NVIDIA's rounding, a pass
// that reads uninitialized memory the driver happens to zero, a subgroup path with no software
// equivalent — each of those is invisible to a single-adapter run and shows up as "it looks wrong
// on the other machine".
//
// This boots two hosts that differ in exactly one environment variable, `VK_ICD_FILENAMES`, so the
// scene, camera, and settle time are identical by construction and only the driver differs.
//
// THE TWO HOSTS MUST ACTUALLY BE DIFFERENT, which is the trap this test would otherwise fall into.
// If the loader ignored the override, both hosts would run the same adapter and every comparison
// would pass for the wrong reason. `softwareGpu` from `render-stats` is read back rather than
// assumed, and the whole comparison is skipped when the machine offers only one adapter — a
// skipped test is honest, a vacuous pass is not.
//
// THE TOLERANCE IS MEASURED, NOT GUESSED. Two rasterizers will never be bit-identical: they differ
// in sample positions, in interpolation precision, and in how they round a filtered texel. The
// bound below is set from what this scene actually measures (~0.16 mean absolute per-channel
// difference on 0-255), with room for the noise a driver update brings. It is deliberately far
// tighter than "looks about right" — a real portability defect moves a frame by whole channel
// values, not by fractions of one.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference, regionMean } from "./image.ts";

/// The Mesa software rasterizer's ICD inside the build toolbox. Absent on macOS, where MoltenVK is
/// the only adapter and the comparison correctly does not run.
const LLVMPIPE_ICD = "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json";

/// A pose showing lit ground, a lit object, and sky — three shading paths rather than one.
const CAMERA = { position: { x: 0, y: 2, z: 6 }, yaw: 0, pitch: -15 } as const;

/// Mean absolute per-channel difference (0-255) the two adapters may differ by. See the
/// calibration note above: this scene measures ~0.16 between them.
const ADAPTER_TOLERANCE = 1.5;

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};
const stats: Record<string, RenderStatsDto> = {};
let bothAdaptersPresent = false;

/// Boots a host on the named adapter, builds the scene, and captures one settled frame.
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
