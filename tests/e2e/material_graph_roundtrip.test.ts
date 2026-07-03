// Phase 20 plumbing: material-get returns the stored (unfolded) node graph, so the editor can load a
// material's graph into the canvas to edit it. Round-trips a graph through set-graph -> get.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("material-get returns the graph that set-graph stored", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "RoundTrip" });
  const graph = {
    nodes: [
      { id: "c", type: "constant", props: { value: [0.3, 0.6, 0.9, 1] } },
      { id: "t", type: "textureSlot", props: { slot: "albedo" } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c", "rgba"], to: ["mul", "a"] },
      { from: ["t", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });

  const got = await engine.call<{ graph: typeof graph }>("material-get", { material: m.id });
  expect(got.graph).toBeDefined();
  expect(got.graph.nodes).toHaveLength(4);
  expect(got.graph.edges).toHaveLength(3);
  expect(got.graph.nodes.find((n) => n.id === "mul")?.type).toBe("multiply");
});

test("material-get returns an empty graph object for a material with no graph", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "NoGraph" });
  const got = await engine.call<{ graph: { nodes?: unknown[] } }>("material-get", { material: m.id });
  expect(got.graph).toBeDefined();
  expect(got.graph.nodes ?? []).toHaveLength(0);
});
