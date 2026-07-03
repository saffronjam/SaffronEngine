// Phase 6 of the physics plan: a walking capsule character via Jolt CharacterVirtual. The capsule is
// the entity's ColliderComponent; a CharacterControllerComponent carries the movement params, and
// move-character feeds it a desired horizontal velocity. The character settles onto the floor, walks
// across it (binding mode a — the controller positions the root), and reports its ground state.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
let created: string[] = [];

interface Vec3 {
  x: number;
  y: number;
  z: number;
}
interface MoveResult {
  position: Vec3;
  onGround: boolean;
}

const world = async (entity: string): Promise<Vec3> =>
  (await engine.call<{ translation: Vec3 }>("get-world-transform", { entity })).translation;

async function spawn(name: string): Promise<string> {
  const id = (await engine.call<{ id: string }>("create-entity", { name })).id;
  created.push(id);
  return id;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterEach(async () => {
  await engine.call("stop").catch(() => {});
  for (const id of created) {
    await engine.call("destroy-entity", { entity: id }).catch(() => {});
  }
  created = [];
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a capsule character settles on the floor and walks across it", async () => {
  // Static floor.
  const floor = await spawn("Floor");
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 20, y: 0.1, z: 20 },
  });

  // The character: a capsule collider + a controller, dropped just above the floor.
  const char = await spawn("Walker");
  await engine.call("set-transform", { entity: char, translation: { x: 0, y: 1.2, z: 0 } });
  await engine.call("add-component", { entity: char, component: "Collider" });
  await engine.call("set-component-field", { entity: char, component: "Collider", field: "shape", value: "capsule" });
  await engine.call("set-component-field", {
    entity: char,
    component: "Collider",
    field: "halfExtents",
    value: { x: 0.3, y: 0.5, z: 0.3 }, // radius 0.3, cylinder half-height 0.5
  });
  await engine.call("add-component", { entity: char, component: "CharacterController" });

  await engine.call("play");
  await engine.settle(700); // settle onto the floor
  const settled = await world(char);
  expect(settled.y).toBeGreaterThan(0.1); // standing on the floor, not sunk through

  // Walk +X for ~1.5 s.
  const moved = await engine.call<MoveResult>("move-character", { entity: char, velocity: { x: 2, y: 0, z: 0 } });
  expect(moved.onGround).toBe(true);
  await engine.settle(1500);

  const after = await world(char);
  expect(after.x).toBeGreaterThan(settled.x + 1.0); // it walked forward
  expect(Math.abs(after.y - settled.y)).toBeLessThan(0.25); // stayed on the floor (didn't sink or fly)
});

test("the character run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
