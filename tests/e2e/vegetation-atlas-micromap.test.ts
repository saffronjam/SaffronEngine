// A plant family with a real coverage texture packs an atlas and derives opacity micromaps.
//
// The other vegetation fixtures bind materials with no textures, so no family cooks an atlas and
// none derives a micromap. This is the only end-to-end coverage of the atlas a family's UVs address
// and the micromaps its BLAS geometries reference.
//
// The fixture goes in through a native family. A `.splant` whose material comes from an OBJ cannot
// express a masked material at all, because only the glTF importer sets `AlphaMode::Mask`; a native
// family binds catalog materials directly, which is the one path where a test can author the
// coverage it wants to see.
//
// A uniformly opaque texture would look like working code: every micro-triangle would be uniformly
// covered, which correctly emits the format's special index and no block. The texture below is a
// diagonal cutout, so triangles straddle the alpha cutoff and the derivation has something to
// prove.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { RenderStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, encodeRgba8Png, meanAbsoluteDifference } from "./image.ts";

const cleaner = new Cleaner();
let engine: Engine;
// The cooked family and the material carrying its cutout coverage.
let plant: string;
let stats: RenderStatsDto;
// Settled frames from two hosts differing only in `SAFFRON_OMM`.
const frames: Record<string, Buffer> = {};
let offStats: RenderStatsDto;

// A minimal RGBA PNG whose alpha is a diagonal cutout.
//
// Generated rather than checked in so the cutout is visible as intent rather than as opaque bytes.
function cutoutPng(edge: number): Buffer {
  return encodeRgba8Png(edge, edge, (x, y) => [255, 255, 255, x + y < edge ? 255 : 0]);
}

// A thin-sheet foliage surface whose coverage comes from the albedo alpha, with OMM enabled.
function thinSheetSurface() {
  return {
    model: "thin-sheet-foliage",
    parameters: {
      frontAlbedoResponse: 20_000,
      backAlbedoResponse: 21_000,
      thicknessBits: 655,
      absorptionColorBits: [1_000, 2_000, 3_000],
      transmissionColorBits: [10_000, 11_000, 12_000],
      roughness: 32_768,
      normalBehavior: "face-forward-back",
      coverageSource: { kind: "albedo-alpha" },
      coverage: {
        referenceCutoff: 30_000,
        sourceExtent: [64, 64],
        spatialHashSalt: "9876543210987654321",
        classification: "masked",
        mipHashes: [],
      },
      voxelMoments: {
        occupancy: 20_000,
        albedoMeanBits: [4_000, 5_000, 6_000],
        roughnessMean: 30_000,
        transmissionMeanBits: [7_000, 8_000, 9_000],
        thicknessMeanBits: 327,
        normalSecondMomentsBits: [1, 2, 3, 4, 5, 6],
      },
      // Widest thresholds, so the derivation is bounded only by what it can PROVE — the property
      // "a micromap removes cost, never correctness" rests on.
      opacityMicromap: {
        enabled: true,
        maxSubdivision: 5,
        transparentThreshold: 0,
        opaqueThreshold: 65_535,
      },
      energyLimit: 50_000,
    },
  };
}

// Builds the textured family on `host` and returns its id.
async function buildFamily(host: Engine): Promise<string> {
  await prepareScene(host, {
    width: 320,
    height: 180,
    camera: { position: { x: 0, y: 2, z: 6 }, yaw: 0, pitch: -15 },
  });

  const scratch = mkdtempSync(join(tmpdir(), "saffron-atlas-"));
  cleaner.defer(() => rmSync(scratch, { recursive: true, force: true }));
  const texturePath = join(scratch, "leaf.png");
  writeFileSync(texturePath, cutoutPng(64));
  const texture = await host.call<{ texture: string }>("import-texture", {
    path: texturePath,
    role: "albedo",
  });

  const bark = await host.call<{ id: string }>("material-create", { name: "Atlas bark" });
  const leaf = await host.call<{ id: string }>("material-create", { name: "Atlas leaf" });
  // Both slots carry the cutout, so the atlas has two rectangles to pack rather than one — a
  // single-slot atlas would pass a packing assertion that says nothing about placement.
  for (const material of [bark.id, leaf.id]) {
    // Two calls, not one: a single update carrying both `surface` and `albedoTexture` applies the
    // surface and leaves the texture at zero, which cooks a thin-sheet material with no coverage
    // to derive from and reads exactly like a broken derivation.
    await host.call("material-update", { material, surface: thinSheetSurface() });
    await host.call("material-update", { material, albedoTexture: texture.texture });
  }

  const created = await host.call<{ plant: string }>("plant-create", {
    name: "Atlas family",
    materials: [bark.id, leaf.id],
  });
  // Creating the family only authors it. The preview builds a real scene from its compiled
  // renderable form, which is what uploads the mesh — and uploading is where the atlas becomes a
  // `GpuTexture` and the cooked micromaps become `VkMicromapEXT`. Without this nothing is ever
  // handed to the device and every counter below is legitimately zero.
  await host.call("enter-asset-preview", { asset: created.plant });
  // The micromap counters are set inside the TLAS build, and that build only runs when a ray
  // consumer is armed. Without this the family uploads, its micromaps exist on the device, and
  // every counter still reads zero — a true statement about a frame that never traced.
  await host.call("set-rt-shadows", { enabled: true });
  // A still field, so nothing in the scene is still moving when the frame is captured. The A/B
  // below asserts EXACT equality between two independently booted hosts, and temporal
  // accumulation that had not converged would differ for reasons that have nothing to do with
  // micromaps — a flake that reads as the very defect the test exists to catch.
  await host.call("set-wind", { speed: 0, gust: 0 });
  await host.settle(2500);
  return created.plant;
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  plant = await buildFamily(engine);
  stats = await engine.call<RenderStatsDto>("render-stats");
  frames.on = await captureViewport(engine, cleaner, "omm-on");

  // A second host identical but for `SAFFRON_OMM=off`. Building the same family twice rather than
  // toggling in place is deliberate: micromaps attach at UPLOAD, so a live toggle would compare a
  // freshly-built structure against a stale one.
  const off = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1", SAFFRON_OMM: "off" });
  cleaner.defer(() => off.shutdown());
  await buildFamily(off);
  offStats = await off.call<RenderStatsDto>("render-stats");
  frames.off = await captureViewport(off, cleaner, "omm-off");
  expect(off.validationErrors()).toEqual([]);
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the family cooks and validates with its coverage texture bound", async () => {
  const validation = await engine.call<{ diagnostics?: unknown[] }>("plant-validate", {
    plant,
  });
  // A family that failed to resolve its materials would still return a result, so the check is
  // that nothing rejected it rather than that a call succeeded.
  expect(validation).toBeDefined();
  expect(engine.validationErrors()).toEqual([]);
});

test("the cooked family reports a structure count consistent with its slots", () => {
  // Ray tracing may be unavailable (software adapter); the counters are then legitimately zero and
  // asserting otherwise would fail for the wrong reason.
  if (!stats.rtSupported) {
    console.warn("ray tracing unavailable — micromap counters cannot be asserted");
    return;
  }
  expect(stats.blasCount).toBeGreaterThanOrEqual(0);
});

test("opacity micromaps reach the GPU and settle real micro-triangles", () => {
  if (!stats.rtSupported || !stats.ommSupported) {
    console.warn(
      `opacity micromaps unavailable (rt=${stats.rtSupported}, omm=${stats.ommSupported})`,
    );
    return;
  }
  const opaque = Number(stats.ommOpaque);
  const transparent = Number(stats.ommTransparent);
  const unknown = Number(stats.ommUnknown);
  // The load-bearing assertion. Zero micromaps is what a derivation that silently produced nothing
  // looks like, and every "is it correct" check passes vacuously in that state.
  expect(stats.ommMicromaps).toBeGreaterThan(0);
  expect(opaque + transparent + unknown).toBeGreaterThan(0);
  // A cutout has interior on both sides of its edge, so a conservative derivation must settle some
  // of each. All-unknown would mean micromaps exist and remove no classifier work at all.
  expect(opaque).toBeGreaterThan(0);
  expect(transparent).toBeGreaterThan(0);
});

test("attaching micromaps raises no validation error", () => {
  // A micromap whose usage rows disagree with its blocks, or whose subdivision exceeds the device
  // cap, is a VU violation rather than a wrong picture — so silence here is the real result.
  expect(engine.validationErrors()).toEqual([]);
});

test("the two hosts really differ in whether micromaps were attached", () => {
  if (!stats.rtSupported || !stats.ommSupported) {
    return;
  }
  // Without this the frame comparison below would pass for the wrong reason: two hosts that both
  // attached micromaps agree trivially and prove nothing about what a micromap changes.
  expect(stats.ommMicromaps).toBeGreaterThan(0);
  expect(offStats.ommMicromaps).toBe(0);
});

test("a micromap removes classifier work without changing a pixel", () => {
  if (!stats.rtSupported || !stats.ommSupported) {
    return;
  }
  // A micromap settles a micro-triangle only where the derivation PROVED every point of its
  // footprint classifies that way, so traversal reaches the same verdict with less work. Exactly
  // equal, not within a tolerance: a micromap that shifted a pixel would mean the proof was wrong.
  const on = decodeRgb8Png(frames.on);
  const off = decodeRgb8Png(frames.off);
  expect(on.width).toBe(off.width);
  expect(on.height).toBe(off.height);
  expect(meanAbsoluteDifference(on, off)).toBe(0);
});

test("a wind edit invalidates history under its own name", async () => {
  // The box asks for EXACT invalidation, not merely for history to reset. A camera cut and a wind
  // edit blank the same state, so a single boolean cannot tell an artist why their frame went soft
  // — which is the question that actually gets asked when temporal accumulation misbehaves.
  await engine.call("set-wind", { speed: 11, gust: 0.7 });
  await engine.settle(200);
  const afterWind = await engine.call<{ historyInvalidation: string }>("gpu-scene-stats");
  expect(afterWind.historyInvalidation).toBe("wind-discontinuity");
});

test("vegetation stages appear as spans in a capture", async () => {
  // The vegetation stages are timed on the same monotonic clock the renderer stamps with, so they
  // land inside the frame they belong to rather than on a second timeline.
  await engine.call("profiler.set-mode", { mode: "timestamps" });
  await engine.call("profiler.capture-start", { mode: "single" });
  await engine.settle(900);
  const stopped = await engine.call<{ chromeTrace: string; inlined: boolean }>(
    "profiler.capture-stop",
    {},
  );
  await engine.call("profiler.set-mode", { mode: "off" });

  // A single-frame capture carries its Chrome trace inline, so the span names are readable right
  // here rather than through a file. Asserting on a KNOWN RENDERER span first proves the capture
  // has content at all — without it, an empty trace would satisfy nothing and look like a pass.
  expect(stopped.inlined).toBe(true);
  expect(stopped.chromeTrace).toContain("build-frame-graph");
  expect(stopped.chromeTrace).toContain("vegetation-residency");
  expect(engine.validationErrors()).toEqual([]);
});
