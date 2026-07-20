import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import { prepareScene } from "./test-utils.ts";
import type {
  EntityRef,
  EnvironmentDto,
  InspectResult,
  SetAtmosphereParams,
} from "@saffron/protocol";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine);
});

afterAll(async () => {
  await engine?.shutdown();
});

test("atmosphere and moon settings round-trip over the control plane", async () => {
  const params: SetAtmosphereParams = {
    enabled: true,
    sunDiskIntensity: 1.25,
    moonDiskAngularRadius: 0.005,
    moonDiskIntensity: 1.5,
    moonEarthshine: 0.04,
    perPixelTransmittance: true,
    skyCaptureCadence: 7,
  };
  const applied = await engine.call<EnvironmentDto>("set-atmosphere", params);
  expect(applied.atmosphere.enabled).toBe(true);
  expect(applied.atmosphere.sunDiskIntensity).toBeCloseTo(1.25, 5);
  expect(applied.atmosphere.moonDiskAngularRadius).toBeCloseTo(0.005, 5);
  expect(applied.atmosphere.moonDiskIntensity).toBeCloseTo(1.5, 5);
  expect(applied.atmosphere.moonEarthshine).toBeCloseTo(0.04, 5);
  expect(applied.atmosphere.perPixelTransmittance).toBe(true);
  expect(applied.atmosphere.skyCaptureCadence).toBeCloseTo(7, 5);

  const environment = await engine.call<EnvironmentDto>("get-environment");
  expect(environment.atmosphere).toEqual(applied.atmosphere);
});

test("directional lights select the atmosphere sun or moon role", async () => {
  const moon = await engine.call<EntityRef>("add-entity", { preset: "directional-light" });
  await engine.call("set-component-field", {
    entity: moon.id,
    component: "DirectionalLight",
    field: "atmosphereRole",
    value: "moon",
  });
  const inspected = await engine.call<InspectResult>("inspect", { entity: moon.id });
  expect(inspected.components.DirectionalLight?.atmosphereRole).toBe("moon");
});

test("celestial direction refreshes complete without Vulkan validation errors", async () => {
  await engine.call("set-light", { direction: { x: -0.25, y: -0.9, z: 0.35 } });
  await engine.settle(1200);
  expect(engine.validationErrors()).toEqual([]);
});
