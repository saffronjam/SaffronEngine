// Shared wind control-plane end-to-end: merge the extended deterministic global
// settings over the wire, reject out-of-range values, and round-trip a placeable
// WindSource component through the registry.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import { prepareScene } from "./test-utils.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("set-wind merges the extended deterministic parameters and validates ranges", async () => {
  const env = await engine.call("set-wind", {
    speed: 14,
    json: {
      turbulenceOctaves: 4,
      turbulenceRoughness: 0.4,
      gustFrequency: 0.3,
      referenceHeight: 12,
      heightExponent: 0.25,
      seed: 9,
    },
  });
  expect(env.wind.speed).toBe(14);
  expect(env.wind.turbulenceOctaves).toBe(4);
  expect(env.wind.turbulenceRoughness).toBeCloseTo(0.4);
  expect(env.wind.gustFrequency).toBeCloseTo(0.3);
  expect(env.wind.referenceHeight).toBeCloseTo(12);
  expect(env.wind.heightExponent).toBeCloseTo(0.25);
  expect(env.wind.seed).toBe(9);

  const read = await engine.call("get-environment");
  expect(read.wind.turbulenceOctaves).toBe(4);

  await expect(
    engine.call("set-wind", { json: { turbulenceOctaves: 99 } }),
  ).rejects.toThrow(/octaves/i);
  await expect(
    engine.call("set-wind", { json: { turbulenceRoughness: 1.5 } }),
  ).rejects.toThrow(/roughness/i);
});

test("a WindSource component round-trips through the registry", async () => {
  const entity = await engine.call("create-entity", { name: "Breeze" });
  await engine.call("add-component", { entity: entity.id, component: "WindSource" });
  await engine.call("set-component", {
    entity: entity.id,
    component: "WindSource",
    json: { kind: "vortex", strength: 7.5, radius: 33, falloff: 0.25, enabled: true },
  });
  const info = await engine.call("inspect", { entity: entity.id });
  const source = info.components.WindSource!;
  expect(source.kind).toBe("vortex");
  expect(source.strength).toBeCloseTo(7.5);
  expect(source.radius).toBeCloseTo(33);
  expect(source.falloff).toBeCloseTo(0.25);
  expect(source.enabled).toBe(true);

  await engine.settle(300);
  expect(engine.validationErrors()).toEqual([]);
});

test("sample-wind composes the global field with placed sources deterministically", async () => {
  // A calm global profile isolates the local source's contribution.
  await engine.call("set-wind", { speed: 0 });
  const calm = await engine.call("sample-wind", {
    positionM: [210, 1, 200],
    timeS: 3,
  });
  expect(calm.velocityMps).toEqual([0, 0, 0]);

  const source = await engine.call("create-entity", { name: "Gust" });
  await engine.call("add-component", { entity: source.id, component: "WindSource" });
  await engine.call("set-component", {
    entity: source.id,
    component: "WindSource",
    json: { kind: "point", strength: 6, radius: 40, falloff: 0, enabled: true },
  });
  await engine.call("set-transform", { entity: source.id, translation: { x: 200, y: 1, z: 200 } });
  await engine.settle(100);

  // Ten metres east of a point source: the radial contribution points +X.
  const sampled = await engine.call("sample-wind", {
    positionM: [210, 1, 200],
    timeS: 3,
  });
  expect(sampled.velocityMps[0]).toBeCloseTo(6, 1);
  expect(Math.abs(sampled.velocityMps[2])).toBeLessThan(1e-3);
  const again = await engine.call("sample-wind", {
    positionM: [210, 1, 200],
    timeS: 3,
  });
  expect(again.velocityMps).toEqual(sampled.velocityMps);

  // The engine clock is monotonic: two clockless samples never step backwards.
  const first = await engine.call("sample-wind", { positionM: [0, 1, 0] });
  await engine.settle(100);
  const second = await engine.call("sample-wind", { positionM: [0, 1, 0] });
  expect(second.timeS).toBeGreaterThanOrEqual(first.timeS);

  await engine.call("destroy-entity", { entity: source.id });
  await engine.call("set-wind", { speed: 10 });
});

test("emit-interaction-impulse stages a field push and validates ranges", async () => {
  const accepted = await engine.call("emit-interaction-impulse", {
    positionM: [0, 0],
    radiusM: 2,
    strength: 3,
    direction: [1, 0],
    depress: 0.5,
  });
  expect(accepted.accepted).toBe(true);

  // A directionless impulse pushes radially and is equally accepted.
  const radial = await engine.call("emit-interaction-impulse", {
    positionM: [4, -2],
    radiusM: 1,
    strength: 1,
  });
  expect(radial.accepted).toBe(true);

  // The field step consumes the staged impulses on the next frames.
  await engine.settle(150);

  await expect(
    engine.call("emit-interaction-impulse", { positionM: [0, 0], radiusM: 100, strength: 1 }),
  ).rejects.toThrow(/radius/i);
  await expect(
    engine.call("emit-interaction-impulse", { positionM: [0, 0], radiusM: 1, strength: 99 }),
  ).rejects.toThrow(/strength/i);
});

test("the wind-vector overlay flag round-trips", async () => {
  const set = await engine.call("set-debug-overlays", {
    windVectors: true,
  });
  expect(set.windVectors).toBe(true);
  await engine.settle(100);
  const read = await engine.call("get-debug-overlays");
  expect(read.windVectors).toBe(true);
  await engine.call("set-debug-overlays", { windVectors: false });
});
