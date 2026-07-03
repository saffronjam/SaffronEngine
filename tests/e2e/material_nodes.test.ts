// Phase 19 (node library): the codegen emitter supports the standard math/utility nodes
// (lerp, oneMinus, saturate, subtract, divide, dot, ...). A graph wiring several of them must
// codegen to compilable Slang.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a graph of math/utility nodes codegens to a compilable shader", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Nodes" });
  const graph = {
    nodes: [
      { id: "c1", type: "constant", props: { value: [0.8, 0.2, 0.1, 1] } },
      { id: "c2", type: "constant", props: { value: [0.1, 0.3, 0.9, 1] } },
      { id: "ct", type: "constant", props: { value: [0.5, 0, 0, 0] } },
      { id: "lerp", type: "lerp" },
      { id: "om", type: "oneMinus" },
      { id: "sat", type: "saturate" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["lerp", "a"] },
      { from: ["c2", "rgba"], to: ["lerp", "b"] },
      { from: ["ct", "rgba"], to: ["lerp", "t"] },
      { from: ["lerp", "rgba"], to: ["om", "a"] },
      { from: ["om", "rgba"], to: ["sat", "a"] },
      { from: ["sat", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  await engine.call("material-set-graph", { material: m.id, graph });
  const compiled = await engine.call<{ ok: boolean }>("material-compile-graph", { material: m.id });
  expect(compiled.ok).toBe(true);
});
