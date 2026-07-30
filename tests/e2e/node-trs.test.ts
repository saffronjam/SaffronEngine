// Node-TRS animation end to end: import BoxAnimated (a >1-node forest with a translate/rotate
// clip on a non-skin node), and prove the importer keeps a LIVE node forest — the child node's
// transform is not baked into vertices, so playing the clip moves the entity — and that
// `list-clip-bindings` resolves the node channel to the spawned entity.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, captureViewport, Cleaner } from "./test-utils.ts";

let engine: Engine;
let rootId = "";
let clipId = "";
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "BoxAnimated.gltf");
const cleaner = new Cleaner();

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `nodetrs-${tag}`);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  const model = await engine.call<{ id: string }>("import-model", { path: FIXTURE });
  const inst = await engine.call<{ id: string }>("instantiate-model", { asset: model.id });
  rootId = inst.id;
  const clips = await engine.call<{ clips: { id: string; name: string }[] }>("list-clips", {
    asset: model.id,
  });
  clipId = clips.clips[0]?.id ?? "";
  await engine.call("focus", { entity: rootId });
  await engine.settle();
});
afterAll(async () => {
  await cleaner.cleanup();
});

test("the import keeps a live node forest (>1 entity, AnimatedBox present)", async () => {
  const { entities } = await engine.call<{ entities: { id: string; name: string }[] }>(
    "list-entities",
  );
  // The forest did not collapse to a single root: the animated child node survives as its own
  // entity with a drivable Transform.
  const box = entities.find((e) => e.name === "AnimatedBox");
  expect(box).toBeDefined();
});

test("list-clip-bindings resolves the node channel against the live forest", async () => {
  const res = await engine.call<{
    channels: { kind: string; label: string; targetName: string }[];
  }>("list-clip-bindings", { entity: rootId, clip: clipId });
  // Two node-TRS channels (translation + rotation) on the AnimatedBox node; both resolve to a
  // node-* kind (not "bone") and a non-empty label.
  expect(res.channels.length).toBeGreaterThanOrEqual(2);
  for (const ch of res.channels) {
    expect(ch.kind.startsWith("node-")).toBe(true);
    expect(ch.label.length).toBeGreaterThan(0);
  }
});

test("playing the node clip moves the entity (transform not baked)", async () => {
  await engine.call("play-animation", { entity: rootId, clip: clipId, loop: true });
  await engine.call("play");
  await engine.settle(300);
  const a = await screenshot("a");
  await engine.settle(400);
  const b = await screenshot("b");
  // The box translates + rotates while the clip plays, so two frames apart differ. A baked
  // transform (no live node) would render the box static.
  expect(b.equals(a)).toBe(false);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
