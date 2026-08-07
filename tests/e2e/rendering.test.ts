// End-to-end rendering tests: drive a real headless engine over the control plane and
// assert on its responses + the engine's own validation output. Run with `bun test`
// (or `make e2e`), inside the saffron-build toolbox.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  // import-model needs a loaded project; SAFFRON_SCRATCH_PROJECT makes one (under the harness temp appdata dir).
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("boots clean: ping answers and no validation errors at startup", async () => {
  const pong = await engine.call("ping");
  expect(pong.pong).toBe(true);
  expect(engine.validationErrors()).toEqual([]);
});

// Async-compute scheduling: the Global-SDF build declares the independent compute queue, and the
// graph places it there whenever the device exposes a compute family it can derive the ownership
// transfer for. `asyncComputeBatches` counts the command buffers the frame plan submitted on that
// lane, so a production pass that stops asking for it — or a graph that stops honouring the
// request — shows up here as zero.
test("runs the distance-field build on the independent compute queue", async () => {
  const stats = await engine.call("render-stats");
  if (!stats.asyncComputeQueue) {
    console.log("skip: this device exposes no independent compute queue family");
    expect(stats.asyncComputeBatches).toBe(0);
    return;
  }
  expect(stats.gdf).toBe(true);
  expect(stats.asyncComputeBatches).toBeGreaterThan(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("imports a model and reports a draw", async () => {
  await engine.call("add-entity", { preset: "cube" });
  const entities = await engine.call("list-entities");
  expect(Array.isArray(entities.entities)).toBe(true);
  const stats = await engine.call("render-stats");
  expect(stats.drawCalls).toBeGreaterThan(0);
});

// setAa clamps the MSAA sample count to the R16G16B16A16_SFLOAT / D32_SFLOAT target formats'
// actual support, not the generic framebuffer limit, so the color/depth images never request a
// count those formats reject on some GPUs (VUID-VkImageCreateInfo-samples-02258).
test("set-aa across every level stays Vulkan-validation-clean", async () => {
  for (const mode of ["msaa2", "msaa4", "msaa8", "msaa2", "off"]) {
    const result = await engine.call("set-aa", { args: [mode] });
    expect(typeof result.aa).toBe("string");
    await engine.settle(200);
  }
  expect(engine.validationErrors()).toEqual([]);
});

// Depth-aware camera frustum overlay: the editor draws each CameraComponent's frustum into the
// post-tonemap overlay pass, depth-tested (read-only) against the scene depth so the lines are
// occluded by geometry instead of painting over it. Binding the scene depth as a read-only
// attachment on the overlay pass — and, under MSAA, resolving the multisampled scene depth into
// the 1x target the overlay reads — are extra GPU paths. Assert they stay Vulkan-validation-clean
// in every AA mode (the suite's oracle for sample-count/resolve bugs).
describe("depth-tested camera frustum overlay", () => {
  beforeAll(async () => {
    // A scene camera (showFrustum defaults true, so the overlay draws its frustum) at the origin
    // facing -Z, plus a cube parked inside the frustum so the depth-tested lines actually get
    // occluded.
    const camera = await engine.call("add-entity", { args: ["camera"] });
    await engine.call("set-transform", { entity: camera.id, translation: { x: 0, y: 0, z: 0 } });
    const cube = await engine.call("add-entity", { args: ["cube"] });
    await engine.call("set-transform", { entity: cube.id, translation: { x: 0, y: 0, z: -4 } });
    // View the frustum head-on from +Z so its edges project on-screen and the depth-tested draw
    // actually runs (an off-screen frustum would clip to zero vertices).
    await engine.call("set-camera", { position: { x: 0, y: 1, z: 9 }, yaw: 0, pitch: -5, fov: 60 });
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
});
