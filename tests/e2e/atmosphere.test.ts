// set-atmosphere over the control plane: every field it is given is reflected in the returned
// EnvironmentDto's atmosphere block, a partial call merges over the read-back (it does not reset),
// and the free-form {json} path merges arbitrary keys. Asserts a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

interface EnvDto {
  atmosphere: {
    enabled: boolean;
    planetRadius: number;
    atmosphereHeight: number;
    rayleighScattering: { x: number; y: number; z: number };
    mieScattering: number;
    mieAnisotropy: number;
    sunDiskIntensity: number;
  };
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("set-atmosphere reflects every field it is given", async () => {
  const env = await engine.call<EnvDto>("set-atmosphere", {
    enabled: true,
    planetRadius: 6360000,
    rayleighScattering: { x: 5.8, y: 13.5, z: 33.1 },
    sunDiskIntensity: 20,
  });
  expect(env.atmosphere.enabled).toBe(true);
  expect(env.atmosphere.planetRadius).toBeCloseTo(6360000, 0);
  expect(env.atmosphere.rayleighScattering.x).toBeCloseTo(5.8, 3);
  expect(env.atmosphere.rayleighScattering.y).toBeCloseTo(13.5, 3);
  expect(env.atmosphere.rayleighScattering.z).toBeCloseTo(33.1, 3);
  expect(env.atmosphere.sunDiskIntensity).toBeCloseTo(20, 3);
});

test("a partial set-atmosphere merges over the read-back, not resets", async () => {
  const env = await engine.call<EnvDto>("set-atmosphere", { mieAnisotropy: 0.8 });
  expect(env.atmosphere.mieAnisotropy).toBeCloseTo(0.8, 3);
  // The fields set by the previous call survive — proof it merges over the current state.
  expect(env.atmosphere.enabled).toBe(true);
  expect(env.atmosphere.planetRadius).toBeCloseTo(6360000, 0);
  expect(env.atmosphere.sunDiskIntensity).toBeCloseTo(20, 3);
});

test("the free-form {json} path merges arbitrary keys", async () => {
  const env = await engine.call<EnvDto>("set-atmosphere", { json: { mieScattering: 4.2 } });
  expect(env.atmosphere.mieScattering).toBeCloseTo(4.2, 3);
  expect(env.atmosphere.enabled).toBe(true);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
