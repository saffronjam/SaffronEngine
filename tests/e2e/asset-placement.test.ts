// The asset drag-in placement flow: `asset-placement` previews a model as a PreviewGhost-tagged
// subtree in the authored scene (rendered, but hidden from the outliner and never persisted),
// moved by a transform write per drag-over. `commit` untags it into a real entity; `clear`
// destroys it. This is the engine side of the editor's drag-from-asset-browser gesture.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetPlacementResult, EntityRef } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

interface EntityList {
  entities: { id: string; name: string }[];
}

async function entityIds(): Promise<Set<string>> {
  const list = await engine.call<EntityList>("list-entities");
  return new Set(list.entities.map((e) => e.id));
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

let modelId = "";

test("import the placement fixture", async () => {
  const ref = await engine.call<{ id: string; type: string }>("import-model", { path: FIXTURE });
  await engine.settle();
  expect(ref.type).toBe("model");
  modelId = ref.id;
});

test("a preview ghost renders but is invisible to the outliner", async () => {
  const before = await entityIds();

  const r = await engine.call<AssetPlacementResult>("asset-placement", {
    phase: "preview",
    asset: modelId,
    u: 0.5,
    v: 0.5,
  });
  expect(r.active).toBe(true);
  expect(r.valid).toBe(true);

  // The ghost lives in the scene (it renders), but list-entities never reports it.
  const during = await entityIds();
  expect(during).toEqual(before);
});

test("commit turns the ghost into exactly one new outliner entity", async () => {
  const before = await entityIds();

  // A drag-over update, then the drop.
  await engine.call<AssetPlacementResult>("asset-placement", { phase: "preview", asset: modelId, u: 0.4, v: 0.6 });
  const committed = await engine.call<AssetPlacementResult>("asset-placement", { phase: "commit" });
  await engine.settle();
  expect(committed.entity).toBeDefined();

  const after = await entityIds();
  // The committed root is now a real, listed entity.
  expect(after.has(committed.entity!.id)).toBe(true);
  // Exactly one new root appeared in the outliner.
  const added = [...after].filter((id) => !before.has(id));
  expect(added).toEqual([committed.entity!.id]);
});

test("clear destroys the ghost and leaves the committed entity untouched", async () => {
  const before = await entityIds();

  await engine.call<AssetPlacementResult>("asset-placement", { phase: "preview", asset: modelId, u: 0.6, v: 0.4 });
  const cleared = await engine.call<AssetPlacementResult>("asset-placement", { phase: "clear" });
  await engine.settle();
  expect(cleared.active).toBe(false);

  const after = await entityIds();
  expect(after).toEqual(before);
});

test("a preview never persists across save and reload", async () => {
  // An active ghost at save time must not leak into the project file.
  await engine.call<AssetPlacementResult>("asset-placement", { phase: "preview", asset: modelId, u: 0.5, v: 0.5 });
  const before = await entityIds();

  const path = `/tmp/saffron-e2e-placement-${process.pid}.json`;
  await engine.call("save-scene", { path });
  await engine.call("asset-placement", { phase: "clear" });
  await engine.call("load-scene", { path });
  await engine.settle();

  const after = await entityIds();
  expect(after).toEqual(before);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
