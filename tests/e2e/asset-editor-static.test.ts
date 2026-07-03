// A static (unskinned) model in the asset editor, which opens every model, not just rigged ones.
// get-asset-model reports hasRig=false with empty bones/clips but a real
// mesh + material count; enter-asset-preview spawns the model (a MeshComponent root, no skeleton) and
// returns a non-zero rootEntity with an empty bone table; exit leaves project.json byte-identical.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const STATIC = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface Capabilities {
  meshCount: number;
  materialCount: number;
  nodeCount: number;
  hasRig: boolean;
  boneCount: number;
  clipCount: number;
}
interface AssetModel {
  mesh: string;
  name: string;
  capabilities: Capabilities;
  bones: unknown[];
  clips: unknown[];
}
interface EnterResult {
  rootEntity: string;
  bones: unknown[];
}

let staticModel = "";
let projectPath = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  staticModel = (await engine.call<{ id: string }>("import-model", { path: STATIC })).id;
  await engine.settle();
  // get-project reports an absolute project.json path under the harness's per-boot appdata temp dir.
  projectPath = (await engine.call<{ path: string }>("get-project")).path;
});
afterAll(async () => {
  await engine?.shutdown();
});

test("get-asset-model on a static model: hasRig=false, empty bones/clips, a real mesh + material count", async () => {
  const model = await engine.call<AssetModel>("get-asset-model", { asset: staticModel });
  expect(model.mesh).toBe(staticModel);
  expect(model.capabilities.hasRig).toBe(false);
  expect(model.capabilities.boneCount).toBe(0);
  expect(model.bones).toEqual([]);
  expect(model.capabilities.clipCount).toBe(0);
  expect(model.clips).toEqual([]);
  expect(model.capabilities.meshCount).toBeGreaterThanOrEqual(1);
  expect(model.capabilities.materialCount).toBeGreaterThanOrEqual(1);
});

test("enter-asset-preview spawns a static model and returns a non-zero root with no bones", async () => {
  await engine.call("exit-asset-preview");
  const res = await engine.call<EnterResult>("enter-asset-preview", { asset: staticModel });
  expect(res.rootEntity).not.toBe("0");
  expect(res.bones).toEqual([]); // static → no skeleton, no bone-entity table
  const ps = await engine.call<{ state: string; previewAsset: string }>("get-play-state");
  expect(ps.state).toBe("edit"); // preview stays in Edit for static models too
  expect(ps.previewAsset).toBe(staticModel);
  await engine.call("exit-asset-preview");
});

test("pick-skeleton-joint finds nothing on a static model (no skeleton)", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("enter-asset-preview", { asset: staticModel });
  await engine.settle(60);
  const hit = await engine.call<{ found: boolean; nodeIndex: number }>("pick-skeleton-joint", {
    u: 0.5,
    v: 0.5,
    radiusPx: 5000,
  });
  expect(hit.found).toBe(false);
  expect(hit.nodeIndex).toBe(-1);
  await engine.call("exit-asset-preview");
});

test("a static-model preview round-trip leaves project.json byte-identical", async () => {
  await engine.call("exit-asset-preview");
  await engine.call("save-project");
  const before = readFileSync(projectPath, "utf8");

  await engine.call("enter-asset-preview", { asset: staticModel });
  await engine.call("exit-asset-preview");

  await engine.call("save-project");
  expect(readFileSync(projectPath, "utf8")).toBe(before);
  expect((await engine.call<{ previewAsset: string }>("get-play-state")).previewAsset).toBe("0");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
