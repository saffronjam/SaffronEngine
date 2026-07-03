// Phase 2 of the physics plan: the component -> body -> step -> write-back loop. A dynamic box is
// dropped above a static floor; physics steps inside the simTick seam and writes the body's world
// transform back into the entity TransformComponent every frame. The box falls under gravity and
// comes to rest on the floor. A collider with no Rigidbody is an implicit static body (the floor).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
let floor = "";
let box = "";

interface WorldTransform {
  translation: { x: number; y: number; z: number };
  scale: { x: number; y: number; z: number };
}
interface PhysicsState {
  active: boolean;
  bodyCount: number;
  dynamicCount: number;
}

const boxY = async (): Promise<number> =>
  (await engine.call<WorldTransform>("get-world-transform", { entity: box })).translation.y;

// floor top = floor center (0) + floor half-height (0.1); box half-extent = 0.5 (default).
const FLOOR_TOP = 0.1;
const BOX_HALF = 0.5;
const REST_Y = FLOOR_TOP + BOX_HALF; // ~0.6

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });

  // Static floor: a thin wide collider with no rigidbody (implicitly static).
  floor = (await engine.call<{ id: string }>("create-entity", { name: "Floor" })).id;
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });

  // Dynamic box dropped from y=5 (default 0.5 half-extent box, default Dynamic rigidbody).
  box = (await engine.call<{ id: string }>("create-entity", { name: "Box" })).id;
  await engine.call("set-transform", { entity: box, translation: { x: 0, y: 5, z: 0 } });
  await engine.call("add-component", { entity: box, component: "Collider" });
  await engine.call("add-component", { entity: box, component: "Rigidbody" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("no world in Edit; the box sits at its authored height", async () => {
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(false);
  expect(await boxY()).toBeCloseTo(5, 3);
});

test("the box falls under gravity and settles on the floor", async () => {
  await engine.call("play");
  await engine.settle(300);
  const falling = await boxY();
  expect(falling).toBeLessThan(5); // it has started to fall

  // Let it settle, then sample twice to confirm it has come to rest (not still moving).
  await engine.settle(2000);
  const settled = await boxY();
  await engine.settle(400);
  const settledLater = await boxY();

  expect(settled).toBeGreaterThan(REST_Y - 0.2); // did not tunnel through the floor
  expect(settled).toBeLessThan(REST_Y + 0.3); // came to rest at ~the floor top + half-extent
  expect(Math.abs(settledLater - settled)).toBeLessThan(0.05); // at rest, not drifting
});

test("physics-state reports the two bodies, one dynamic", async () => {
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(true);
  expect(state.bodyCount).toBe(2);
  expect(state.dynamicCount).toBe(1);
});

test("stopping discards the world; the authored box height is untouched", async () => {
  await engine.call("stop");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(false);
  // The authored scene was never written during play — the box is back at y=5.
  expect(await boxY()).toBeCloseTo(5, 3);
});

test("the falling-box run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
