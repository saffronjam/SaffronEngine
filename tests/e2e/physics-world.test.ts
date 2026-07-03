// Phase 1 of the physics plan: the per-play-session Jolt world. A PhysicsWorld is built on the
// Edit->Playing edge and discarded on ->Edit, mirroring the script VM. physics-state reports
// whether a world is live; it is inactive in Edit, active while playing, and inactive again after
// stop. No bodies exist yet (Phase 2 adds them), so the counts stay zero.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

interface PhysicsState {
  active: boolean;
  bodyCount: number;
  dynamicCount: number;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("no physics world exists in Edit", async () => {
  const state = await engine.call<PhysicsState>("physics-state");
  expect(state.active).toBe(false);
  expect(state.bodyCount).toBe(0);
  expect(state.dynamicCount).toBe(0);
});

test("entering play allocates a Jolt world; stopping frees it", async () => {
  await engine.call("play");
  await engine.settle();
  const playing = await engine.call<PhysicsState>("physics-state");
  expect(playing.active).toBe(true);
  expect(playing.bodyCount).toBe(0); // no components author bodies yet (Phase 2)
  expect(playing.dynamicCount).toBe(0);

  await engine.call("stop");
  await engine.settle();
  const stopped = await engine.call<PhysicsState>("physics-state");
  expect(stopped.active).toBe(false);
});

test("a second play/stop cycle re-allocates a fresh world (no leak across the edge)", async () => {
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(true);
  await engine.call("stop");
  await engine.settle();
  expect((await engine.call<PhysicsState>("physics-state")).active).toBe(false);
});

test("the world lifecycle is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
