// Built-in primitive meshes (cube/plane/sphere): each spawns as a reserved-id `Mesh`
// (`3`/`4`/`5`) plus a default `MaterialSet`, adds NO rows to the asset catalog, and its
// reserved id survives save/reload. Validated by read-back through inspect/list-assets.
// (Ids cross the wire as decimal strings, so the reserved id reads as "3"/"4"/"5".)

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot();
});
afterAll(async () => {
  await engine?.shutdown();
});

const PRIMITIVES = [
  { preset: "cube", name: "Cube", mesh: "3" },
  { preset: "plane", name: "Plane", mesh: "4" },
  { preset: "sphere", name: "Sphere", mesh: "5" },
] as const;

test("primitives spawn as reserved-id meshes with a default material and add no catalog rows", async () => {
  const before = await engine.call("list-assets");
  const beforeCount = before.assets.length;

  for (const p of PRIMITIVES) {
    const ref = await engine.call("add-entity", { preset: p.preset });
    const info = await engine.call("inspect", { entity: ref.id });
    // A native mesh reference by reserved id.
    expect(info.components).toHaveProperty("Mesh");
    expect(info.components.Mesh?.mesh).toBe(p.mesh);
    // Plus a single default material slot.
    expect(info.components).toHaveProperty("MaterialSet");
    expect(info.components.MaterialSet?.slots?.length).toBe(1);
  }

  // The whole point: primitives never touch the catalog — no rows added, none named for them.
  const after = await engine.call("list-assets");
  expect(after.assets.length).toBe(beforeCount);
  const names = after.assets.map((a) => a.name);
  for (const p of PRIMITIVES) {
    expect(names).not.toContain(p.name);
  }
});

test("a spawned primitive serializes as its reserved id and survives save/reload", async () => {
  await engine.call("add-entity", { preset: "sphere" });
  const dir = await mkdtemp(join(tmpdir(), "saffron-primitive-"));
  const projectPath = join(dir, "project.json");
  await engine.call("save-project", { path: projectPath });
  await engine.loadProject(projectPath);

  // Addressed by name after reload (ids re-mint); the reserved mesh id round-trips.
  const info = await engine.call("inspect", { entity: "Sphere" });
  expect(info.components.Mesh?.mesh).toBe("5");
});

test("add-entity rejects an unknown preset", async () => {
  await expect(engine.call("add-entity", { args: ["pyramid"] })).rejects.toThrow();
});
