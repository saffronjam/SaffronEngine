// Phase 17: a node graph stored on a .smat folds to the flat material params when every output
// channel is driven by a constant or texture node (no codegen needed). Proves the data model +
// the foldable lowering + the resolve integration: a graph with a constant red base-color folds to
// the same render as a material with red set directly.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a foldable node graph drives the material like direct factors", async () => {
  const a = await engine.call<{ id: string }>("material-create", { name: "GraphA" });
  const b = await engine.call<{ id: string }>("material-create", { name: "DirectB" });

  const graph = {
    nodes: [
      { id: "c", type: "constant", props: { value: [1, 0, 0, 1] } },
      { id: "out", type: "materialOutput" },
    ],
    edges: [{ from: ["c", "rgba"], to: ["out", "baseColor"] }],
  };
  const set = await engine.call<{ id: string; foldable: boolean }>("material-set-graph", {
    material: a.id,
    graph,
  });
  expect(set.foldable).toBe(true);

  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await engine.call<{ png: string }>("preview-render", { material: a.id, size: 128 });
  const pb = await engine.call<{ png: string }>("preview-render", { material: b.id, size: 128 });

  expect(pa.png).toBe(pb.png); // the graph folds to the same red material
  expect(engine.validationErrors()).toEqual([]);
});
