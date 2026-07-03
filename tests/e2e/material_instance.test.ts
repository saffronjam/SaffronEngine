// Phase 16: material instances. A child .smat with a `parent` resolves to the parent's params with
// its sparse `overrides` applied — and editing the parent reflows every instance (edit-once-propagate).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("an instance inherits its parent, applies overrides, and reflows on parent edit", async () => {
  const p = await engine.call<{ id: string }>("material-create", { name: "Parent" });
  await engine.call("material-update", { material: p.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } }); // red
  const c = await engine.call<{ id: string }>("material-create-instance", {
    parent: p.id,
    name: "Inst",
  });

  const pShot = await engine.call<{ png: string }>("preview-render", { material: p.id, size: 128 });
  const cInherit = await engine.call<{ png: string }>("preview-render", { material: c.id, size: 128 });
  expect(cInherit.png).toBe(pShot.png); // pure inheritance, no overrides yet

  await engine.call("material-set-override", { material: c.id, field: "roughness", value: 0.15 });
  const cOverride = await engine.call<{ png: string }>("preview-render", { material: c.id, size: 128 });
  expect(cOverride.png).not.toBe(pShot.png); // the roughness override diverges it

  await engine.call("material-update", { material: p.id, baseColor: { x: 0, y: 1, z: 0, w: 1 } }); // green
  const cReflow = await engine.call<{ png: string }>("preview-render", { material: c.id, size: 128 });
  expect(cReflow.png).not.toBe(cOverride.png); // reflowed red -> green, keeping the override
  expect(engine.validationErrors()).toEqual([]);
});
