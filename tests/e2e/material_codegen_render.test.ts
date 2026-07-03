// Phase 18 (render-wiring): a non-foldable node graph renders in the preview via a codegen'd shader.
// preview-render detects the procedural graph, emits + slangc-compiles a preview shader whose
// evalSurface is the graph, builds a per-graph pipeline, and renders the sphere with it. The end of
// the headline pipeline: graph -> Slang -> slangc -> PSO -> a rendered, validation-clean image.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a procedural graph renders via codegen in the preview", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "CodegenPrev" });
  const graph = {
    nodes: [
      { id: "c1", type: "constant", props: { value: [1, 0, 0, 1] } },
      { id: "c2", type: "constant", props: { value: [0.5, 0.5, 0.5, 1] } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["mul", "a"] },
      { from: ["c2", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });

  const prev = await engine.call<{ png: string }>("preview-render", { material: m.id, size: 128 });
  expect(prev.png.startsWith("iVBORw0KGgo")).toBe(true); // valid PNG from the codegen'd pipeline
  expect(prev.png.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});
