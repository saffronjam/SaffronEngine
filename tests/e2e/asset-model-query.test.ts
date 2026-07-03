// The asset-model query commands: get-asset-model reads a model's capabilities + bone tree + clips from
// its .smodel container, and list-clips honors an asset selector. Both resolve a mesh sub-asset or a clip
// sub-asset to the same owning container, so the clip<->mesh association is intrinsic (same file). Every
// model opens — a static (unskinned) model reports hasRig=false with an empty bone tree, not an error.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");
const STATIC = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface Bone {
  index: number;
  name: string;
  parent: number;
  joint: boolean;
}
interface Capabilities {
  meshCount: number;
  materialCount: number;
  nodeCount: number;
  hasRig: boolean;
  boneCount: number;
  clipCount: number;
}
interface AssetModel {
  mesh: string;
  name: string;
  capabilities: Capabilities;
  bones: Bone[];
  clips: { id: string; name: string; duration: number }[];
}
interface ModelInfo {
  id: string;
  subAssets: { id: string; name: string; type: string }[];
}

let legModel = "";
let meshSub = "";
let clipSub = "";
let clipName = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  const ref = await engine.call<{ id: string }>("import-model", { path: LEG });
  legModel = ref.id;
  await engine.settle();
  const info = await engine.call<ModelInfo>("model-info", { asset: legModel });
  meshSub = info.subAssets.find((s) => s.type === "mesh")!.id;
  const clip = info.subAssets.find((s) => s.type === "animation")!;
  clipSub = clip.id;
  clipName = clip.name;
});
afterAll(async () => {
  await engine?.shutdown();
});

test("get-asset-model returns the skeleton (joints + parent indices), the clips, and capabilities", async () => {
  const model = await engine.call<AssetModel>("get-asset-model", { asset: legModel });
  expect(model.mesh).toBe(legModel);
  // leg.gltf: LegMesh + Hip/Knee/Ankle joints — the rig is the 3 joints, not the mesh node.
  expect(model.bones.length).toBe(3);
  const byName = new Map(model.bones.map((b) => [b.name, b]));
  expect(byName.get("Hip")?.joint).toBe(true);
  expect(byName.get("Hip")?.parent).toBe(-1);
  expect(byName.get("Knee")?.parent).toBe(byName.get("Hip")?.index);
  expect(byName.get("Ankle")?.parent).toBe(byName.get("Knee")?.index);
  expect(model.clips.length).toBe(1);
  expect(model.clips[0].id).toBe(clipSub);
  expect(model.clips[0].name).toBe(clipName);
  // Capabilities mirror the contents: rigged, three bones, one clip, at least one mesh.
  expect(model.capabilities.hasRig).toBe(true);
  expect(model.capabilities.boneCount).toBe(3);
  expect(model.capabilities.clipCount).toBe(1);
  expect(model.capabilities.meshCount).toBeGreaterThanOrEqual(1);
});

test("get-asset-model on a mesh sub-asset resolves to the same model", async () => {
  const model = await engine.call<AssetModel>("get-asset-model", { asset: meshSub });
  expect(model.mesh).toBe(legModel);
  expect(model.bones.length).toBe(3);
});

test("get-asset-model on a clip sub-asset resolves to the same model (clip<->mesh link is intrinsic)", async () => {
  const model = await engine.call<AssetModel>("get-asset-model", { asset: clipSub });
  expect(model.mesh).toBe(legModel);
  expect(model.clips.some((c) => c.id === clipSub)).toBe(true);
});

test("list-clips honors the asset selector", async () => {
  const filtered = await engine.call<{ clips: { id: string }[] }>("list-clips", { asset: legModel });
  expect(filtered.clips.length).toBe(1);
  expect(filtered.clips[0].id).toBe(clipSub);
});

test("list-assets carries rigged on a rigged model's rows and duration on clips", async () => {
  const list = await engine.call<{
    assets: { id: string; type: string; rigged?: boolean; duration?: number }[];
  }>("list-assets");
  const mesh = list.assets.find((a) => a.id === meshSub);
  expect(mesh?.rigged).toBe(true);
  const clip = list.assets.find((a) => a.id === clipSub);
  expect(clip?.duration).toBeGreaterThan(0);
});

test("get-asset-model on a static (unskinned) model reports hasRig=false, not an error", async () => {
  const ref = await engine.call<{ id: string }>("import-model", { path: STATIC });
  await engine.settle();
  const model = await engine.call<AssetModel>("get-asset-model", { asset: ref.id });
  expect(model.mesh).toBe(ref.id);
  expect(model.capabilities.hasRig).toBe(false);
  expect(model.bones).toEqual([]);
  expect(model.capabilities.boneCount).toBe(0);
  expect(model.capabilities.clipCount).toBe(0);
  expect(model.clips).toEqual([]);
  // A static model is still a renderable model: at least one mesh and its materials are reported.
  expect(model.capabilities.meshCount).toBeGreaterThanOrEqual(1);
  expect(model.capabilities.materialCount).toBeGreaterThanOrEqual(1);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
