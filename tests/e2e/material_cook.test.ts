// Phase 21 (cook step): material-cook bakes every codegen material's übershader variant to disk (the
// shipping/precompile direction), reusing the scene-path compile. Foldable + graphless materials are
// skipped. Validates that two non-foldable materials are compiled and their variants land on disk.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

function proceduralGraph() {
  return {
    nodes: [
      { id: "c1", type: "constant", props: { value: [0.4, 0.7, 0.2, 1] } },
      { id: "c2", type: "constant", props: { value: [0.9, 0.9, 0.9, 1] } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["mul", "a"] },
      { from: ["c2", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
}

test("material-cook compiles every codegen material's übershader variant", async () => {
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);

  const a = await engine.call<{ id: string }>("material-create", { name: "CookA" });
  const b = await engine.call<{ id: string }>("material-create", { name: "CookB" });
  await engine.call("material-set-graph", { material: a.id, graph: proceduralGraph() });
  await engine.call("material-set-graph", { material: b.id, graph: proceduralGraph() });

  const cooked = await engine.call<{ compiled: number; failed: number }>("material-cook", {});
  expect(cooked.compiled).toBeGreaterThanOrEqual(2);
  expect(cooked.failed).toBe(0);

  expect(existsSync(join(root, "assets", "materials", `${a.id}_mesh.spv`))).toBe(true);
  expect(existsSync(join(root, "assets", "materials", `${b.id}_mesh.spv`))).toBe(true);
});
