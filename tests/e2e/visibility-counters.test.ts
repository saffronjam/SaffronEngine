// The GPU visibility counters report the frame's real work, not plausible-looking numbers.
//
// A telemetry word wired to the wire but never incremented reads as a healthy zero, and a zero is
// indistinguishable from "this frame did no such work". So each counter is asserted against a scene
// where its true value is known independently of the counter itself.
//
// `bins` counts executor buckets that received a record, which is the number of indirect draws the
// frame issues, counted on the pass that already touches every record rather than by scanning the
// bucket table afterwards. `deformed` counts the instances a view composed deformed bounds for,
// which is deformation work that reached a view rather than slots that merely carry the flag.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { GpuSceneMirrorStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, prepareScene } from "./test-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;
// Counters with an empty scene, and after content arrives.
let empty: GpuSceneMirrorStatsDto;
let populated: GpuSceneMirrorStatsDto;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    width: 320,
    height: 180,
    camera: { position: { x: 0, y: 2, z: 6 }, yaw: 0, pitch: -15 },
  });
  await engine.settle(900);
  empty = await engine.call("gpu-scene-stats");

  await engine.call("add-entity", { preset: "plane" });
  for (const x of [-2, 0, 2]) {
    const cube = await engine.call("add-entity", { preset: "cube" });
    await engine.call("set-component", {
      entity: cube.id,
      component: "Transform",
      json: {
        translation: { x, y: 1, z: 0 },
        scale: { x: 1, y: 1, z: 1 },
        rotation: { x: 0, y: 0, z: 0 },
      },
    });
  }
  await engine.settle(1200);
  populated = await engine.call("gpu-scene-stats");
}, 180_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("the bin count rises with content and never exceeds the records it bins", () => {
  // A bin holds at least one record by definition, so more bins than records would mean the
  // counter is counting something other than what its name says.
  expect(populated.visibility.bins).toBeGreaterThan(empty.visibility.bins);
  expect(populated.visibility.bins).toBeLessThanOrEqual(populated.visibility.records);
  expect(populated.visibility.bins).toBeGreaterThan(0);
});

test("the counters agree with each other", () => {
  // Independent words derived from the same walk. A counter incremented in the wrong branch
  // usually shows up here rather than as an implausible value on its own.
  expect(populated.visibility.culledNodes).toBeLessThanOrEqual(populated.visibility.visitedNodes);
  expect(populated.visibility.visible).toBeGreaterThan(empty.visibility.visible);
  expect(populated.visibility.records).toBeGreaterThan(empty.visibility.records);
});

test("the deformed count reflects wind-flagged instances only", () => {
  // Nothing in this scene is wind-flagged — the mirror sets that flag on vegetation points alone,
  // so a cube in a gale is motionless by design. A nonzero count here would mean the counter is
  // incrementing on every instance rather than on the deformed ones, which is exactly the kind of
  // wrong that a "greater than zero" assertion would have called healthy.
  expect(populated.visibility.deformed).toBe(0);
});

test("the reach view keeps what a gather can read, not what the camera can see", async () => {
  // Global illumination is not a camera. The reach view culls against the window a march or a
  // reflection ray can reach — which contains the camera's frustum and a great deal behind it —
  // so an instance the camera cannot see must still survive it. This scene is small and sits
  // entirely inside that window, so the whole of it is reachable and nothing is rejected.
  expect(populated.visibility.giReachVisible).toBeGreaterThanOrEqual(populated.visibility.visible);
  expect(populated.visibility.giReachCulled).toBe(0);
  expect(populated.visibility.giReachVisible).toBeGreaterThan(empty.visibility.giReachVisible);

  // Walking the camera far outside the field's coarsest cascade leaves the scene behind: it is
  // then unreachable, and the reach cull is what says so. Without the reach pass the counter
  // cannot move at all, and without the box test everything would stay visible forever.
  await engine.call("set-camera", { position: { x: 100_000, y: 2, z: 100_000 }, yaw: 0, pitch: 0 });
  await engine.settle(900);
  const away = await engine.call("gpu-scene-stats");
  expect(away.visibility.giReachCulled).toBeGreaterThan(0);
  expect(away.visibility.giReachVisible).toBe(0);

  await engine.call("set-camera", { position: { x: 0, y: 2, z: 6 }, yaw: 0, pitch: -15 });
  await engine.settle(900);
  const back = await engine.call("gpu-scene-stats");
  expect(back.visibility.giReachVisible).toBeGreaterThan(0);
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);

test("no view class raises a missing-page request the buffer cannot hold", async () => {
  // Every view in the frame appends missing-page requests, and each class owns its own
  // region — so a drop is caused by that class's own volume and by nothing else. An ordinary
  // scene is nowhere near any region's ceiling; this is the guard for the day something
  // starts flooding one, which is otherwise a page arriving a frame late over and over with
  // nothing saying so.
  //
  // That the counter CAN fire is proven in `a_flooded_class_loses_only_its_own_requests`
  // rather than here: filling a 4096-entry region takes more page faults in one frame than
  // any scene a test can build, and an e2e assertion that only holds when the flood happens
  // to occur is not a proof of anything.
  expect(populated.pageResidency.requestsDropped).toBe(0);
  expect(populated.pageResidency.requestOverflowClasses).toBe(0);

  // The budget is what makes the region reachable at all, so it has to be drivable and it
  // has to refuse to exceed what is allocated.
  const lowered = await engine.call("page-request-budget", { entries: 1 });
  expect(lowered.entries).toBe(1);
  expect(lowered.capacity).toBeGreaterThan(1);
  const restored = await engine.call("page-request-budget", { entries: lowered.capacity * 4 });
  expect(restored.entries).toBe(restored.capacity);
  expect(engine.validationErrors()).toEqual([]);
}, 60_000);

test("no capacity flag is raised for an ordinary scene", () => {
  // A silent clamp is how geometry disappears; the flags exist so it is never silent.
  expect(populated.visibility.overflowFlags).toBe(0);
  expect(populated.visibility.pressureFlags).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("covered samples count real lanes only while something is measuring", async () => {
  // Quad utilization has no pipeline statistic of its own. It is covered samples over fragment
  // invocations — the atomic discards helper lanes, the statistic counts them — so the pair only
  // means anything if the counter is genuinely zero when nothing is measuring and genuinely
  // nonzero when something is. Both halves are asserted, because a counter that is always zero
  // reads as healthy and a counter that always fires costs an atomic per fragment forever.
  const idle = await engine.call("gpu-scene-stats");
  expect(idle.visibility.coveredSamples).toBe(0);

  await engine.call("profiler.set-mode", { mode: "timestamps" });
  await engine.settle(400);
  const armed = await engine.call("gpu-scene-stats");
  expect(armed.visibility.coveredSamples).toBeGreaterThan(0);

  await engine.call("profiler.set-mode", { mode: "off" });
  await engine.settle(400);
  const stopped = await engine.call("gpu-scene-stats");
  expect(stopped.visibility.coveredSamples).toBe(0);
});
