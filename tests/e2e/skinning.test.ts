// Skeleton import + GPU skinning over the control plane: a rigged glTF imports as a
// bone-entity hierarchy (one entity per node, joints tagged, parented via parentId),
// the skinned mesh resolves its joints by uuid, and the GPU pass deforms — verified
// by the research-gate screenshot trio (bind pose renders, a moved joint changes the
// pixels, the skinning kill-switch removes the draw). The exact joint math is covered
// by the engine's hierarchy self-test; this file proves the wire + the GPU half.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "skinned-strip.gltf");
const shots: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

interface Entry {
  id: string;
  name: string;
  parentId?: string;
  bone?: boolean;
}

async function entries(): Promise<Entry[]> {
  return (await engine.call<{ entities: Entry[] }>("list-entities")).entities;
}

/// Capture the viewport and wait for the deferred write to land on disk.
async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-skin-${process.pid}-${tag}.png`;
  shots.push(path);
  await engine.call("screenshot", { target: "viewport", path });
  const deadline = Date.now() + 10_000;
  while (!existsSync(path)) {
    if (Date.now() > deadline) {
      throw new Error(`screenshot ${tag} never landed at ${path}`);
    }
    await engine.settle(100);
  }
  await engine.settle(200);
  return readFileSync(path);
}

let meshId = "";

test("a rigged glTF imports as a bone-entity hierarchy", async () => {
  const imported = await engine.importEntity(FIXTURE);
  meshId = imported.id;
  await engine.settle();

  const list = await entries();
  const root = list.find((e) => e.name === "RootJoint");
  const tip = list.find((e) => e.name === "TipJoint");
  const mesh = list.find((e) => e.id === meshId);
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
    const info = await engine.call<{ components: { SkinnedMesh?: Skin } }>("inspect", { entity: e.id });
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
  await engine.call("focus", { entity: meshId });
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

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
