// Phase 18 (scene-path codegen): a non-foldable material graph is spliced into the real übershader
// (mesh.slang's evalSurface, between the @graph markers) and slangc-compiled to a per-material PSO, so
// the codegen material renders on an actual scene entity with full lighting — not just the preview.
// Proves: (1) material-set-graph compiled the übershader variant on disk; (2) assigning it to an entity
// changes the render; (3) the spliced shader binds + draws validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const shots: string[] = [];

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-ibl", { args: ["on"] }).catch(() => {});
  await engine.call("add-entity", { args: ["directional-light"] }).catch(() => {});
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-scenecg-${process.pid}-${tag}.png`;
  shots.push(path);
  await engine.call("screenshot", { target: "viewport", path });
  const deadline = Date.now() + 10_000;
  while (!existsSync(path)) {
    if (Date.now() > deadline) {
      throw new Error(`screenshot ${tag} never landed`);
    }
    await engine.settle(100);
  }
  await engine.settle(200);
  return readFileSync(path);
}

test("a codegen material compiles a übershader variant and renders on an entity", async () => {
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);

  const e = await engine.importEntity(MAPPED);
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);
  const before = await screenshot("before");

  const m = await engine.call<{ id: string }>("material-create", { name: "SceneCodegen" });
  const graph = {
    nodes: [
      { id: "c1", type: "constant", props: { value: [0, 1, 0, 1] } },
      { id: "c2", type: "constant", props: { value: [1, 1, 1, 1] } },
      { id: "mul", type: "multiply" },
      { id: "out", type: "materialOutput" },
    ],
    edges: [
      { from: ["c1", "rgba"], to: ["mul", "a"] },
      { from: ["c2", "rgba"], to: ["mul", "b"] },
      { from: ["mul", "rgba"], to: ["out", "baseColor"] },
    ],
  };
  const set = await engine.call<{ foldable: boolean }>("material-set-graph", { material: m.id, graph });
  expect(set.foldable).toBe(false); // procedural multiply -> codegen path

  // material-set-graph compiled a per-material übershader variant (the splice produced valid Slang).
  const spv = join(root, "assets", "materials", `${m.id}_mesh.spv`);
  expect(existsSync(spv)).toBe(true);

  await engine.call("material-assign", { entity: e.id, material: m.id });
  await engine.settle(400);
  const after = await screenshot("after");

  expect(after.equals(before)).toBe(false); // the codegen surface changed the rendered result
  expect(engine.validationErrors()).toEqual([]); // the spliced übershader variant bound + drew cleanly
});
