// Phase 9 of the physics plan: an active ragdoll mixed against the animation through the per-bone
// PoseBuffer blend layer. A leaf bone is blown to physics (set-ragdoll bodyWeight 1) and falls under
// gravity; turning the motors on (active) drives the bodies back toward the animated pose; ramping
// the weight back to 0 returns the bone to the animation — a hit blends a limb to physics and recovers.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
let rigId = ""; // the SkinnedMesh/BonePhysics carrier under the model's container root
let ankle = "";
let restAnkleY = 0;
let beforeY = 0; // the animated ankle height at play start, before physics takes over

const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");

interface Inspect {
  components: { SkinnedMesh?: { bones: string[] }; BonePhysics?: { bones: unknown[] } };
}
interface RagdollResult {
  present: boolean;
  active: boolean;
  bodyWeight: number;
  bones: number;
}

const worldY = async (entity: string): Promise<number> =>
  (await engine.call<{ translation: { y: number } }>("get-world-transform", { entity })).translation.y;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  meshId = (await engine.importEntity(LEG)).id;
  rigId = await engine.rig(meshId); // the rig descendant carries SkinnedMesh + BonePhysics
  const info = await engine.call<Inspect>("inspect", { entity: rigId });
  ankle = info.components.SkinnedMesh!.bones[2]; // hip / knee / ankle — the leaf
  restAnkleY = await worldY(ankle);

  const floor = (await engine.call<{ id: string }>("create-entity", { name: "Floor" })).id;
  await engine.call("set-transform", { entity: floor, translation: { x: 0, y: restAnkleY - 1.5, z: 0 } });
  await engine.call("add-component", { entity: floor, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor,
    component: "Collider",
    field: "halfExtents",
    value: { x: 10, y: 0.1, z: 10 },
  });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("import auto-fits a BonePhysicsComponent (one entry per bone)", async () => {
  const info = await engine.call<Inspect>("inspect", { entity: rigId });
  expect(info.components.BonePhysics?.bones.length).toBe(3);
});

test("set-ragdoll auto-creates the ragdoll and reports its blend state", async () => {
  await engine.call("play");
  await engine.settle(200);
  beforeY = await worldY(ankle); // animated pose at play start, before physics

  // No enable-ragdoll round-trip: the first set-ragdoll builds the ragdoll, passive (motors off).
  const created = await engine.call<RagdollResult>("set-ragdoll", { entity: meshId, bodyWeight: 1 });
  expect(created.present).toBe(true);
  expect(created.active).toBe(false);
  expect(created.bones).toBe(3);
  expect(created.bodyWeight).toBeCloseTo(1, 1);

  const got = await engine.call<RagdollResult>("get-ragdoll", { entity: meshId });
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
  await engine.call<RagdollResult>("set-ragdoll", { entity: meshId, active: true });
  expect((await engine.call<RagdollResult>("get-ragdoll", { entity: meshId })).active).toBe(true);
  await engine.settle(1000);

  // Ramp the physics weight back to 0: the bone follows the animation again (the recover).
  await engine.call<RagdollResult>("set-ragdoll", { entity: meshId, active: false, bodyWeight: 0 });
  await engine.settle(600);
  const recoverY = await worldY(ankle);
  expect(recoverY).toBeGreaterThan(limpY + 0.1); // back up at the animated pose
  expect(Math.abs(recoverY - beforeY)).toBeLessThan(0.3);

  await engine.call("stop");
  await engine.settle();
}, 20000);

test("the active-ragdoll run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
