// Phase 19 (procedural nodes, now visible): the uv/frac/sin/... nodes codegen into the preview path,
// so a procedural graph renders a pattern on the sphere. Validates a frac(uv * 8) graph renders.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a procedural uv/frac graph codegen-renders in the preview", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Procedural" });
  const graph = {
    nodes: [
      { id: "uv", type: "uv" },
      { id: "s", type: "constant", props: { value: [8, 8, 8, 1] } },
      { id: "mul", type: "multiply" },
      { id: "f", type: "frac" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["uv", "out"], to: ["mul", "a"] },
      { from: ["s", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["f", "a"] },
      { from: ["f", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });

  const prev = await engine.call<{ png: string }>("preview-render", { material: m.id, size: 128 });
  expect(prev.png.startsWith("iVBORw0KGgo")).toBe(true);
  expect(prev.png.length).toBeGreaterThan(200);
  expect(engine.validationErrors()).toEqual([]);
});
