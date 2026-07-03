// Fly-camera look over the control plane: streamed fly-input deltas drain through a
// per-frame exponential filter (lookPending, tau 25ms), so the full delta must land on
// yaw/pitch once the filter settles — smoothing reshapes the motion, it never loses input.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { Engine } from "./harness.ts";
import type { EditorCamera } from "@saffron/protocol";

const projectDir = `/tmp/saffron-e2e-camera-${process.pid}`;

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot();
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

interface Ref {
  id: string;
  name: string;
}
interface Inspect {
  id: string;
  name: string;
  components: Record<string, any>;
}

test("fly-input look deltas converge to delta * lookSpeed", async () => {
  const before = await engine.call<EditorCamera>("get-camera");

  await engine.call("fly-input", { active: true, lookDx: 100, lookDy: -50 });
  // tau is 25ms, so a few hundred ms of frames fully drains the pending delta.
  await engine.settle(400);
  const after = await engine.call<EditorCamera>("get-camera");

  expect(after.yaw).toBeCloseTo(before.yaw + 100 * before.lookSpeed, 3);
  expect(after.pitch).toBeCloseTo(before.pitch + 50 * before.lookSpeed, 3);

  await engine.call("fly-input", { active: false });
  expect(engine.validationErrors()).toEqual([]);
});

test("the editor camera roundtrips through project save/load", async () => {
  const saved = await engine.call<EditorCamera>("set-camera", {
    position: { x: 1.5, y: 2.5, z: -3.5 },
    yaw: 42,
    pitch: -13,
    fov: 60,
  });
  await engine.call("save-project", { path: `${projectDir}/project.json` });

  await engine.call("set-camera", { position: { x: 0, y: 0, z: 0 }, yaw: 0, pitch: 0, fov: 45 });
  await engine.loadProject(`${projectDir}/project.json`);

  const loaded = await engine.call<EditorCamera>("get-camera");
  expect(loaded.position).toEqual(saved.position);
  expect(loaded.yaw).toBeCloseTo(saved.yaw, 4);
  expect(loaded.pitch).toBeCloseTo(saved.pitch, 4);
  expect(loaded.fov).toBeCloseTo(saved.fov, 4);
  expect(engine.validationErrors()).toEqual([]);
});

test("scene camera editor helpers default on and roundtrip", async () => {
  const camera = await engine.call<Ref>("add-entity", { args: ["camera"] });
  const created = await engine.call<Inspect>("inspect", { entity: camera.id });
  expect(created.components.Camera.showModel).toBe(true);
  expect(created.components.Camera.showFrustum).toBe(true);
  expect(created.components.Camera.frustumMaxDistance).toBeCloseTo(10, 4);

  await engine.call("set-component-field", {
    entity: camera.id,
    component: "Camera",
    field: "showModel",
    value: false,
  });
  await engine.call("set-component-field", {
    entity: camera.id,
    component: "Camera",
    field: "showFrustum",
    value: false,
  });
  await engine.call("set-component-field", {
    entity: camera.id,
    component: "Camera",
    field: "frustumMaxDistance",
    value: 8,
  });
  await engine.call("save-project", { path: `${projectDir}/project.json` });

  await engine.loadProject(`${projectDir}/project.json`);
  const loaded = await engine.call<Inspect>("inspect", { entity: camera.id });
  expect(loaded.components.Camera.showModel).toBe(false);
  expect(loaded.components.Camera.showFrustum).toBe(false);
  expect(loaded.components.Camera.frustumMaxDistance).toBeCloseTo(8, 4);
  expect(engine.validationErrors()).toEqual([]);
});
