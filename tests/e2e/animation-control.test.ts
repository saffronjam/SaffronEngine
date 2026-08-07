// Driving animation over the control plane: list-clips / play-animation / get-animation-state /
// seek-animation / set-animation-playing / set-animation-loop. play-animation previews in Edit (no
// Play needed), so the playhead advances through the host's per-frame evaluator and the state
// command reports it. The pose math + GPU path are covered by the animation self-test and the
// playback screenshot test; this file proves the wire.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
let modelId = "";
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  const model = await engine.call("import-model", { path: FIXTURE });
  modelId = model.id;
  const instance = await engine.call("instantiate-model", { asset: model.id });
  meshId = instance.id;
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
});

test("list-clips reports the imported clip", async () => {
  const { clips } = await engine.call("list-clips", { asset: modelId });
  expect(clips.length).toBe(1);
  expect(clips[0].name).toBe("Bend");
  expect(clips[0].duration).toBeCloseTo(1.0, 3);
});

test("play-animation advances the playhead in Edit preview", async () => {
  const started = await engine.call("play-animation", { entity: meshId, clip: "Bend", loop: true });
  expect(started.playing).toBe(true);
  expect(started.clipName).toBe("Bend");
  expect(started.wrap).toBe("loop");

  await engine.settle(300);
  const state = await engine.call("get-animation-state", { entity: meshId });
  expect(state.playing).toBe(true);
  expect(state.time).toBeGreaterThan(0); // advanced without entering Play
});

test("seek-animation sets the playhead, pause freezes it", async () => {
  await engine.call("set-animation-playing", { entity: meshId, playing: false });
  const seeked = await engine.call("seek-animation", { entity: meshId, time: 0.25 });
  expect(seeked.time).toBeCloseTo(0.25, 3);
  expect(seeked.playing).toBe(false);

  await engine.settle(200);
  const state = await engine.call("get-animation-state", { entity: meshId });
  expect(state.playing).toBe(false);
  expect(state.time).toBeCloseTo(0.25, 3); // paused, so the playhead did not move
});

test("set-animation-playing resumes from the paused playhead, not the start", async () => {
  await engine.call("play-animation", { entity: meshId, clip: "Bend", loop: true });
  await engine.settle(200);
  const paused = await engine.call("set-animation-playing", { entity: meshId, playing: false });
  expect(paused.playing).toBe(false);
  expect(paused.time).toBeGreaterThan(0); // advanced before the pause

  // Resuming must continue from the paused time, not reset to 0.
  const resumed = await engine.call("set-animation-playing", { entity: meshId, playing: true });
  expect(resumed.playing).toBe(true);
  expect(resumed.time).toBeCloseTo(paused.time, 3);

  await engine.settle(200);
  const later = await engine.call("get-animation-state", { entity: meshId });
  expect(later.time).toBeGreaterThan(paused.time); // kept advancing from where it resumed
});

test("set-animation-loop changes the wrap mode", async () => {
  const once = await engine.call("set-animation-loop", { entity: meshId, wrap: "once" });
  expect(once.wrap).toBe("once");
});

test("the animationVersion bumps on each mutation", async () => {
  const a = await engine.call("get-animation-state", { entity: meshId });
  await engine.call("seek-animation", { entity: meshId, time: 0.5 });
  const b = await engine.call("get-animation-state", { entity: meshId });
  expect(b.animationVersion).toBeGreaterThan(a.animationVersion);
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
