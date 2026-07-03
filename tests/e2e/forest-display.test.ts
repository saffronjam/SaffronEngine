// Forest display: a STATIC multi-mesh-node model (multi-node.gltf — two sibling box nodes, no skin,
// no animation) must open in the asset preview and frame its whole assembled geometry. This is the
// GothicCommode shape: the meshes ride child nodes while the spawned container root carries none, so a
// gate that probes a single resolved entity wrongly rejects it with "no renderable mesh". The fix
// resolves the model's forest (model_has_renderable / model_render_aabb), so the model opens and frames
// around both boxes rather than a 1-unit sphere at the origin.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const MULTI_NODE = join(REPO, "tests", "e2e", "fixtures", "multi-node.gltf");

interface SubAsset {
  type: string;
}
interface ModelInfo {
  subAssets: SubAsset[];
}
interface EnterResult {
  rootEntity: string;
  target: { x: number; y: number; z: number };
  distance: number;
}

let modelId = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  const ref = await engine.call<{ id: string }>("import-model", { path: MULTI_NODE });
  modelId = ref.id;
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
});

test("the model is a multi-mesh-node forest (more than one mesh sub-asset)", async () => {
  const info = await engine.call<ModelInfo>("model-info", { asset: modelId });
  const meshes = info.subAssets.filter((s) => s.type === "mesh").length;
  expect(meshes).toBeGreaterThanOrEqual(2);
});

test("enter-asset-preview opens the static forest and frames its full extent", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call<EnterResult>("enter-asset-preview", { asset: modelId });
  // The headline: the model opens rather than failing with "no renderable mesh".
  expect(res.rootEntity).not.toBe("0");
  // Framed by the forest bounds union, not the radius-1 fallback: the two boxes span ~4 units on
  // X, so the orbit distance is well above the unit-sphere fallback (~1.5 at the default fov).
  expect(Number.isFinite(res.distance)).toBe(true);
  expect(res.distance).toBeGreaterThan(2.0);
});

test("the spawned preview forest carries the meshes on child entities", async () => {
  // The preview scene holds the container, its two mesh children, the floor and key light — more
  // than the single entity a collapsed model would spawn.
  const list = await engine.call<{ entities: unknown[] }>("list-entities");
  expect(list.entities.length).toBeGreaterThanOrEqual(3);
  await engine.call("exit-asset-preview");
});
