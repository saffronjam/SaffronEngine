// Phase 5 of the physics plan: the Kinematic motion type + per-bone kinematic bodies that follow the
// animated pose (binding mode b, animation -> physics). A Kinematic body is not pushed by the
// simulation (it ignores gravity); a rig that opts in via KinematicBonesComponent gets one kinematic
// body per joint, driven each step by MoveKinematic so a moving character shoves the world.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, Cleaner, trackEntity } from "./test-utils.ts";

let engine: Engine;
const caseCleaner = new Cleaner();
const suiteCleaner = new Cleaner();
const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");

interface PhysicsState {
  active: boolean;
  bodyCount: number;
  dynamicCount: number;
}
interface KinematicBonesResult {
  entity: string;
  enabled: boolean;
  boneCount: number;
}

const worldY = async (entity: string): Promise<number> =>
  (await engine.call<{ translation: { y: number } }>("get-world-transform", { entity })).translation
    .y;

async function spawn(name: string): Promise<string> {
  const id = (await engine.call<{ id: string }>("create-entity", { name })).id;
  return trackEntity(caseCleaner, engine, id);
}

beforeAll(async () => {
  engine = await bootEngine(suiteCleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});
afterEach(async () => {
  await engine.call("stop").catch(() => {});
  await caseCleaner.cleanup();
});
afterAll(async () => {
  await suiteCleaner.cleanup();
});

test("a Kinematic body ignores gravity while a Dynamic one falls", async () => {
  // Static floor.
  const floor = await spawn("Floor");
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });

  // A kinematic body and a dynamic body, both dropped from y = 5.
  const kin = await spawn("Kin");
  await engine.call("set-transform", { entity: kin, translation: { x: 2, y: 5, z: 0 } });
  await engine.call("add-component", { entity: kin, component: "Collider" });
  await engine.call("add-component", { entity: kin, component: "Rigidbody" });
  await engine.call("set-component-field", {
    entity: kin,
    component: "Rigidbody",
    field: "motion",
    value: "kinematic",
  });

  const dyn = await spawn("Dyn");
  await engine.call("set-transform", { entity: dyn, translation: { x: -2, y: 5, z: 0 } });
  await engine.call("add-component", { entity: dyn, component: "Collider" });
  await engine.call("add-component", { entity: dyn, component: "Rigidbody" });

  await engine.call("play");
  await engine.settle(1500);
  expect(await worldY(kin)).toBeCloseTo(5, 1); // kinematic: held in place, gravity ignored
  expect(await worldY(dyn)).toBeLessThan(2); // dynamic: fell to the floor
});

test("a rig with KinematicBones gets one kinematic body per joint", async () => {
  const meshId = trackEntity(caseCleaner, engine, (await engine.importEntity(LEG)).id);
  // A skinned model wraps its rig under a container root; the SkinnedMesh + bones live on a
  // descendant. Resolve it for the component add; the command itself accepts the container root.
  const rigId = await engine.rig(meshId);
  // Adding the component auto-fits a per-bone capsule (BonePhysicsComponent) and enables following.
  await engine.call("add-component", { entity: rigId, component: "KinematicBones" });
  const toggled = await engine.call<KinematicBonesResult>("set-kinematic-bones", {
    entity: meshId, // the model root resolves to the rig descendant server-side
    enabled: true,
  });
  expect(toggled.boneCount).toBe(3); // leg.gltf: Hip / Knee / Ankle
  expect(toggled.enabled).toBe(true);

  await engine.call("play");
  await engine.settle(400);
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.bodyCount).toBeGreaterThanOrEqual(3); // one kinematic body per joint
  expect(state.dynamicCount).toBe(0); // bone bodies are kinematic, not dynamic
});

test("the bones run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
