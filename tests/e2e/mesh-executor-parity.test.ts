// The mesh-shader executor renders what the indexed executor renders.
//
// `VK_EXT_mesh_shader` gives a second way to execute the same binned records. It is a second
// path by design, not a replacement — MoltenVK has no mesh stage and runs the indexed executor
// at full quality — so the claim to prove is equivalence, not improvement.
//
// WHAT MAKES THE CUT SHARED RATHER THAN COMPARED: both executors consume the same command
// stream from `scene_bin_scatter`. The indexed path takes those words as draw arguments; the
// mesh path reads the identical words as data, recovering its draw from `SV_DrawIndex` and its
// triangle block from the group id. So "identical semantic cluster cuts" is structural — a
// divergence would mean one executor ignored records the binner emitted, which is a different
// bug from the two disagreeing about geometry.
//
// That leaves the image as the thing worth measuring, and it is measured by booting two hosts
// that differ in exactly one environment variable. Everything else — scene, camera, lighting,
// wind — is identical by construction.
//
// The frames should be bit-identical: the mesh entry calls the *same* `executorVertexOutput`
// helper the vertex entry does, so the arithmetic is not merely equivalent but literally the
// same code. The tolerance below exists for rasterization order, not for a shading difference.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";
import { Cleaner, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";

/// The pose both hosts render from.
const CAMERA = { position: { x: 0, y: 5, z: 9 }, yaw: 0, pitch: -25 };

/// Mean absolute per-channel difference (0-255) the two executors may differ by. Both run the
/// same vertex arithmetic over the same records, so this covers rasterization order alone.
const PARITY_TOLERANCE = 1.0;

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};
/// Which executor each run actually used, read back from the engine rather than assumed.
const active: Record<string, boolean> = {};
let meshShaderSupported = false;

/// Boots a host with the executor selected, builds the scene, and captures one settled frame.
async function captureWithExecutor(meshExecutor: boolean): Promise<Buffer> {
  const engine = await Engine.boot({
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_MESH_EXECUTOR: meshExecutor ? "1" : "0",
  });
  cleaner.defer(() => engine.shutdown());
  await prepareScene(engine, { width: 480, height: 270, camera: CAMERA });
  await engine.call("add-entity", { preset: "plane" });
  const cube = await engine.call<{ id: string }>("add-entity", { preset: "cube" });
  await engine.call("set-component", {
    entity: cube.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 1, z: 0 },
      scale: { x: 1.5, y: 1.5, z: 1.5 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  await engine.call("set-wind", { speed: 0, gust: 0 });
  await engine.settle(1200);
  const frame = await captureViewport(engine, cleaner, `mesh-exec-${meshExecutor}`);
  active[meshExecutor ? "mesh" : "indexed"] = (
    await engine.call<RenderStatsDto>("render-stats")
  ).meshExecutor;
  expect(engine.validationErrors()).toEqual([]);
  return frame;
}

beforeAll(async () => {
  // The capability is device-dependent: MoltenVK has no mesh stage, so there the whole
  // comparison is correctly skipped rather than faked.
  const probe = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  meshShaderSupported = (await probe.call<RenderStatsDto>("render-stats")).meshShader;
  await probe.shutdown();
  if (!meshShaderSupported) {
    return;
  }
  frames.indexed = await captureWithExecutor(false);
  frames.mesh = await captureWithExecutor(true);
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the two runs really used different executors", () => {
  if (!meshShaderSupported) {
    return;
  }
  // Without this the image comparison below could pass because the toggle did nothing and both
  // runs rendered through the indexed path — a parity test that proves nothing at all.
  expect(active.indexed).toBe(false);
  expect(active.mesh).toBe(true);
});

test("the mesh executor renders the indexed executor's image", () => {
  if (!meshShaderSupported) {
    return;
  }
  const indexed = decodeRgb8Png(frames.indexed!);
  const mesh = decodeRgb8Png(frames.mesh!);
  expect(meanAbsoluteDifference(indexed, mesh)).toBeLessThan(PARITY_TOLERANCE);
});

test("both executors drew something", () => {
  if (!meshShaderSupported) {
    return;
  }
  // Two blank frames would satisfy the comparison above perfectly. This is the control:
  // the captures must contain actual geometry, which for this scene means a cube against a
  // sky — a spread of values, not one flat colour.
  const mesh = decodeRgb8Png(frames.mesh!);
  const first = mesh.pixels[0]!;
  const differs = mesh.pixels.some((value) => Math.abs(value - first) > 8);
  expect(differs).toBe(true);
});
