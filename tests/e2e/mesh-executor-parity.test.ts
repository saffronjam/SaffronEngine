// The mesh-shader executor renders what the indexed executor renders.
//
// `VK_EXT_mesh_shader` is a second way to execute the same binned records, not a replacement —
// MoltenVK has no mesh stage and runs the indexed executor at full quality — so the claim is
// equivalence.
//
// Both executors consume the same command stream from `scene_bin_scatter`: the indexed path takes
// those words as draw arguments, the mesh path reads the identical words as data and recovers its
// draw from `SV_DrawIndex` and its triangle block from the group id. Identical cluster cuts are
// therefore structural, and a divergence would mean one executor ignored records the binner emitted.
//
// That leaves the image, measured by rendering the same scene twice in one host with
// `set-mesh-executor` between the captures — one process, one device, one scene, so the executor is
// the only thing that differs. The frames are compared for bit-identity, not closeness: the mesh
// entry calls the same `executorVertexOutput` helper the vertex entry does, over the same records,
// behind the same indexed depth pre-pass — so every shaded fragment resolves from identical inputs.
// The capture turns off everything that accumulates across frames and then waits for the frame to
// stop changing, because a frame still reaching its fixed point differs from itself between two
// readings, and either capture taken there measures how long it settled rather than which executor
// drew it.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import { Cleaner, captureSettledViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";

// The pose both captures render from.
const CAMERA = { position: { x: 0, y: 5, z: 9 }, yaw: 0, pitch: -25 };

const cleaner = new Cleaner();
const frames: Record<string, Buffer> = {};
// Which executor each capture actually used, read back from the engine rather than assumed.
const active: Record<string, boolean> = {};
let meshExecutorSupported = false;
// Which executor a freshly booted host draws through, before anything asks for one.
let bootExecutor = false;

beforeAll(async () => {
  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  cleaner.defer(() => engine.shutdown());

  // The capability is device-dependent: MoltenVK has no mesh stage, and a device whose mesh
  // output limits fall short of one workgroup's shape does not qualify either. There the whole
  // comparison is correctly skipped rather than faked.
  const boot = await engine.call("set-mesh-executor", {});
  meshExecutorSupported = boot.supported;
  bootExecutor = boot.enabled;
  if (!meshExecutorSupported) {
    return;
  }

  await prepareScene(engine, { width: 480, height: 270, camera: CAMERA });
  await engine.call("add-entity", { preset: "plane" });
  const cube = await engine.call("add-entity", { preset: "cube" });
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
  // Nothing that accumulates over frames: the comparison below is exact, and a temporal history
  // or a round-robin probe update makes a capture a function of how long it settled rather than
  // of what drew it. None of them is what this suite measures — the executors differ in how
  // vertices reach the rasterizer, and every one of these passes consumes the resolved frame.
  const aa = await engine.call("set-aa", { mode: "off" });
  expect(aa.aa).toBe("off");
  await engine.call("set-gi", { mode: "off" });
  await engine.call("set-gdf", { enabled: false });

  for (const [name, mesh] of [
    ["indexed", false],
    ["mesh", true],
  ] as const) {
    const selected = await engine.call("set-mesh-executor", { enabled: mesh });
    expect(selected.enabled).toBe(mesh);
    frames[name] = await captureSettledViewport(engine, cleaner, `mesh-exec-${name}`);
    active[name] = (await engine.call("render-stats")).meshExecutor;
  }
  expect(engine.validationErrors()).toEqual([]);
}, 300_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the device's own capability selects the executor, with nothing to opt into", () => {
  // The mesh stage is not an opt-in mode layered over the capability: a host that comes up on a
  // qualifying device is already drawing through it, and one that does not qualify is on the
  // indexed path. `set-mesh-executor` exists to compare the two, not to enable one.
  expect(bootExecutor).toBe(meshExecutorSupported);
});

test("the two captures really used different executors", () => {
  if (!meshExecutorSupported) {
    return;
  }
  // Without this the image comparison below could pass because the switch did nothing and both
  // captures rendered through the indexed path — a parity test that proves nothing at all.
  expect(active.indexed).toBe(false);
  expect(active.mesh).toBe(true);
});

test("the mesh executor renders the indexed executor's image", () => {
  if (!meshExecutorSupported) {
    return;
  }
  const indexed = decodeRgb8Png(frames.indexed!);
  const mesh = decodeRgb8Png(frames.mesh!);
  // Exactly equal, not merely close: a single differing channel would mean one executor
  // shaded a fragment from inputs the other did not.
  expect(meanAbsoluteDifference(indexed, mesh)).toBe(0);
});

test("both executors drew something", () => {
  if (!meshExecutorSupported) {
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

test("a device that does not qualify keeps the indexed executor", async () => {
  if (meshExecutorSupported) {
    return;
  }
  // The other half of the capability gate: asking for the mesh executor on a device short of the
  // feature bits or the output limits must leave the indexed path in force, not fail the call.
  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  cleaner.defer(() => engine.shutdown());
  const asked = await engine.call("set-mesh-executor", { enabled: true });
  expect(asked.enabled).toBe(false);
  expect((await engine.call("render-stats")).meshExecutor).toBe(false);
}, 120_000);
