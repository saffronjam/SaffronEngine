// The KHR any-hit path running over canonical coverage on real ray-tracing hardware.
//
// The blocker carries a thin-sheet-foliage surface with an `albedo-alpha` coverage source and a
// `masked` classification, so its instance packs FORCE_NO_OPAQUE and its triangles surface as
// non-opaque ray candidates rather than auto-committing. `gpuSceneRayCandidateCovered` then
// reconstructs the hit and runs `classifyCanonicalCoverage`; for an albedo-alpha source the verdict
// is `sampled * baseColorAlpha` against the reference cutoff, and with no albedo texture bound
// `sampled` is the default-white 1.0, so the base-colour alpha alone decides.
//
// Shadow maps are off for the duration, so the only thing darkening the receiver is the ray
// shadow. Sharing the receiver with the raster shadow would make the measurement meaningless: the
// blocker's coverage cuts it out of the shadow-map pass too, so the patch would brighten whether or
// not a single ray candidate was ever classified.
//
// The assertion samples the receiver patch the ray shadow falls on, not the whole frame: the
// blocker's own shading also responds to its coverage, so a frame-wide comparison cannot separate
// "the candidate was rejected" from "the blocker looks different". The guard case pins the patch to
// the ray shadow — with ray-query shadows disabled it reads as bright as a rejected candidate
// leaves it.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { MaterialSurfaceDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene, trackEntity } from "./test-utils.ts";
import { type Rgb8Image, decodeRgb8Png, regionMean } from "./image.ts";

const cleaner = new Cleaner();
let engine: Engine;
let rtSupported = false;
let blocker = "";
let material = "";

// A thin-sheet foliage surface whose coverage comes from albedo alpha and is `masked`, so the
// classifier — not the geometry — decides whether a ray candidate commits.
const MASKED_THIN_SHEET: MaterialSurfaceDto = {
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
      sourceExtent: [512, 256],
      spatialHashSalt: "9876543210987654321",
      classification: "masked",
      mipHashes: [],
    },
    voxelMoments: {
      occupancy: 32_768,
      albedoMeanBits: [16_384, 16_384, 16_384],
      roughnessMean: 32_768,
      transmissionMeanBits: [8_192, 8_192, 8_192],
      thicknessMeanBits: 655,
      normalSecondMomentsBits: [0, 0, 0, 0, 0, 0],
    },
    opacityMicromap: {
      enabled: false,
      maxSubdivision: 0,
      transparentThreshold: 0,
      opaqueThreshold: 65_535,
    },
    energyLimit: 65_535,
  },
};

async function setBlockerAlpha(alpha: number): Promise<void> {
  await engine.call("material-update", {
    material,
    surface: MASKED_THIN_SHEET,
    baseColor: { x: 0.5, y: 0.5, z: 0.5, w: alpha },
  });
  await engine.settle(400);
}

// The receiver patch the candidate-driven ray shadow lands on, in frame fractions so it survives a
// viewport size change. Located by diffing a ray-shadowed frame against one with ray shadows off,
// which is the same thing the guard case asserts.
function shadowPatch(frame: Rgb8Image) {
  return {
    x: Math.floor(frame.width * 0.465),
    y: Math.floor(frame.height * 0.557),
    width: Math.max(1, Math.floor(frame.width * 0.037)),
    height: Math.max(1, Math.floor(frame.height * 0.053)),
  };
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { position: { x: 0, y: 5, z: 9 }, yaw: 0, pitch: -25 } });
  rtSupported = (await engine.call("render-stats")).rtSupported;
  if (!rtSupported) {
    return;
  }
  // A receiver to catch the shadow, and a blocker between it and the sun.
  trackEntity(cleaner, engine, await engine.call("add-entity", { preset: "plane" }));
  const cube = trackEntity(
    cleaner,
    engine,
    await engine.call("add-entity", { preset: "cube" }),
  );
  blocker = cube.id;
  await engine.call("set-component", {
    entity: blocker,
    component: "Transform",
    json: {
      translation: { x: 0, y: 2, z: 0 },
      scale: { x: 2, y: 0.1, z: 2 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  const created = await engine.call("material-create", { name: "AnyHitLeaf" });
  material = created.id;
  await engine.call("material-assign", { entity: blocker, material });
  await engine.call("set-shadows", { enabled: false });
  await engine.call("set-rt-shadows", { enabled: true });
  await engine.settle(500);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("ray-query shadows are armed and the blocker is in the TLAS", async () => {
  if (!rtSupported) {
    return;
  }
  const stats = await engine.call("render-stats");
  expect(stats.rtShadows).toBe(true);
  expect(stats.rtInstances).toBeGreaterThan(0);
  expect(stats.blasCount).toBeGreaterThan(0);
  // Every instance in this scene is a mirrored entity, so every one of them resolves its
  // candidates against a scene record rather than committing unconditionally. The two
  // representations that legitimately resolve nothing — a family-space aggregate and a
  // generated-topology structure, neither of which names a submesh — are absent here, and the
  // difference says so instead of leaving a bare equality to rot once one of them appears.
  expect(stats.rtAggregateInstances).toBe(0);
  expect(stats.tessellatedBlasCount).toBe(0);
  expect(stats.rtResolvableInstances).toBe(
    stats.rtInstances - stats.rtAggregateInstances - stats.tessellatedBlasCount,
  );
});

test("a rejected coverage candidate lets the shadow ray through; a committed one shadows", async () => {
  if (!rtSupported) {
    return;
  }
  // Alpha 1.0 → `1.0 * 1.0` clears the cutoff → covered. Alpha 0.0 → below it → not covered.
  await setBlockerAlpha(1.0);
  const covered = decodeRgb8Png(await captureViewport(engine, cleaner, "anyhit-covered"));
  await setBlockerAlpha(0.0);
  const cutOut = decodeRgb8Png(await captureViewport(engine, cleaner, "anyhit-cutout"));
  // The guard: the same covered blocker with ray-query shadows off. It says what the patch would
  // read if no ray shadow reached it at all, which is what a rejected candidate should reproduce.
  await setBlockerAlpha(1.0);
  await engine.call("set-rt-shadows", { enabled: false });
  await engine.settle(400);
  const noRayShadow = decodeRgb8Png(await captureViewport(engine, cleaner, "anyhit-guard"));
  await engine.call("set-rt-shadows", { enabled: true });
  await engine.settle(400);

  const patch = shadowPatch(covered);
  const shadowed = regionMean(covered, patch);
  const lit = regionMean(cutOut, patch);
  const unshadowed = regionMean(noRayShadow, patch);
  // A committed candidate darkens the receiver; a rejected one lets the ray through and leaves it
  // lit. The margin is far above the frame-to-frame noise the whole-frame metric sits in.
  expect(lit).toBeGreaterThan(shadowed + 5);
  // The patch is a ray-shadow receiver, not some other surface that happens to change: with the
  // ray shadow disabled it brightens by the same amount rejecting the candidate does.
  expect(unshadowed).toBeGreaterThan(shadowed + 5);
  expect(Math.abs(lit - unshadowed)).toBeLessThan(2);
  expect(engine.validationErrors()).toEqual([]);
});
