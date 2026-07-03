// Phase 18 (codegen core): a non-foldable node graph (procedural/math nodes) is lowered to a Slang
// evalSurface body, spliced into a self-contained shader, and compiled by slangc. Proves the
// graph -> compilable-shader pipeline (the per-material PSO render path is the larger follow-on).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a procedural graph is detected non-foldable and codegens to a compilable shader", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Codegen" });
  const graph = {
    nodes: [
      { id: "c", type: "constant", props: { value: [0.5, 0.5, 0.5, 1] } },
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
  const set = await engine.call<{ foldable: boolean }>("material-set-graph", { material: m.id, graph });
  expect(set.foldable).toBe(false); // a multiply node can't fold to params — needs codegen

  const compiled = await engine.call<{ ok: boolean }>("material-compile-graph", { material: m.id });
  expect(compiled.ok).toBe(true); // the emitter produced Slang that slangc compiled to SPIR-V
});
