// Depth-aware camera frustum overlay: the editor draws each CameraComponent's frustum into
// the post-tonemap overlay pass, now depth-tested (read-only) against the scene depth so the
// lines are occluded by geometry instead of painting over it. Binding the scene depth as a
// read-only attachment on the overlay pass — and, under MSAA, resolving the multisampled scene
// depth into the 1x target the overlay reads — are new GPU paths. Assert they stay
// Vulkan-validation-clean in every AA mode (the suite's oracle for sample-count/resolve bugs).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

interface Ref {
  id: string;
  name: string;
}

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // A scene camera (showFrustum defaults true, so the overlay draws its frustum) at the origin
  // facing -Z, plus a cube parked inside the frustum so the depth-tested lines actually get
  // occluded.
  const camera = await engine.call<Ref>("add-entity", { args: ["camera"] });
  await engine.call("set-transform", { entity: camera.id, translation: { x: 0, y: 0, z: 0 } });
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 0, y: 0, z: -4 } });
  // View the frustum head-on from +Z so its edges project on-screen and the depth-tested draw
  // actually runs (an off-screen frustum would clip to zero vertices).
  await engine.call("set-camera", { position: { x: 0, y: 1, z: 9 }, yaw: 0, pitch: -5, fov: 60 });
});
afterAll(async () => {
  await engine?.shutdown();
});

// Every AA mode: the MSAA ones (msaa2/4/8) drive the depth-resolve path, off/fxaa/taa the
// direct 1x-depth-store path.
const AA_MODES = ["off", "fxaa", "taa", "msaa2", "msaa4", "msaa8"];

for (const mode of AA_MODES) {
  test(`depth-tested frustum overlay stays validation-clean (aa=${mode})`, async () => {
    await engine.call("set-aa", { args: [mode] });
    await engine.settle(300);
    expect(engine.validationErrors()).toEqual([]);
  });
}
