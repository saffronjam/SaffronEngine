// The physics lifecycle end to end on a single booted host: the per-play-session Jolt world
// (built on the Edit->Playing edge, discarded on ->Edit), the component -> body -> step ->
// write-back loop (a dynamic box falls under gravity and settles on a static floor), and the
// telemetry contract the Physics panel depends on (physics-state / drain-contacts / physics-bodies
// are Edit-safe, then report the live world while playing; apply-impulse pushes a dynamic body).
//
// One boot: the empty-world lifecycle runs first (no bodies), then the falling-box scene is
// authored and reused for the live-telemetry cases so the box's landing feeds a real contact.

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
type HitTarget = { kind: "scene-entity"; id: string } | { kind: "vegetation"; plant: string };

interface ContactDrain {
  events: { kind: string; targetA?: HitTarget; targetB?: HitTarget; sensor: boolean }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}
interface PhysicsBodies {
  bodies: { entity: string; motion: string; active: boolean; position: { y: number } }[];
}

const boxY = async (): Promise<number> =>
  (await engine.call<WorldTransform>("get-world-transform", { entity: box })).translation.y;

// floor top = floor center (0) + floor half-height (0.1); box half-extent = 0.5 (default).
const FLOOR_TOP = 0.1;
const BOX_HALF = 0.5;
const REST_Y = FLOOR_TOP + BOX_HALF; // ~0.6

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("no physics world exists in Edit", async () => {
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(false);
  expect(state.bodyCount).toBe(0);
  expect(state.dynamicCount).toBe(0);
});

test("physics-state, drain-contacts, and physics-bodies are Edit-safe (inactive / empty)", async () => {
  const drain = await engine.call<ContactDrain>("drain-contacts", { since: 0 });
  expect(drain.events).toEqual([]);
  expect(drain.overflowed).toBe(false);
  const bodies = await engine.call<PhysicsBodies>("physics-bodies");
  expect(bodies.bodies).toEqual([]);
});

test("entering play allocates a Jolt world; stopping frees it", async () => {
  await engine.call("play");
  await engine.settle();
  const playing = await engine.call<PhysicsState>("physics-state");
  expect(playing.active).toBe(true);
  expect(playing.bodyCount).toBe(0); // no components author bodies yet
  expect(playing.dynamicCount).toBe(0);

  await engine.call("stop");
  await engine.settle();
  const stopped = await engine.call<PhysicsState>("physics-state");
  expect(stopped.active).toBe(false);
});

test("a second play/stop cycle re-allocates a fresh world (no leak across the edge)", async () => {
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(true);
  await engine.call("stop");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(false);
});

test("no world in Edit; the box sits at its authored height", async () => {
  // Author the falling-box scene: a static floor (a collider with no rigidbody is implicitly
  // static) + a dynamic box dropped from y=5. Reused by the live-telemetry cases below.
  floor = (await engine.call<{ id: string }>("create-entity", { name: "Floor" })).id;
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });

  box = (await engine.call<{ id: string }>("create-entity", { name: "Box" })).id;
  await engine.call("set-transform", { entity: box, translation: { x: 0, y: 5, z: 0 } });
  await engine.call("add-component", { entity: box, component: "Collider" });
  await engine.call("add-component", { entity: box, component: "Rigidbody" });

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

test("the physics lifecycle run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
