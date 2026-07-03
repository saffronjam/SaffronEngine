// Scene/entity operations, validated by READ-BACK through inspect/list/get — mutate, then
// query and assert the observed state. Entities are addressed by NAME, which keeps these
// tests independent of the freshly minted u64 ids and exercises a realistic client path.
// (Ids cross the wire as decimal strings, so a plain JSON.parse keeps them lossless.)

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import type { EntityList } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot();
});
afterAll(async () => {
  await engine?.shutdown();
});

const names = (list: EntityList) => list.entities.map((e) => e.name);

test("create-entity appears in list-entities, destroy-entity removes it", async () => {
  const name = "e2e-lifecycle";
  await engine.call("create-entity", { args: [name] });
  expect(names(await engine.call<EntityList>("list-entities"))).toContain(name);

  await engine.call("destroy-entity", { entity: name });
  expect(names(await engine.call<EntityList>("list-entities"))).not.toContain(name);
});

test("set-transform is observable via inspect", async () => {
  const name = "e2e-xform";
  await engine.call("create-entity", { args: [name] }); // createEntity adds a Transform
  await engine.call("set-transform", { entity: name, translation: { x: 1.5, y: -2, z: 3.25 } });

  const info = await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
    entity: name,
  });
  expect(info.components.Transform.translation).toEqual({ x: 1.5, y: -2, z: 3.25 });
});

test("add-component / remove-component are observable via inspect", async () => {
  const name = "e2e-components";
  await engine.call("create-entity", { args: [name] });

  await engine.call("add-component", { entity: name, component: "Material" });
  let info = await engine.call<{ components: Record<string, unknown> }>("inspect", {
    entity: name,
  });
  expect(info.components).toHaveProperty("Material");

  await engine.call("remove-component", { entity: name, component: "Material" });
  info = await engine.call<{ components: Record<string, unknown> }>("inspect", { entity: name });
  expect(info.components).not.toHaveProperty("Material");
});

test("component order is explicit, add-at-bottom, validated, and persisted", async () => {
  const name = "e2e-component-order";
  await engine.call("create-entity", { args: [name] });

  let info = await engine.call<Inspect>("inspect", { entity: name });
  expect(info.componentOrder).toEqual(["Name", "Transform"]);

  await engine.call("add-component", { entity: name, component: "Camera" });
  await engine.call("add-component", { entity: name, component: "Material" });
  info = await engine.call<Inspect>("inspect", { entity: name });
  expect(info.componentOrder).toEqual(["Name", "Transform", "Camera", "Material"]);

  await engine.call("set-component-order", {
    entity: name,
    components: ["Material", "Name", "Camera", "Transform"],
  });
  info = await engine.call<Inspect>("inspect", { entity: name });
  expect(info.componentOrder).toEqual(["Material", "Name", "Camera", "Transform"]);

  await expect(
    engine.call("set-component-order", {
      entity: name,
      components: ["Material", "Name", "Camera", "Camera"],
    }),
  ).rejects.toThrow(/duplicate|non-present|must contain/);

  await engine.call("remove-component", { entity: name, component: "Camera" });
  info = await engine.call<Inspect>("inspect", { entity: name });
  expect(info.componentOrder).toEqual(["Material", "Name", "Transform"]);

  await engine.call("set-component-order", {
    entity: name,
    components: ["Name", "Transform", "Material"],
  });
  const dir = await mkdtemp(join(tmpdir(), "saffron-component-order-"));
  const projectPath = join(dir, "project.json");
  await engine.call("save-project", { path: projectPath });
  await engine.loadProject(projectPath);

  info = await engine.call<Inspect>("inspect", { entity: name });
  expect(info.componentOrder).toEqual(["Name", "Transform", "Material"]);
});

test("set-gizmo round-trips through get-gizmo", async () => {
  await engine.call("set-gizmo", { op: "rotate", space: "local" });
  expect(await engine.call("get-gizmo")).toEqual({
    op: "rotate",
    space: "local",
    preserveChildren: false,
  });

  await engine.call("set-gizmo", { op: "translate", space: "world" });
  expect(await engine.call("get-gizmo")).toEqual({
    op: "translate",
    space: "world",
    preserveChildren: false,
  });
});

// Negative oracles: bad input must be REJECTED (ok:false), not silently accepted.
test("invalid input is rejected, not silently accepted", async () => {
  await expect(engine.call("set-aa", { args: ["nonsense"] })).rejects.toThrow();
  await expect(engine.call("inspect", { entity: "does-not-exist" })).rejects.toThrow();
  await expect(engine.call("set-exposure", { args: ["not-a-number"] })).rejects.toThrow();
});

interface Vec3 {
  x: number;
  y: number;
  z: number;
}

interface Inspect {
  components: Record<string, unknown>;
  componentOrder: string[];
}
