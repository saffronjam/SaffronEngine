// Foot IK as a blend-layer producer: a two-bone analytic solver writes into the
// PoseBuffer override_/weight layer so an animated rig's foot plants on a ground plane instead
// of clipping/floating. The leg.gltf fixture is a 3-joint hip→knee→ankle chain whose KneeBend
// clip drops the ankle; with the ground raised, foot IK lifts the ankle back up to track it.
// This drives the whole path over the control plane and asserts the ankle's WORLD Y (read via
// get-world-transform) rises toward the ground target — the "reacts to the environment" result.
// The pose is frozen (playing=false in Play) so the IK-off vs IK-on readings compare one pose.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let rigId = "";
let ankleId = "";
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");

interface Inspect {
  components: { SkinnedMesh?: { bones: string[] } };
}

const worldY = async (entity: string): Promise<number> =>
  (await engine.call<{ translation: { y: number } }>("get-world-transform", { entity })).translation.y;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  await engine.importEntity(FIXTURE);
  // A rigged import returns the container root (ModelInstance + Relationship + Transform); the
  // SkinnedMesh/AnimationPlayer/FootIk components live on a mesh descendant. Find that descendant
  // and read its bone list — the generic set-component[-field] commands operate on the exact entity,
  // so they must target the rig descendant, not the root.
  const list = (await engine.call<{ entities: { id: string }[] }>("list-entities")).entities;
  let bones: string[] = [];
  for (const e of list) {
    const skin = (await engine.call<Inspect>("inspect", { entity: e.id })).components.SkinnedMesh;
    if (skin) {
      rigId = e.id;
      bones = skin.bones;
      break;
    }
  }
  ankleId = bones[2];
  // hip→knee→ankle chain; the knee bends in -X, so the pole points that way.
  await engine.call("set-component", {
    entity: rigId,
    component: "FootIk",
    json: { enabled: false, groundHeight: 0, chains: [{ upper: 0, mid: 1, end: 2, poleVector: { x: -1, y: 0, z: 0 } }] },
  });
  // Enter Play and bend the knee, then freeze (playing=false) at a bent pose: the ankle is now
  // below full reach, so it has slack for the IK to lift, and the readings compare one pose.
  await engine.call("set-component-field", {
    entity: rigId,
    component: "AnimationPlayer",
    field: "autoplay",
    value: true,
  });
  await engine.call("play");
  await engine.settle(500);
  await engine.call("set-animation-playing", { entity: rigId, playing: false });
  await engine.settle(300);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("the rig is a 3-joint chain", () => {
  expect(ankleId).toBeTruthy();
});

test("raising the ground lifts the ankle's world Y to track the ground target", async () => {
  const yOff = await worldY(ankleId);
  const ground = yOff + 0.08; // above the bent ankle, comfortably within the chain's reach
  await engine.call("set-foot-ik", { entity: rigId, enabled: true, groundHeight: ground });
  await engine.settle(300);
  const yOn = await worldY(ankleId);
  expect(yOn).toBeGreaterThan(yOff + 0.05); // the ankle rose
  expect(Math.abs(yOn - ground)).toBeLessThan(0.03); // and planted on the ground plane
});

test("disabling foot IK reverts the ankle to its animated position", async () => {
  await engine.call("set-foot-ik", { entity: rigId, enabled: true, groundHeight: (await worldY(ankleId)) + 0.08 });
  await engine.settle(300);
  const lifted = await worldY(ankleId);
  await engine.call("set-foot-ik", { entity: rigId, enabled: false });
  await engine.settle(300);
  const reverted = await worldY(ankleId);
  expect(lifted).toBeGreaterThan(reverted + 0.02); // the IK had lifted it; disabling drops it back
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
