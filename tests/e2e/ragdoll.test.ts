// Phase 8 of the physics plan: a passive ragdoll built from the reserved BonePhysicsComponent. A
// rigged character is told to go limp (enable-ragdoll); the bodies drive the bones (write into
// PoseOverride at full physics weight), so a leaf bone collapses under gravity and settles on the
// floor. Disabling restores the animation pose. The BonePhysicsComponent is auto-fit on import.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
let rigId = ""; // the SkinnedMesh/BonePhysics carrier under the model's container root
let ankle = "";
let restAnkleY = 0;

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
  restAnkleY = await worldY(ankle); // authored rest pose (Edit)

  // A floor below the leg so the collapsed ragdoll settles rather than falling forever.
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

test("enable-ragdoll collapses the leaf bone onto the floor; disable restores the pose", async () => {
  await engine.call("play");
  await engine.settle(200);
  const beforeY = await worldY(ankle); // ~rest pose at play start

  const enabled = await engine.call<RagdollResult>("enable-ragdoll", { entity: meshId, enabled: true });
  expect(enabled.present).toBe(true);
  expect(enabled.bones).toBe(3);

  await engine.settle(2500); // collapse + settle
  const limpY = await worldY(ankle);
  await engine.settle(400);
  const settledY = await worldY(ankle);

  expect(limpY).toBeLessThan(beforeY - 0.2); // the bone fell under gravity
  expect(limpY).toBeGreaterThan(restAnkleY - 2.0); // settled near the floor, did not tunnel away
  expect(Math.abs(settledY - limpY)).toBeLessThan(0.15); // came to rest

  // Disable -> the ragdoll is removed; the bone reverts toward the animation/rest pose.
  const disabled = await engine.call<RagdollResult>("enable-ragdoll", { entity: meshId, enabled: false });
  expect(disabled.present).toBe(false);
  await engine.settle(400);
  expect(await worldY(ankle)).toBeGreaterThan(limpY + 0.1); // back up toward rest

  await engine.call("stop");
  await engine.settle();
});

test("the ragdoll run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
