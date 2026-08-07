// The ragdoll + character-controller surface end to end on a single booted host: a passive ragdoll
// (bodies drive the bones at full physics weight, a leaf collapses onto the floor, disabling restores
// the pose), an active ragdoll (motors mix physics against the animation through the per-bone PoseBuffer
// blend layer, and ramping the weight back recovers), the per-selection panel commands
// (enable-ragdoll / set-ragdoll / get-ragdoll, all play-gated), and a capsule CharacterVirtual driven
// by move-character. The BonePhysicsComponent is auto-fit on import.
//
// One boot: the passive + active cases share one imported rig (built once in beforeAll, torn down
// after the active section); the panel + character cases author their own throwaway entities and
// clean them up per case.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import type { Vec3 } from "@saffron/protocol";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, Cleaner, trackEntity } from "./test-utils.ts";

let engine: Engine;
let meshId = ""; // the imported model root shared by the passive + active sections
let rigId = ""; // the SkinnedMesh/BonePhysics carrier under the model's container root
let sharedFloor = "";
let ankle = "";
let restAnkleY = 0;
let beforeY = 0; // the animated ankle height at play start, before physics takes over
const suiteCleaner = new Cleaner();
const sharedCleaner = new Cleaner();
const caseCleaner = new Cleaner();

const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");

const worldY = async (entity: string): Promise<number> =>
  (await engine.call("get-world-transform", { entity })).translation.y;

const world = async (entity: string): Promise<Vec3> =>
  (await engine.call("get-world-transform", { entity })).translation;

async function spawn(name: string): Promise<string> {
  const id = (await engine.call("create-entity", { name })).id;
  return trackEntity(caseCleaner, engine, id);
}

async function cleanup(): Promise<void> {
  await engine.call("stop").catch(() => {});
  await caseCleaner.cleanup();
}

beforeAll(async () => {
  engine = await bootEngine(suiteCleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  suiteCleaner.defer(() => sharedCleaner.cleanup());
  suiteCleaner.defer(() => cleanup());
  meshId = trackEntity(sharedCleaner, engine, (await engine.importEntity(LEG)).id);
  rigId = await engine.rig(meshId); // the rig descendant carries SkinnedMesh + BonePhysics
  const info = await engine.call("inspect", { entity: rigId });
  ankle = info.components.SkinnedMesh!.bones[2]; // hip / knee / ankle — the leaf
  restAnkleY = await worldY(ankle); // authored rest pose (Edit)

  // A floor below the leg so the collapsed ragdoll settles rather than falling forever.
  sharedFloor = trackEntity(
    sharedCleaner,
    engine,
    (await engine.call("create-entity", { name: "Floor" })).id,
  );
  await engine.call("set-transform", {
    entity: sharedFloor,
    translation: { x: 0, y: restAnkleY - 1.5, z: 0 },
  });
  await engine.call("add-component", { entity: sharedFloor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: sharedFloor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });
});
afterAll(async () => {
  await suiteCleaner.cleanup();
});

test("import auto-fits a BonePhysicsComponent (one entry per bone)", async () => {
  const info = await engine.call("inspect", { entity: rigId });
  expect(info.components.BonePhysics?.bones.length).toBe(3);
});

test("enable-ragdoll collapses the leaf bone onto the floor; disable restores the pose", async () => {
  await engine.call("play");
  await engine.settle(200);
  const beforePlayY = await worldY(ankle); // ~rest pose at play start

  const enabled = await engine.call("enable-ragdoll", {
    entity: meshId,
    enabled: true,
  });
  expect(enabled.present).toBe(true);
  expect(enabled.bones).toBe(3);

  await engine.settle(2500); // collapse + settle
  const limpY = await worldY(ankle);
  await engine.settle(400);
  const settledY = await worldY(ankle);

  expect(limpY).toBeLessThan(beforePlayY - 0.2); // the bone fell under gravity
  expect(limpY).toBeGreaterThan(restAnkleY - 2.0); // settled near the floor, did not tunnel away
  expect(Math.abs(settledY - limpY)).toBeLessThan(0.15); // came to rest

  // Disable -> the ragdoll is removed; the bone reverts toward the animation/rest pose.
  const disabled = await engine.call("enable-ragdoll", {
    entity: meshId,
    enabled: false,
  });
  expect(disabled.present).toBe(false);
  await engine.settle(400);
  expect(await worldY(ankle)).toBeGreaterThan(limpY + 0.1); // back up toward rest

  await engine.call("stop");
  await engine.settle();
});

test("set-ragdoll auto-creates the ragdoll and reports its blend state", async () => {
  await engine.call("play");
  await engine.settle(200);
  beforeY = await worldY(ankle); // animated pose at play start, before physics

  // No enable-ragdoll round-trip: the first set-ragdoll builds the ragdoll, passive (motors off).
  const createdRagdoll = await engine.call("set-ragdoll", {
    entity: meshId,
    bodyWeight: 1,
  });
  expect(createdRagdoll.present).toBe(true);
  expect(createdRagdoll.active).toBe(false);
  expect(createdRagdoll.bones).toBe(3);
  expect(createdRagdoll.bodyWeight).toBeCloseTo(1, 1);

  const got = await engine.call("get-ragdoll", { entity: meshId });
  expect(got.present).toBe(true);
});

test("a hit blends the limb to physics; ramping the weight back to 0 recovers the animation", async () => {
  // Full physics weight (passive): the leaf bone falls under gravity, diverging from the animation.
  // SwingTwist motors restore relative joint pose, not the unconstrained root's world height — so a
  // free ragdoll's recover to the *animated pose* is the weight blend, not the motors (a kinematic
  // root anchor, the standing recover, is deferred). The motor path runs under the active flag below.
  await engine.settle(2500);
  const limpY = await worldY(ankle);
  expect(limpY).toBeLessThan(beforeY - 0.2); // physics took over and fell

  // Motors on: the drive runs every fixed step toward the animation target (covered validation-clean).
  await engine.call("set-ragdoll", { entity: meshId, active: true });
  expect((await engine.call("get-ragdoll", { entity: meshId })).active).toBe(true);
  await engine.settle(1000);

  // Ramp the physics weight back to 0: the bone follows the animation again (the recover).
  await engine.call("set-ragdoll", { entity: meshId, active: false, bodyWeight: 0 });
  await engine.settle(600);
  const recoverY = await worldY(ankle);
  expect(recoverY).toBeGreaterThan(limpY + 0.1); // back up at the animated pose
  expect(Math.abs(recoverY - beforeY)).toBeLessThan(0.3);

  await engine.call("stop");
  await engine.settle();

  // The passive + active sections are done with the shared rig; tear it down so the panel +
  // character cases below start from a clean scene.
  await sharedCleaner.cleanup();
}, 20000);

test("enable-ragdoll builds a ragdoll and set/get-ragdoll drive its blend", async () => {
  const leg = trackEntity(caseCleaner, engine, await engine.importEntity(LEG));
  await engine.call("play");
  await engine.settle(200);

  // enable-ragdoll resolves the model root to its rig descendant (SkinnedMesh + BonePhysics).
  const enabled = await engine.call("enable-ragdoll", { entity: leg.id });
  expect(enabled.present).toBe(true);
  expect(enabled.bones).toBeGreaterThan(0);

  // Drive the uniform physics blend to 1 (pure physics) and turn motors on.
  await engine.call("set-ragdoll", { entity: leg.id, active: true, bodyWeight: 1 });
  const state = await engine.call("get-ragdoll", { entity: leg.id });
  expect(state.present).toBe(true);
  expect(state.active).toBe(true);
  expect(state.bodyWeight).toBeGreaterThan(0.5);

  await cleanup();
});

test("the ragdoll commands error before play (the panel play-gates them)", async () => {
  const leg = trackEntity(caseCleaner, engine, await engine.importEntity(LEG));
  // No play: ctx.physics is null, so enable-ragdoll rejects.
  await expect(engine.call("enable-ragdoll", { entity: leg.id })).rejects.toThrow();
  await cleanup();
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
  const moved = await engine.call("move-character", {
    entity: char,
    velocity: { x: 3, y: 0, z: 0 },
  });
  expect(typeof moved.onGround).toBe("boolean");
  expect(typeof moved.position.x).toBe("number");

  await cleanup();
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
  await engine.call("set-component-field", {
    entity: char,
    component: "Collider",
    field: "shape",
    value: "capsule",
  });
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
  const moved = await engine.call("move-character", {
    entity: char,
    velocity: { x: 2, y: 0, z: 0 },
  });
  expect(moved.onGround).toBe(true);
  await engine.settle(1500);

  const after = await world(char);
  expect(after.x).toBeGreaterThan(settled.x + 1.0); // it walked forward
  expect(Math.abs(after.y - settled.y)).toBeLessThan(0.25); // stayed on the floor (didn't sink or fly)

  await cleanup();
});

test("the ragdoll/character run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
