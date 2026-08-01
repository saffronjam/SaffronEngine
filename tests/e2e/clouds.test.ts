// Volumetric-cloud end-to-end: exercise the one scene-state block, generated wire DTO, GPU shape
// resources, lit march, temporal reconstruction, and lasting density debugger.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  SetCloudsParams,
  SetWindParams,
} from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner, prepareScene } from "./test-utils.ts";

let engine: Engine;
const cleaner = new Cleaner();

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `clouds-${tag}`);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    width: 320,
    height: 180,
    camera: { position: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 30, fov: 60 },
  });
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("clouds are disabled by default", async () => {
  const env = await engine.call("get-environment");
  expect(env.cloud.enabled).toBe(false);
  expect(env.cloud.weatherTexture).toBe("0");
});

test("set-clouds merges and round-trips the complete shape block", async () => {
  const params: SetCloudsParams = {
    enabled: true,
    coverage: 0.7,
    cloudType: 0.4,
    precipitation: 0.2,
    anvilBias: 0.25,
    layerAltitude: 50,
    layerHeight: 400,
    baseScale: 0.002,
    detailScale: 0.02,
    detailStrength: 0.35,
    curlStrength: 20,
    weatherScale: 0.002,
    weatherOffset: { x: 0.1, y: 0, z: 0.2 },
    primarySteps: 72,
    lightSteps: 8,
    dropletDiameter: 24,
    temporalFactor: 0.15,
    castCloudShadows: true,
    cloudShadowStrength: 0.8,
    cloudShadowOnSurfaceStrength: 0.65,
  };
  const applied = await engine.call("set-clouds", params);
  expect(applied.cloud.enabled).toBe(true);
  expect(applied.cloud.coverage).toBeCloseTo(0.7, 6);
  expect(applied.cloud.cloudType).toBeCloseTo(0.4, 6);
  expect(applied.cloud.precipitation).toBeCloseTo(0.2, 6);
  expect(applied.cloud.anvilBias).toBeCloseTo(0.25, 6);
  expect(applied.cloud.layerAltitude).toBeCloseTo(50, 6);
  expect(applied.cloud.layerHeight).toBeCloseTo(400, 6);
  expect(applied.cloud.weatherOffset.x).toBeCloseTo(0.1, 6);
  expect(applied.cloud.weatherOffset.y).toBeCloseTo(0, 6);
  expect(applied.cloud.weatherOffset.z).toBeCloseTo(0.2, 6);
  expect(applied.cloud.primarySteps).toBe(72);
  expect(applied.cloud.lightSteps).toBe(8);
  expect(applied.cloud.dropletDiameter).toBeCloseTo(24, 6);
  expect(applied.cloud.temporalFactor).toBeCloseTo(0.15, 6);
  expect(applied.cloud.castCloudShadows).toBe(true);
  expect(applied.cloud.cloudShadowStrength).toBeCloseTo(0.8, 6);
  expect(applied.cloud.cloudShadowOnSurfaceStrength).toBeCloseTo(0.65, 6);

  const readBack = await engine.call("get-environment");
  expect(readBack.cloud).toEqual(applied.cloud);
});

test("shared wind and cloud shadows round-trip and affect the lit frame", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.call("set-wind", { orientation: 0, speed: 0, gust: 0 } satisfies SetWindParams);
  await engine.call("set-clouds", {
    enabled: true,
    coverage: 0.8,
    castCloudShadows: false,
    cloudShadowStrength: 1,
    cloudShadowOnSurfaceStrength: 1,
  });
  await engine.settle(900);
  const unshadowed = await screenshot("integration-unshadowed");

  const shadowedEnv = await engine.call("set-clouds", {
    castCloudShadows: true,
  });
  expect(shadowedEnv.cloud.castCloudShadows).toBe(true);
  expect(shadowedEnv.cloud.cloudShadowStrength).toBeCloseTo(1, 6);
  expect(shadowedEnv.cloud.cloudShadowOnSurfaceStrength).toBeCloseTo(1, 6);
  await engine.settle(900);
  const shadowed = await screenshot("integration-shadowed");
  expect(shadowed.equals(unshadowed)).toBe(false);

  const wind: SetWindParams = { orientation: 45, speed: 20, gust: 0.5 };
  const windyEnv = await engine.call("set-wind", wind);
  expect(windyEnv.wind.orientation).toBeCloseTo(45, 6);
  expect(windyEnv.wind.speed).toBeCloseTo(20, 6);
  expect(windyEnv.wind.gust).toBeCloseTo(0.5, 6);
  await engine.settle(900);
  const windy = await screenshot("integration-windy");
  expect(windy.equals(shadowed)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});

test("lit clouds composite before bloom and respond to physical lighting controls", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.call("set-clouds", {
    enabled: true,
    coverage: 0.8,
    primarySteps: 72,
    lightSteps: 1,
    dropletDiameter: 5,
    temporalFactor: 0.2,
  });
  await engine.settle(900);
  const smallDroplets = await screenshot("lit-small-droplets");

  await engine.call("set-clouds", { lightSteps: 12, dropletDiameter: 50 });
  await engine.settle(900);
  const largeDroplets = await screenshot("lit-large-droplets");
  expect(largeDroplets.equals(smallDroplets)).toBe(false);

  await engine.call("set-clouds", { enabled: false });
  await engine.settle(300);
  const disabled = await screenshot("lit-disabled");
  expect(disabled.equals(largeDroplets)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);

  await engine.call("set-clouds", { enabled: true });
});

test("cloud-density view mode reaches render stats", async () => {
  const result = await engine.call("set-view-mode", {
    mode: "cloud-density",
  });
  expect(result.viewMode).toBe("cloud-density");
  const stats = await engine.call("render-stats");
  expect(stats.viewMode).toBe("cloud-density");
});

test("coverage, type, anvil, and precipitation reshape the GPU density field", async () => {
  await engine.call("set-clouds", {
    coverage: 0.15,
    cloudType: 0.1,
    precipitation: 0,
    anvilBias: 0,
  });
  await engine.settle(600);
  const sparse = await screenshot("sparse");

  await engine.call("set-clouds", { coverage: 0.9 });
  await engine.settle(600);
  const covered = await screenshot("covered");
  expect(covered.equals(sparse)).toBe(false);

  await engine.call("set-clouds", { cloudType: 0.95, anvilBias: 1 });
  await engine.settle(600);
  const anvil = await screenshot("anvil");
  expect(anvil.equals(covered)).toBe(false);

  await engine.call("set-clouds", { precipitation: 1 });
  await engine.settle(600);
  const precipitation = await screenshot("precipitation");
  expect(precipitation.equals(anvil)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});

test("invalid cloud ranges are rejected", async () => {
  await expect(engine.call("set-clouds", { coverage: 1.1 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { layerHeight: 0 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { detailScale: -1 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { primarySteps: 0 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { lightSteps: 0 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { dropletDiameter: 51 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { temporalFactor: 1.1 })).rejects.toThrow();
  await expect(engine.call("set-clouds", { cloudShadowStrength: 1.1 })).rejects.toThrow();
  await expect(engine.call("set-wind", { speed: -1 })).rejects.toThrow();
  await expect(engine.call("set-wind", { gust: -1 })).rejects.toThrow();
});
