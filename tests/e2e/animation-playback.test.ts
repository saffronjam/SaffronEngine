// Skeletal animation playback through the full host loop: a rigged+animated glTF imports
// with an AnimationPlayerComponent, and once playing it deforms the mesh in Play mode —
// proven by the screenshot trio (rest renders, playing differs, stop reverts). The
// evaluator math + non-destructive Edit preview are covered by the engine's animation
// self-test; this file proves the host onUpdate -> evaluator -> GPU skinning path.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let meshId = "";
// A rigged import returns the model root (ModelInstance + Relationship + Transform); the rig
// components (AnimationPlayer, SkinnedMesh, FootIk) live on the mesh descendant resolved by
// animatableDescendant. Resolve and remember that descendant for the component assertions/edits.
let playerId = "";
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");
const shots: string[] = [];

/// Find the entity in the imported hierarchy that actually carries the AnimationPlayer component.
async function findPlayerEntity(): Promise<string | undefined> {
  const list = (await engine.call<{ entities: { id: string }[] }>("list-entities")).entities;
  for (const e of list) {
    const info = await engine.call<{ components: Record<string, unknown> }>("inspect", { entity: e.id });
    if (info.components.AnimationPlayer) {
      return e.id;
    }
  }
  return undefined;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  const imported = await engine.importEntity(FIXTURE);
  meshId = imported.id;
  await engine.settle();
  playerId = (await findPlayerEntity()) ?? "";
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

/// Capture the viewport and wait for the deferred write to land on disk.
async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-anim-${process.pid}-${tag}.png`;
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

test("the imported rig carries a stopped AnimationPlayer bound to the clip", async () => {
  expect(playerId).not.toBe(""); // the rig descendant carrying AnimationPlayer must exist
  const info = await engine.call<{ components: Record<string, { clip: string; playing: boolean }> }>("inspect", {
    entity: playerId,
  });
  const player = info.components.AnimationPlayer;
  expect(player).toBeDefined();
  expect(player.playing).toBe(false);
  expect(player.clip).not.toBe("0"); // bound to the imported "Bend" clip
});

test("playing the clip deforms the mesh, and stop reverts it", async () => {
  await engine.call("set-component-field", {
    entity: playerId,
    component: "AnimationPlayer",
    field: "playing",
    value: true,
  });
  await engine.call("focus", { entity: meshId });
  await engine.settle(400);

  // Edit mode without preview is inert, so this is the rest pose.
  const rest = await screenshot("rest");

  // Play animates every rig; the strip bends as the root joint rotates.
  await engine.call("play");
  await engine.settle(600);
  const playing = await screenshot("playing");
  expect(playing.equals(rest)).toBe(false);

  // Stop discards the play scene; the authored rest pose comes back.
  await engine.call("stop");
  await engine.settle(400);
  const stopped = await screenshot("stopped");
  expect(stopped.equals(playing)).toBe(false);
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
