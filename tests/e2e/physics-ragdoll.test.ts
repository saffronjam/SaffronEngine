// physics-ui plan, phase 5: the per-selection ragdoll + character controls the Physics panel drives.
// The ragdoll commands (enable-ragdoll / set-ragdoll / get-ragdoll) build a Jolt Ragdoll from the
// rig's auto-fit BonePhysicsComponent and blend physics against animation; move-character feeds a
// CharacterController a desired velocity. All are play-only (the world exists only in play).

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let created: string[] = [];
const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");

interface RagdollResult {
  present: boolean;
  active: boolean;
  bodyWeight: number;
  bones: number;
}
interface MoveResult {
  position: { x: number; y: number; z: number };
  onGround: boolean;
}

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

test("enable-ragdoll builds a ragdoll and set/get-ragdoll drive its blend", async () => {
  const leg = await engine.importEntity(LEG);
  created.push(leg.id);
  await engine.call("play");
  await engine.settle(200);

  // enable-ragdoll resolves the model root to its rig descendant (SkinnedMesh + BonePhysics).
  const enabled = await engine.call<RagdollResult>("enable-ragdoll", { entity: leg.id });
  expect(enabled.present).toBe(true);
  expect(enabled.bones).toBeGreaterThan(0);

  // Drive the uniform physics blend to 1 (pure physics) and turn motors on.
  await engine.call<RagdollResult>("set-ragdoll", { entity: leg.id, active: true, bodyWeight: 1 });
  const state = await engine.call<RagdollResult>("get-ragdoll", { entity: leg.id });
  expect(state.present).toBe(true);
  expect(state.active).toBe(true);
  expect(state.bodyWeight).toBeGreaterThan(0.5);
});

test("the ragdoll commands error before play (the panel play-gates them)", async () => {
  const leg = await engine.importEntity(LEG);
  created.push(leg.id);
  // No play: ctx.physics is null, so enable-ragdoll rejects.
  await expect(engine.call("enable-ragdoll", { entity: leg.id })).rejects.toThrow();
});

test("move-character feeds a capsule character its desired velocity in play", async () => {
  const floor = await spawn("Floor");
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 20, y: 0.1, z: 20 },
  });

  const char = await spawn("Character");
  await engine.call("set-transform", { entity: char, translation: { x: 0, y: 1, z: 0 } });
  await engine.call("add-component", { entity: char, component: "Collider" });
  await engine.call("set-component-field", {
    entity: char,
    component: "Collider",
    field: "shape",
    value: "capsule",
  });
  await engine.call("add-component", { entity: char, component: "CharacterController" });

  await engine.call("play");
  await engine.settle(500);
  const moved = await engine.call<MoveResult>("move-character", {
    entity: char,
    velocity: { x: 3, y: 0, z: 0 },
  });
  expect(typeof moved.onGround).toBe("boolean");
  expect(typeof moved.position.x).toBe("number");
});

test("the ragdoll/character run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
