// physics-ui plan, phase 4: the telemetry contract the Physics panel depends on. physics-state and
// drain-contacts must be Edit-SAFE (inactive / empty, never an error) so the panel can mount in Edit
// and show its empty state; while Playing they report live body counts and drain the contact feed.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
let box = "";

interface PhysicsState {
  active: boolean;
  bodyCount: number;
  dynamicCount: number;
}
interface ContactDrain {
  events: { kind: string; entityA: string; entityB: string; sensor: boolean }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}
interface PhysicsBodies {
  bodies: { entity: string; motion: string; active: boolean; position: { y: number } }[];
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // A dynamic box over a static floor — the box lands and generates a contact.
  const floor = (await engine.call<{ id: string }>("create-entity", { name: "Floor" })).id;
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });
  box = (await engine.call<{ id: string }>("create-entity", { name: "Box" })).id;
  await engine.call("set-transform", { entity: box, translation: { x: 0, y: 3, z: 0 } });
  await engine.call("add-component", { entity: box, component: "Collider" });
  await engine.call("add-component", { entity: box, component: "Rigidbody" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("physics-state and drain-contacts are Edit-safe (inactive / empty, never an error)", async () => {
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(false);
  expect(state.bodyCount).toBe(0);
  const drain = await engine.call<ContactDrain>("drain-contacts", { since: 0 });
  expect(drain.events).toEqual([]);
  expect(drain.overflowed).toBe(false);
  const bodies = await engine.call<PhysicsBodies>("physics-bodies");
  expect(bodies.bodies).toEqual([]);
});

test("while Playing, physics-state reports the live world and contacts drain", async () => {
  await engine.call("play");
  await engine.settle(2000); // let the box fall and land

  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(true);
  expect(state.bodyCount).toBe(2);
  expect(state.dynamicCount).toBe(1);

  // The box landing on the floor fires at least one contact begin event.
  const drain = await engine.call<ContactDrain>("drain-contacts", { since: 0 });
  expect(drain.events.some((e) => e.kind === "begin")).toBe(true);
  expect(drain.highWaterSeq).toBeGreaterThan(0);

  // physics-bodies lists every live body (the floor + the box) with motion + position.
  const bodies = await engine.call<PhysicsBodies>("physics-bodies");
  expect(bodies.bodies.length).toBe(2);
  expect(bodies.bodies.some((b) => b.motion === "dynamic")).toBe(true);
  expect(bodies.bodies.every((b) => typeof b.position.y === "number")).toBe(true);
});

test("apply-impulse pushes a Dynamic body and returns its new velocity", async () => {
  // The box is still dynamic + active in the world (we are mid-play from the prior test).
  const result = await engine.call<{ velocity: { x: number; y: number; z: number } }>("apply-impulse", {
    entity: box,
    impulse: { x: 0, y: 0, z: 5 },
  });
  expect(result.velocity.z).toBeGreaterThan(0); // the impulse imparted +Z velocity
});

test("stopping returns physics-state to inactive", async () => {
  await engine.call("stop");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(false);
});

test("the physics telemetry run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
