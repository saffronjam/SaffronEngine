// The asset editor's composed full-flow: import a model, snapshot the authored project, enter the preview,
// scrub-pose a bone, switch to a second model (enter while entered = swap), exit, and assert the authored
// project.json is BYTE-IDENTICAL and the entity list unchanged. Plus the cross-cutting negative lanes.
// The per-phase tests (asset-model-query, asset-preview) cover the parts; this is the journey + keystone.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");
const STRIP = join(REPO, "tests", "e2e", "fixtures", "skinned-strip.gltf");

interface BoneEntity {
  index: number;
  entity: string;
}
interface EnterResult {
  rootEntity: string;
  bones: BoneEntity[];
}

let legModel = "";
let stripModel = "";
let projectPath = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  legModel = (await engine.call<{ id: string }>("import-model", { path: LEG })).id;
  stripModel = (await engine.call<{ id: string }>("import-model", { path: STRIP })).id;
  await engine.settle();
  // get-project reports an absolute project.json path under the harness's per-boot appdata temp dir.
  projectPath = (await engine.call<{ path: string }>("get-project")).path;
});
afterAll(async () => {
  await engine?.shutdown();
});

async function entityIds(): Promise<string[]> {
  return (await engine.call<{ entities: { id: string }[] }>("list-entities")).entities.map((e) => e.id).sort();
}

test("the full open -> scrub -> switch -> exit journey leaves the authored scene byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");
  const entitiesBefore = await entityIds();

  // 1. Open the leg model; the bone table resolves.
  const leg = await engine.call<EnterResult>("enter-asset-preview", { asset: legModel });
  expect(leg.rootEntity).not.toBe("0");
  expect(leg.bones.length).toBe(3);

  // 2. Paused-pick the clip, scrub, and confirm a bone pose tracks the playhead.
  const clips = await engine.call<{ clips: { id: string }[] }>("get-asset-model", { asset: legModel });
  await engine.call("play-animation", { entity: leg.rootEntity, clip: clips.clips[0].id, paused: true });
  await engine.call("seek-animation", { entity: leg.rootEntity, time: 0.0 });
  await engine.settle(120);
  const pose0 = await engine.call<{ translation: { x: number; y: number; z: number } }>("get-world-transform", {
    entity: leg.bones[leg.bones.length - 1].entity,
  });
  await engine.call("seek-animation", { entity: leg.rootEntity, time: 0.6 });
  await engine.settle(120);
  const pose1 = await engine.call<{ translation: { x: number; y: number; z: number } }>("get-world-transform", {
    entity: leg.bones[leg.bones.length - 1].entity,
  });
  const moved =
    Math.abs(pose1.translation.x - pose0.translation.x) > 1e-4 ||
    Math.abs(pose1.translation.y - pose0.translation.y) > 1e-4 ||
    Math.abs(pose1.translation.z - pose0.translation.z) > 1e-4;
  expect(moved).toBe(true);

  // 3. Switch to a second model while entered (a swap), then exit.
  const strip = await engine.call<EnterResult>("enter-asset-preview", { asset: stripModel });
  expect(strip.rootEntity).not.toBe(leg.rootEntity);
  expect(strip.bones.length).toBe(2);
  const ps = await engine.call<{ previewAsset: string }>("get-play-state");
  expect(ps.previewAsset).toBe(stripModel);
  await engine.call("exit-asset-preview");

  // 4. The authored project is byte-identical and the entity list unchanged.
  await engine.call("save-project");
  expect(readFileSync(projectPath, "utf8")).toBe(before);
  expect(await entityIds()).toEqual(entitiesBefore);
  expect((await engine.call<{ previewAsset: string }>("get-play-state")).previewAsset).toBe("0");
});

test("the negative lanes hold: play during preview, enter during play", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: legModel });
  await expect(engine.call("play")).rejects.toThrow(/asset preview/);
  await engine.call("exit-asset-preview");

  await engine.call("play");
  await expect(engine.call("enter-asset-preview", { asset: legModel })).rejects.toThrow(/stop play/);
  await engine.call("stop");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
