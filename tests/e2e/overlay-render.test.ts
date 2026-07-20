// The viewport's debug render-output + overlay surface end to end on a single booted host, each case
// proving the mode/overlay actually reaches the framebuffer via a screenshot diff:
//   - view mode: the debug render-output selector (set-view-mode) echoes, reads back through
//     render-stats, and a few modes (wireframe / albedo / detail-lighting / lit-wireframe) change the
//     render;
//   - skeleton overlay: the native line-skeleton (set/get-skeleton-overlay) round-trips and draws bone
//     segments + joint dots over the selected rig;
//   - debug overlays: the world-space debug toggles (set/get-debug-overlays — bounds / scene AABB /
//     light volumes / grid / colliders) round-trip, partial-update, render, and survive project
//     save/load.
//
// One boot. View mode runs first against a cube-only scene (matching its standalone baseline); the rig
// is imported for the skeleton case; the debug save/load case runs last because it reloads the project
// (invalidating entity ids), so nothing that depends on a prior id follows it.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner, prepareScene } from "./test-utils.ts";

let engine: Engine;
let cubeId = "";
let rigId = "";
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const projectDir = `/tmp/saffron-e2e-overlay-project-${process.pid}`;
const cleaner = new Cleaner();

interface Ref {
  id: string;
  name: string;
}
interface ViewModeResult {
  viewMode: string;
}
interface RenderStats {
  viewMode: string;
}
interface DebugOverlays {
  bounds: boolean;
  sceneAabb: boolean;
  lightVolumes: boolean;
  grid: boolean;
  colliders: boolean;
}
interface OverlayState {
  show: boolean;
  axes: boolean;
  jointSize: number;
}
interface Entry {
  id: string;
  name: string;
}

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `overlay-${tag}`);
}

beforeAll(async () => {
  cleaner.defer(() => rmSync(projectDir, { recursive: true, force: true }));
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  cubeId = cube.id;
  await engine.call("focus", { entity: cube.id });
  await engine.settle();
});
afterAll(async () => {
  await cleaner.cleanup();
});

test("the default view mode is lit", async () => {
  const stats = await engine.call<RenderStats>("render-stats", {});
  expect(stats.viewMode).toBe("lit");
});

test("set-view-mode echoes the mode and reads back through render-stats", async () => {
  const set = await engine.call<ViewModeResult>("set-view-mode", { mode: "wireframe" });
  expect(set.viewMode).toBe("wireframe");
  const stats = await engine.call<RenderStats>("render-stats", {});
  expect(stats.viewMode).toBe("wireframe");
});

test("wireframe changes the render", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit");
  await engine.call("set-view-mode", { mode: "wireframe" });
  await engine.settle(300);
  const wire = await screenshot("wire");
  expect(wire.equals(lit)).toBe(false);
});

test("a buffer channel (albedo) round-trips and changes the render", async () => {
  const set = await engine.call<ViewModeResult>("set-view-mode", { mode: "albedo" });
  expect(set.viewMode).toBe("albedo");
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit2");
  await engine.call("set-view-mode", { mode: "albedo" });
  await engine.settle(300);
  const albedo = await screenshot("albedo");
  expect(albedo.equals(lit)).toBe(false);
});

// Every mode beyond the originals: the in-fragment debug channels (unlit / detail-lighting /
// lighting-only / reflections / depth / ambient-occlusion / gi / light-complexity) plus the two
// dedicated passes (lit-wireframe, motion-vectors). All must echo + read back through
// render-stats; the dedicated passes no-op gracefully when their inputs are absent, but the
// command round-trip is unconditional.
const NEW_MODES = [
  "unlit",
  "lit-wireframe",
  "detail-lighting",
  "lighting-only",
  "reflections",
  "depth",
  "ambient-occlusion",
  "gi",
  "light-complexity",
  "motion-vectors",
];

test("every new view mode echoes and reads back through render-stats", async () => {
  for (const mode of NEW_MODES) {
    const set = await engine.call<ViewModeResult>("set-view-mode", { mode });
    expect(set.viewMode).toBe(mode);
    const stats = await engine.call<RenderStats>("render-stats", {});
    expect(stats.viewMode).toBe(mode);
  }
  await engine.call("set-view-mode", { mode: "lit" });
});

test("detail-lighting changes the render", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit3");
  await engine.call("set-view-mode", { mode: "detail-lighting" });
  await engine.settle(300);
  const detail = await screenshot("detail");
  expect(detail.equals(lit)).toBe(false);
});

test("lit-wireframe overlays edges on the shaded scene", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit4");
  await engine.call("set-view-mode", { mode: "lit-wireframe" });
  await engine.settle(300);
  const litWire = await screenshot("lit-wire");
  expect(litWire.equals(lit)).toBe(false);
  // Reset to lit so the overlay diffs below render against the ordinary shaded scene.
  await engine.call("set-view-mode", { mode: "lit" });
});

test("the skeleton overlay is off by default", async () => {
  const state = await engine.call<OverlayState>("get-skeleton-overlay", {});
  expect(state.show).toBe(false);
});

test("set-skeleton-overlay round-trips through get", async () => {
  const set = await engine.call<OverlayState>("set-skeleton-overlay", {
    show: true,
    axes: true,
    jointSize: 6,
  });
  expect(set.show).toBe(true);
  expect(set.axes).toBe(true);
  expect(set.jointSize).toBeCloseTo(6, 4);
  const got = await engine.call<OverlayState>("get-skeleton-overlay", {});
  expect(got.show).toBe(true);
  expect(got.axes).toBe(true);
  await engine.call("set-skeleton-overlay", { show: false });
});

test("turning bones on draws the skeleton over the selected rig", async () => {
  await engine.importEntity(FIXTURE);
  await engine.settle();

  // A rigged import places the SkinnedMesh on the mesh descendant of the imported root (the root
  // carries only ModelInstance + Relationship + Transform). The skeleton overlay self-gates to the
  // selected entity's SkinnedMeshComponent, so select the descendant that actually holds the rig.
  const list = (await engine.call<{ entities: Entry[] }>("list-entities")).entities;
  for (const e of list) {
    const info = await engine.call<{ components: { SkinnedMesh?: unknown } }>("inspect", {
      entity: e.id,
    });
    if (info.components.SkinnedMesh) {
      rigId = e.id;
      break;
    }
  }
  expect(rigId).not.toBe("");

  await engine.call("select", { entity: rigId });
  await engine.call("focus", { entity: rigId });
  await engine.settle();

  await engine.call("set-skeleton-overlay", { show: false });
  await engine.settle(300);
  const off = await screenshot("skel-off");
  await engine.call("set-skeleton-overlay", { show: true, axes: true });
  await engine.settle(300);
  const on = await screenshot("skel-on");
  expect(on.equals(off)).toBe(false);
  // Clear the skeleton overlay so it is constant (off) background for the debug-overlay diffs below.
  await engine.call("set-skeleton-overlay", { show: false });
});

test("the debug overlays are off by default", async () => {
  const state = await engine.call<DebugOverlays>("get-debug-overlays", {});
  expect(state.bounds).toBe(false);
  expect(state.sceneAabb).toBe(false);
  expect(state.lightVolumes).toBe(false);
  expect(state.grid).toBe(false);
  expect(state.colliders).toBe(false);
});

test("set-debug-overlays round-trips through get", async () => {
  const set = await engine.call<DebugOverlays>("set-debug-overlays", { bounds: true });
  expect(set.bounds).toBe(true);
  const got = await engine.call<DebugOverlays>("get-debug-overlays", {});
  expect(got.bounds).toBe(true);
});

test("a partial update leaves the other flags untouched", async () => {
  await engine.call("set-debug-overlays", { bounds: true });
  const after = await engine.call<DebugOverlays>("set-debug-overlays", { sceneAabb: true });
  expect(after.bounds).toBe(true);
  expect(after.sceneAabb).toBe(true);
  expect(after.lightVolumes).toBe(false);
});

test("turning bounds on draws the AABB over the mesh", async () => {
  // Re-aim the camera at the cube (the skeleton case pointed it at the rig).
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  await engine.call("focus", { entity: cubeId });
  await engine.call("set-debug-overlays", { bounds: false, sceneAabb: false });
  await engine.settle(300);
  const off = await screenshot("bounds-off");
  await engine.call("set-debug-overlays", { bounds: true });
  await engine.settle(300);
  const on = await screenshot("bounds-on");
  expect(on.equals(off)).toBe(false);
});

test("turning the grid on changes the render", async () => {
  // Look down at the ground plane so the grid is in view (a horizontal eye sees it edge-on).
  await engine.call("set-camera", { position: { x: 0, y: 6, z: 10 }, yaw: 0, pitch: -28 });
  await engine.call("set-debug-overlays", { bounds: false, sceneAabb: false, grid: false });
  await engine.settle(300);
  const off = await screenshot("grid-off");
  await engine.call("set-debug-overlays", { grid: true });
  await engine.settle(300);
  const on = await screenshot("grid-on");
  expect(on.equals(off)).toBe(false);
});

test("colliders round-trips and draws a wireframe over a collider", async () => {
  // The cube already carries a Mesh; give it a Collider so the overlay has a shape to draw.
  await engine.call("add-component", { entity: cubeId, component: "Collider" }).catch(() => {});
  await engine.call("set-debug-overlays", {
    bounds: false,
    sceneAabb: false,
    grid: false,
    colliders: false,
  });
  await engine.settle(300);
  const off = await screenshot("col-off");
  const on1 = await engine.call<DebugOverlays>("set-debug-overlays", { colliders: true });
  expect(on1.colliders).toBe(true);
  const got = await engine.call<DebugOverlays>("get-debug-overlays", {});
  expect(got.colliders).toBe(true);
  await engine.settle(300);
  const on = await screenshot("col-on");
  expect(on.equals(off)).toBe(false);
});

test("the overlay toggles round-trip through project save/load", async () => {
  const projectPath = `${projectDir}/project.json`;
  await engine.call("set-debug-overlays", {
    bounds: true,
    sceneAabb: false,
    lightVolumes: true,
    grid: true,
  });
  await engine.call("save-project", { path: projectPath });

  // Flip every flag, then prove the load restores the saved combination.
  await engine.call("set-debug-overlays", {
    bounds: false,
    sceneAabb: true,
    lightVolumes: false,
    grid: false,
  });
  await engine.loadProject(projectPath);

  const loaded = await engine.call<DebugOverlays>("get-debug-overlays", {});
  expect(loaded.bounds).toBe(true);
  expect(loaded.sceneAabb).toBe(false);
  expect(loaded.lightVolumes).toBe(true);
  expect(loaded.grid).toBe(true);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
