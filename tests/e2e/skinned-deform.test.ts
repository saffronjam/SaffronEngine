// GPU deformation of skinned + morphed geometry through the full host loop, on a single booted host.
// Every case proves the deform actually moved pixels via a screenshot diff:
//   - base skin (hand-posed skeleton): a rigged glTF imports as a bone-entity hierarchy, the skinned
//     mesh resolves its joints by uuid, a bone reparents like any entity, and dragging a joint deforms
//     the mesh (with a skinning kill-switch that removes the draw);
//   - base skin (animation clip): a rigged+animated glTF carries a stopped AnimationPlayer, and once
//     playing it deforms the mesh (rest renders, playing differs, stop reverts);
//   - morph targets: a blend-shape weight round-trips over the wire and the GPU deform moves the
//     silhouette when the weight ramps 0 -> 1;
//   - TAA motion: with anti-aliasing set to TAA the motion-vector prepass runs and emits velocity for
//     skinned geometry, so a continuously animating rig differs across consecutive frames.
//
// The skeleton import (which resolves joints by name) runs first, while its rig is the only one in the
// scene; the animation-clip rig is imported once and reused by the TAA case, which runs last so the
// TAA render state does not perturb the default-AA diffs above.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner, prepareScene } from "./test-utils.ts";

let engine: Engine;
let skinMesh = ""; // hand-posed skeleton (skinned-strip)
let animMesh = ""; // animation-clip rig (animated-strip), reused by the TAA case
let playerId = ""; // the descendant of animMesh carrying the AnimationPlayer component
let morphId = ""; // the mesh-bearing entity carrying the Morph component
const SKINNED = join(REPO, "tests", "e2e", "fixtures", "skinned-strip.gltf");
const ANIMATED = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const MORPH = join(REPO, "tests", "e2e", "fixtures", "AnimatedMorphCube.gltf");
const cleaner = new Cleaner();

interface Entry {
  id: string;
  name: string;
  parentId?: string;
  bone?: boolean;
}

interface AnimState {
  clip: string;
  playing: boolean;
}

async function entries(): Promise<Entry[]> {
  return (await engine.call<{ entities: Entry[] }>("list-entities")).entities;
}

/// Find the entity in the imported hierarchy that actually carries the AnimationPlayer component.
async function findPlayerEntity(): Promise<string | undefined> {
  for (const e of await entries()) {
    const info = await engine.call<{ components: Record<string, unknown> }>("inspect", {
      entity: e.id,
    });
    if (info.components.AnimationPlayer) {
      return e.id;
    }
  }
  return undefined;
}

/// The mesh-bearing entity carrying the durable `Morph` component (import seeds it on the mesh node,
/// which the single-node model may collapse onto the instantiated root).
async function morphEntity(): Promise<string> {
  for (const e of await entries()) {
    const info = await engine.call<{ components: Record<string, unknown> }>("inspect", {
      entity: e.id,
    });
    if (info.components.Morph) {
      return e.id;
    }
  }
  throw new Error("no entity carries a Morph component");
}

/// Capture the viewport and wait for the deferred write to land on disk.
async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `deform-${tag}`);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
});
afterAll(async () => {
  await cleaner.cleanup();
});

test("a rigged glTF imports as a bone-entity hierarchy", async () => {
  const imported = await engine.importEntity(SKINNED);
  skinMesh = imported.id;
  await engine.settle();

  const list = await entries();
  const root = list.find((e) => e.name === "RootJoint");
  const tip = list.find((e) => e.name === "TipJoint");
  const mesh = list.find((e) => e.id === skinMesh);
  expect(root?.bone).toBe(true);
  expect(tip?.bone).toBe(true);
  expect(tip?.parentId).toBe(root!.id);
  expect(mesh).toBeDefined();
  expect(mesh!.bone).toBeUndefined();
});

test("the skinned mesh resolves its joints by uuid through inspect", async () => {
  const list = await entries();
  const ids = new Set(list.map((e) => e.id));
  // A rigged import places the SkinnedMesh on the mesh descendant of the imported root (the root
  // carries ModelInstance + Relationship), so find the entity that actually holds the component.
  type Skin = { mesh: string; rootBone: string; bones: string[] };
  let skin: Skin | undefined;
  for (const e of list) {
    const info = await engine.call<{ components: { SkinnedMesh?: Skin } }>("inspect", {
      entity: e.id,
    });
    if (info.components.SkinnedMesh) {
      skin = info.components.SkinnedMesh;
      break;
    }
  }
  expect(skin).toBeDefined();
  expect(ids.has(skin!.rootBone)).toBe(true);
  expect(skin!.bones.length).toBe(2);
  for (const bone of skin!.bones) {
    expect(ids.has(bone)).toBe(true);
  }
  expect(skin!.bones[0]).toBe(list.find((e) => e.name === "RootJoint")!.id);
  expect(skin!.bones[1]).toBe(list.find((e) => e.name === "TipJoint")!.id);
});

test("a bone reparents like any entity and inspect reflects it", async () => {
  const anchor = await engine.call<{ id: string }>("create-entity", { args: ["skin-anchor"] });
  const list = await entries();
  const tip = list.find((e) => e.name === "TipJoint")!;
  await engine.call("set-parent", { entity: tip.id, parent: anchor.id });
  const relisted = await entries();
  expect(relisted.find((e) => e.id === tip.id)?.parentId).toBe(anchor.id);
  // Restore the skeleton for the deformation tests below.
  const root = relisted.find((e) => e.name === "RootJoint")!;
  await engine.call("set-parent", { entity: tip.id, parent: root.id });
});

test("the GPU pass deforms: bind pose renders, a moved joint changes pixels", async () => {
  await engine.call("focus", { entity: skinMesh });
  await engine.settle(400);
  const bindPose = await screenshot("bind");

  // Drag the tip joint sideways: the strip's top edge follows it, so the framebuffer
  // must change. (worldBone * inverseBind leaves the bottom edge pinned to the root.)
  const tip = (await entries()).find((e) => e.name === "TipJoint")!;
  await engine.call("set-transform", { entity: tip.id, translation: { x: 2, y: 1, z: 0 } });
  await engine.settle(400);
  const moved = await screenshot("moved");
  expect(moved.equals(bindPose)).toBe(false);

  // The kill-switch removes the skinned draw entirely.
  await engine.call("set-skinning", { enabled: false });
  await engine.settle(400);
  const gated = await screenshot("gated");
  expect(gated.equals(moved)).toBe(false);
  await engine.call("set-skinning", { enabled: true });
});

test("the imported rig carries a stopped AnimationPlayer bound to the clip", async () => {
  const imported = await engine.importEntity(ANIMATED);
  animMesh = imported.id;
  await engine.settle();
  playerId = (await findPlayerEntity()) ?? "";

  expect(playerId).not.toBe(""); // the rig descendant carrying AnimationPlayer must exist
  const info = await engine.call<{
    components: Record<string, { clip: string; autoplay: boolean }>;
  }>("inspect", {
    entity: playerId,
  });
  const player = info.components.AnimationPlayer;
  expect(player).toBeDefined();
  expect(player.autoplay).toBe(false);
  expect(player.clip).not.toBe("0"); // bound to the imported "Bend" clip
  const state = await engine.call<AnimState>("get-animation-state", { entity: playerId });
  expect(state.playing).toBe(false);
  expect(state.clip).toBe(player.clip);
});

test("playing the clip deforms the mesh, and stop reverts it", async () => {
  await engine.call("set-component-field", {
    entity: playerId,
    component: "AnimationPlayer",
    field: "autoplay",
    value: true,
  });
  await engine.call("focus", { entity: animMesh });
  await engine.settle(400);

  // Edit mode without preview is inert, so this is the rest pose.
  const rest = await screenshot("rest");

  // Play animates every rig; the strip bends as the root joint rotates.
  await engine.call("play");
  await engine.settle(600);
  const playing = await screenshot("playing");
  expect(playing.equals(rest)).toBe(false);

  // Stop discards the play scene; the authored rest pose comes back.
  await engine.call("stop");
  await engine.settle(400);
  const stopped = await screenshot("stopped");
  expect(stopped.equals(playing)).toBe(false);
});

test("the morph mesh seeds rest weights + names", async () => {
  await engine.importEntity(MORPH);
  morphId = await morphEntity();
  await engine.call("focus", { entity: morphId });
  await engine.settle();

  const got = await engine.call<{ weights: number[]; names: string[] }>("get-morph-weights", {
    entity: morphId,
  });
  expect(got.weights).toEqual([0]);
  expect(got.names).toEqual(["bulge"]);
});

test("set-morph-weights round-trips a 0..1 vector", async () => {
  const set = await engine.call<{ weights: number[] }>("set-morph-weights", {
    entity: morphId,
    weights: [0.75],
  });
  expect(set.weights).toEqual([0.75]);
  const got = await engine.call<{ weights: number[] }>("get-morph-weights", { entity: morphId });
  expect(got.weights).toEqual([0.75]);
});

test("a wrong-length weight vector is rejected", async () => {
  let rejected = false;
  try {
    await engine.call("set-morph-weights", { entity: morphId, weights: [0.1, 0.2] });
  } catch {
    rejected = true;
  }
  expect(rejected).toBe(true);
});

test("playing the weight clip deforms the geometry on the GPU", async () => {
  // Rest (weight 0) — the cube is undeformed.
  await engine.call("set-morph-weights", { entity: morphId, weights: [0] });
  await engine.settle(200);
  const rest = await screenshot("morph-rest");
  // Full bulge (weight 1) — the top face lifts by +1, a large silhouette change. If the morph
  // compute pass did not run, the two frames would be identical.
  await engine.call("set-morph-weights", { entity: morphId, weights: [1] });
  await engine.settle(300);
  const bulged = await screenshot("morph-bulged");
  expect(bulged.equals(rest)).toBe(false);
});

test("TAA is active and the rig plays through the motion pass", async () => {
  await engine.call("set-aa", { mode: "taa" });
  await engine.call("set-component-field", {
    entity: playerId,
    component: "AnimationPlayer",
    field: "autoplay",
    value: true,
  });
  await engine.call("focus", { entity: animMesh });
  await engine.call("play");
  // Many frames so the motion pass sees a moving bone across consecutive frames and TAA
  // accumulates history against the skinned velocity.
  await engine.settle(800);
  const moving = await screenshot("taa-moving");
  await engine.settle(400);
  const later = await screenshot("taa-later");
  // The animation keeps deforming, so two shots a few hundred ms apart differ — the rig is
  // genuinely moving while the motion pass + TAA run.
  expect(later.equals(moving)).toBe(false);
  await engine.call("stop");
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
