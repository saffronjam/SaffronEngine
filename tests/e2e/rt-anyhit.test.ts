// The KHR any-hit path running over canonical coverage on real ray-tracing hardware.
//
// The blocker carries a thin-sheet-foliage surface with an `albedo-alpha` coverage source and a
// `masked` classification, so its instance is packed FORCE_NO_OPAQUE and its triangles surface as
// non-opaque ray candidates rather than auto-committing. `gpuSceneRayCandidateCovered` then
// reconstructs the hit and runs `classifyCanonicalCoverage`; for an albedo-alpha source the verdict
// is `sampled * baseColorAlpha` against the reference cutoff, and with no albedo texture bound
// `sampled` is the default-white 1.0, so the base-colour alpha alone decides.
//
// WHAT THIS ESTABLISHES: the candidate-confirmation path executes over a masked thin-sheet blocker
// with ray-query shadows armed, a TLAS built, and zero validation messages — the chain that had
// never run on any device the project could reach.
//
// The assertion samples the RECEIVER patch the ray shadow falls on, not the whole frame. That
// distinction is the whole test: the blocker's own shading also responds to its coverage, so a
// frame-wide comparison cannot separate "the candidate was rejected" from "the blocker looks
// different". The patch below was located by diffing a normal frame against a build that rejects
// every candidate — the pixels that changed are exactly the candidate-driven ray shadow — and it
// lies just below the blocker, well clear of the blocker's own pixels.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { EntityRef, RenderStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene, trackEntity } from "./test-utils.ts";
import { decodeRgb8Png, regionMean } from "./image.ts";

const cleaner = new Cleaner();
let engine: Engine;
let rtSupported = false;
let blocker = "";
let material = "";

/// A thin-sheet foliage surface whose coverage comes from albedo alpha and is `masked`, so the
/// classifier — not the geometry — decides whether a ray candidate commits.
const MASKED_THIN_SHEET = {
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
    baseColor: [0.5, 0.5, 0.5, alpha],
  });
  await engine.settle(400);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { position: { x: 0, y: 5, z: 9 }, yaw: 0, pitch: -25 } });
  rtSupported = (await engine.call<RenderStatsDto>("render-stats")).rtSupported;
  if (!rtSupported) {
    return;
  }
  // A receiver to catch the shadow, and a blocker between it and the sun.
  trackEntity(cleaner, engine, await engine.call<EntityRef>("add-entity", { preset: "plane" }));
  const cube = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("add-entity", { preset: "cube" }),
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
  const created = await engine.call<{ id: string }>("material-create", { name: "AnyHitLeaf" });
  material = created.id;
  await engine.call("material-assign", { entity: blocker, material });
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
  const stats = await engine.call<RenderStatsDto>("render-stats");
  expect(stats.rtShadows).toBe(true);
  expect(stats.rtInstances).toBeGreaterThan(0);
  expect(stats.blasCount).toBeGreaterThan(0);
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

  // The receiver patch the candidate-driven ray shadow lands on, in frame fractions so it survives
  // a viewport size change.
  const shadowPatch = {
    x: Math.floor(covered.width * 0.465),
    y: Math.floor(covered.height * 0.557),
    width: Math.max(1, Math.floor(covered.width * 0.037)),
    height: Math.max(1, Math.floor(covered.height * 0.053)),
  };
  const shadowed = regionMean(covered, shadowPatch);
  const lit = regionMean(cutOut, shadowPatch);
  // A committed candidate darkens the receiver; a rejected one lets the ray through and leaves it
  // lit. The margin is far above the frame-to-frame noise the whole-frame metric sits in.
  expect(lit).toBeGreaterThan(shadowed + 5);
  expect(engine.validationErrors()).toEqual([]);
});
