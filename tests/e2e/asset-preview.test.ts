// The asset preview scene: enter-asset-preview spawns a model into an isolated preview scene and makes
// AssetPreview the active view (set-active-view assetPreview), so animation commands drive it unchanged;
// exit-asset-preview drops the preview and returns the renderer to the Scene view + the authored scene +
// camera. set-active-view {scene|assetPreview} switches the rendered view WITHOUT dropping the preview (it
// stays alive, parked, ready to re-activate). Each view owns an independent offscreen target sized via
// set-viewport-size {view}. The keystone invariant: a full enter -> scrub -> exit round-trip leaves
// project.json byte-identical. Mutual exclusion with play and authored-scene mutations is enforced.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const LEG = join(REPO, "tests", "e2e", "fixtures", "leg.gltf");
const STRIP = join(REPO, "tests", "e2e", "fixtures", "skinned-strip.gltf");
const STATIC = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

let legModel = "";
let stripModel = "";
let staticModel = "";
let projectPath = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  legModel = (await engine.call("import-model", { path: LEG })).id;
  stripModel = (await engine.call("import-model", { path: STRIP })).id;
  staticModel = (await engine.call("import-model", { path: STATIC })).id;
  await engine.settle();
  // get-project reports an absolute project.json path under the harness's per-boot appdata temp dir.
  projectPath = (await engine.call("get-project")).path;
});
afterAll(async () => {
  await engine?.shutdown();
});

async function listEntityIds(): Promise<string[]> {
  const list = await engine.call("list-entities");
  return list.entities.map((e) => e.id).sort();
}

test("enter-asset-preview spawns the model and reports the bone table", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call("enter-asset-preview", { asset: legModel });
  expect(res.rootEntity).not.toBe("0");
  expect(res.bones.length).toBe(3); // Hip/Knee/Ankle joints map to spawned entities
  for (const b of res.bones) {
    expect(b.entity).not.toBe("0");
  }
  const ps = await engine.call("get-play-state");
  expect(ps.state).toBe("edit"); // preview stays in Edit
  expect(ps.previewAsset).toBe(legModel);
  await engine.call("exit-asset-preview");
});

test("seek advances the previewed model's animation state", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call("enter-asset-preview", { asset: legModel });
  const root = res.rootEntity;
  const s0 = await engine.call("seek-animation", { entity: root, time: 0.0 });
  await engine.call("seek-animation", { entity: root, time: 0.4 });
  const state = await engine.call("get-animation-state", { entity: root });
  expect(state.time).toBeGreaterThan(s0.time);
  expect(state.time).toBeCloseTo(0.4, 2);
  await engine.call("exit-asset-preview");
});

test("play during preview is rejected; enter during play is rejected", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: legModel });
  await expect(engine.call("play")).rejects.toThrow(/asset preview/);
  await engine.call("exit-asset-preview");

  await engine.call("play");
  await expect(engine.call("enter-asset-preview", { asset: legModel })).rejects.toThrow(
    /stop play/,
  );
  await engine.call("stop");
});

test("project + asset mutations are rejected while previewing", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: legModel });
  await expect(engine.call("import-model", { path: LEG })).rejects.toThrow(/asset preview/);
  await expect(engine.call("reload-project")).rejects.toThrow(/asset preview/);
  await engine.call("exit-asset-preview");
});

test("a preview round-trip leaves project.json byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");
  const entitiesBefore = await listEntityIds();

  await engine.call("enter-asset-preview", { asset: legModel });
  // Re-enter the same model is a swap (drop + respawn); exit must still land cleanly.
  const res = await engine.call("enter-asset-preview", { asset: legModel });
  await engine.call("seek-animation", { entity: res.rootEntity, time: 0.3 });
  await engine.call("exit-asset-preview");

  await engine.call("save-project");
  const after = readFileSync(projectPath, "utf8");
  expect(after).toBe(before); // includes the editorCamera block: the engine-side camera restore holds
  expect(await listEntityIds()).toEqual(entitiesBefore);

  const ps = await engine.call("get-play-state");
  expect(ps.state).toBe("edit");
  expect(ps.previewAsset).toBe("0");
});

test("scrubbing the previewed model moves its bones (the pose follows the playhead)", async () => {
  await engine.call("exit-asset-preview");
  const entered = await engine.call("enter-asset-preview", { asset: legModel });
  // The first seek arms previewInEdit, so the evaluator poses the rig at the playhead.
  await engine.call("seek-animation", { entity: entered.rootEntity, time: 0 });
  await engine.settle(150);
  const before = await Promise.all(
    entered.bones.map((b) => engine.call("get-world-transform", { entity: b.entity })),
  );
  await engine.call("seek-animation", { entity: entered.rootEntity, time: 0.6 });
  await engine.settle(150);
  const after = await Promise.all(
    entered.bones.map((b) => engine.call("get-world-transform", { entity: b.entity })),
  );
  const moved = before.some((b, i) => {
    const a = after[i].translation;
    return (
      Math.abs(a.x - b.translation.x) > 1e-4 ||
      Math.abs(a.y - b.translation.y) > 1e-4 ||
      Math.abs(a.z - b.translation.z) > 1e-4
    );
  });
  expect(moved).toBe(true); // KneeBend deforms the chain, so a joint's world position tracks the seek
  // A burst of seeks stays clean and the final state matches the last seek.
  for (const t of [0.1, 0.3, 0.5, 0.2, 0.45]) {
    await engine.call("seek-animation", { entity: entered.rootEntity, time: t });
  }
  const final = await engine.call("get-animation-state", { entity: entered.rootEntity });
  expect(final.time).toBeCloseTo(0.45, 2);
  await engine.call("exit-asset-preview");
});

test("set-skeleton-highlight tints a joint without moving scene selection", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call("enter-asset-preview", { asset: legModel });
  const model = await engine.call("get-asset-model", {
    asset: legModel,
  });
  const joint = model.bones.find((b) => b.joint)!;
  const overlay = await engine.call("set-skeleton-highlight", {
    joint: joint.index,
  });
  expect(overlay.highlightJoint).toBe(joint.index);
  expect(overlay.show).toBe(true); // preview defaults the overlay on
  // Selection stayed on the previewed model (the highlight uses a dedicated channel, not scene selection),
  // so the selection-keyed animation state the timeline reads is still resolvable.
  const state = await engine.call("get-animation-state", { entity: res.rootEntity });
  expect(state).toBeDefined();
  await engine.call("set-skeleton-highlight", { joint: -1 });
  await engine.call("exit-asset-preview");
});

test("seek-animation accepts seekBlend and still lands on the seeked time", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call("enter-asset-preview", { asset: legModel });
  // seekBlend eases the POSE toward the time over 0.1s; the reported playhead is still the seeked time.
  const state = await engine.call("seek-animation", {
    entity: res.rootEntity,
    time: 0.5,
    seekBlend: 0.1,
  });
  expect(state.time).toBeCloseTo(0.5, 2);
  await engine.call("exit-asset-preview");
});

test("pick-skeleton-joint returns a bone for a wide-radius viewport click, none when not previewing", async () => {
  await engine.call("exit-asset-preview");
  // Not previewing → no joint.
  const idle = await engine.call("pick-skeleton-joint", {
    u: 0.5,
    v: 0.5,
  });
  expect(idle.found).toBe(false);

  await engine.call("enter-asset-preview", { asset: legModel });
  await engine.settle(60);
  // A radius covering the whole viewport always resolves the nearest visible joint of the framed rig.
  const hit = await engine.call("pick-skeleton-joint", {
    u: 0.5,
    v: 0.5,
    radiusPx: 5000,
  });
  expect(hit.found).toBe(true);
  expect(hit.nodeIndex).toBeGreaterThanOrEqual(0);
  await engine.call("exit-asset-preview");
});

test("set-asset-preview-options toggles the floor slab live", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: legModel });
  const withFloor = (await engine.call("list-entities")).entities.length;
  const off = await engine.call("set-asset-preview-options", { floor: false });
  expect(off.floor).toBe(false);
  const withoutFloor = (await engine.call("list-entities")).entities.length;
  expect(withoutFloor).toBe(withFloor - 1);
  const on = await engine.call("set-asset-preview-options", { floor: true });
  expect(on.floor).toBe(true);
  expect((await engine.call("list-entities")).entities.length).toBe(withFloor);
  await engine.call("exit-asset-preview");
});

test("set-active-view scene routes activeScene to the authored scene; assetPreview returns to the preview", async () => {
  await engine.call("exit-asset-preview");
  const authoredBefore = await listEntityIds();

  await engine.call("enter-asset-preview", { asset: legModel });
  const previewEntities = await listEntityIds();
  expect(previewEntities).not.toEqual(authoredBefore); // the preview scene is a different entity set

  // set-active-view scene deactivates the preview view WITHOUT dropping it — activeScene routes back to
  // the authored scene and previewing() clears, so the authored scene is editable again.
  await engine.call("set-active-view", { view: "scene" });
  expect(await listEntityIds()).toEqual(authoredBefore);
  // previewAsset stays set (the preview scene is alive, just not the active view), so re-activating restores it.
  expect((await engine.call("get-play-state")).previewAsset).toBe(legModel);

  await engine.call("set-active-view", { view: "assetPreview" });
  expect(await listEntityIds()).toEqual(previewEntities); // back to the same preview — no re-spawn

  await engine.call("exit-asset-preview");
  expect(await listEntityIds()).toEqual(authoredBefore);
});

test("an enter -> set-active-view scene -> assetPreview -> exit round-trip leaves project.json byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");

  await engine.call("enter-asset-preview", { asset: legModel });
  await engine.call("set-active-view", { view: "scene" });
  await engine.call("set-active-view", { view: "assetPreview" });
  await engine.call("exit-asset-preview");

  await engine.call("save-project");
  expect(readFileSync(projectPath, "utf8")).toBe(before);
});

test("set-active-view round-trips the active-view token and rejects an unknown one", async () => {
  await engine.call("exit-asset-preview");
  const toScene = await engine.call("set-active-view", { view: "scene" });
  expect(toScene.view).toBe("scene");
  const toPreview = await engine.call("set-active-view", { view: "assetPreview" });
  expect(toPreview.view).toBe("assetPreview"); // switching the rendered view needs no live preview scene
  await expect(engine.call("set-active-view", { view: "nope" })).rejects.toThrow(); // one canonical token per view
  await engine.call("set-active-view", { view: "scene" }); // leave the renderer on the scene view
});

test("set-viewport-size targets independent per-view offscreen sizes", async () => {
  // Independent per-view offscreen sizing is the publish-mode (editor) behavior: each view owns its own
  // render target driven by set-viewport-size {view}. In present mode the hidden window drives the scene
  // size instead (host.cppm: !shmPublish tracks app.window), so boot a dedicated shm-publish engine — the
  // engine creates + unlinks its own segments, so no reader is needed.
  const stamp = `${process.pid}-${Date.now()}`;
  const shm = await Engine.boot({
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_VIEWPORT_SHM_SCENE: `/saffron-e2e-scene-${stamp}`,
    SAFFRON_VIEWPORT_SHM_ASSET: `/saffron-e2e-asset-${stamp}`,
  });
  try {
    await shm.call("set-active-view", { view: "scene" });
    // Each view owns its own offscreen target; size them differently. A parked view's resize is deferred
    // until it is activated, so the assetPreview target reaches 800x600 only once it becomes the active view.
    await shm.call("set-viewport-size", { view: "scene", width: 640, height: 360 });
    await shm.call("set-viewport-size", { view: "assetPreview", width: 800, height: 600 });
    await shm.settle();

    // viewport-native-info reports the ACTIVE view's offscreen size.
    const scene = await shm.call("viewport-native-info");
    expect(scene.width).toBe(640);
    expect(scene.height).toBe(360);

    await shm.call("set-active-view", { view: "assetPreview" });
    await shm.settle();
    const preview = await shm.call("viewport-native-info");
    expect(preview.width).toBe(800);
    expect(preview.height).toBe(600);

    // Switching back, the scene view kept its own size — two fully independent targets.
    await shm.call("set-active-view", { view: "scene" });
    await shm.settle();
    const sceneAgain = await shm.call("viewport-native-info");
    expect(sceneAgain.width).toBe(640);
    expect(sceneAgain.height).toBe(360);
  } finally {
    await shm.shutdown();
  }
});

// A static (unskinned) model: the asset editor opens every model, not just rigged ones. get-asset-model
// reports hasRig=false with empty bones/clips but a real mesh + material count; enter spawns a
// MeshComponent root (no skeleton) with an empty bone table; a preview round-trip stays byte-identical.
test("get-asset-model on a static model: hasRig=false, empty bones/clips, a real mesh + material count", async () => {
  const model = await engine.call("get-asset-model", { asset: staticModel });
  expect(model.mesh).toBe(staticModel);
  expect(model.capabilities.hasRig).toBe(false);
  expect(model.capabilities.boneCount).toBe(0);
  expect(model.bones).toEqual([]);
  expect(model.capabilities.clipCount).toBe(0);
  expect(model.clips).toEqual([]);
  expect(model.capabilities.meshCount).toBeGreaterThanOrEqual(1);
  expect(model.capabilities.materialCount).toBeGreaterThanOrEqual(1);
});

test("enter-asset-preview spawns a static model and returns a non-zero root with no bones", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call("enter-asset-preview", { asset: staticModel });
  expect(res.rootEntity).not.toBe("0");
  expect(res.bones).toEqual([]); // static → no skeleton, no bone-entity table
  const ps = await engine.call("get-play-state");
  expect(ps.state).toBe("edit"); // preview stays in Edit for static models too
  expect(ps.previewAsset).toBe(staticModel);
  await engine.call("exit-asset-preview");
});

test("pick-skeleton-joint finds nothing on a static model (no skeleton)", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: staticModel });
  await engine.settle(60);
  const hit = await engine.call("pick-skeleton-joint", {
    u: 0.5,
    v: 0.5,
    radiusPx: 5000,
  });
  expect(hit.found).toBe(false);
  expect(hit.nodeIndex).toBe(-1);
  await engine.call("exit-asset-preview");
});

test("a static-model preview round-trip leaves project.json byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");

  await engine.call("enter-asset-preview", { asset: staticModel });
  await engine.call("exit-asset-preview");

  await engine.call("save-project");
  expect(readFileSync(projectPath, "utf8")).toBe(before);
  expect((await engine.call("get-play-state")).previewAsset).toBe("0");
});

// Switching to a SECOND, distinct model while entered is a swap: the new root differs, its bone table
// reflects the new rig, and previewAsset tracks it. Exiting still lands the authored scene byte-identical
// with the entity list unchanged.
test("switching to a second model while entered swaps the preview and exits byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");
  const entitiesBefore = await listEntityIds();

  const leg = await engine.call("enter-asset-preview", { asset: legModel });
  expect(leg.bones.length).toBe(3);
  const strip = await engine.call("enter-asset-preview", { asset: stripModel });
  expect(strip.rootEntity).not.toBe(leg.rootEntity);
  expect(strip.bones.length).toBe(2);
  expect((await engine.call("get-play-state")).previewAsset).toBe(stripModel);
  await engine.call("exit-asset-preview");

  await engine.call("save-project");
  expect(readFileSync(projectPath, "utf8")).toBe(before);
  expect(await listEntityIds()).toEqual(entitiesBefore);
  expect((await engine.call("get-play-state")).previewAsset).toBe("0");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
