// Debug overlays: the viewport debug-visualization toggles (bounds / scene AABB / light volumes)
// drawn as world-space lines in the editor overlay pass. This drives the set/get-debug-overlays
// control toggle (round-trip + partial update), proves the bounds overlay actually renders (adding
// a mesh and turning bounds on changes pixels), and asserts the run stays validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { Engine } from "./harness.ts";

let engine: Engine;
let cubeId = "";
const shots: string[] = [];
const projectDir = `/tmp/saffron-e2e-dbgov-project-${process.pid}`;

interface Ref {
  id: string;
  name: string;
}

interface DebugOverlays {
  bounds: boolean;
  sceneAabb: boolean;
  lightVolumes: boolean;
  grid: boolean;
  colliders: boolean;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  cubeId = cube.id;
  await engine.call("focus", { entity: cube.id });
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
  rmSync(projectDir, { recursive: true, force: true });
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-dbgov-${process.pid}-${tag}.png`;
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

test("the overlays are off by default", async () => {
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
  await engine.call("set-debug-overlays", { bounds: false, sceneAabb: false });
  await engine.settle(300);
  const off = await screenshot("off");
  await engine.call("set-debug-overlays", { bounds: true });
  await engine.settle(300);
  const on = await screenshot("on");
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
  await engine.call("load-project", { path: projectPath });

  const loaded = await engine.call<DebugOverlays>("get-debug-overlays", {});
  expect(loaded.bounds).toBe(true);
  expect(loaded.sceneAabb).toBe(false);
  expect(loaded.lightVolumes).toBe(true);
  expect(loaded.grid).toBe(true);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
