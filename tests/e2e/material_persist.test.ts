// Proves a per-object texture override survives project save + reload. Imports a textured
// fixture, assigns a normal map through assign-asset (which writes a slot-0 `normalTexture`
// override on the entity's MaterialSet), saves + reloads the project, and asserts the
// override persisted.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { InspectResult } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("an assigned normal map survives project save + reload", async () => {
  const e = await engine.importEntity(MAPPED);
  await engine.settle();
  // A real texture id from the imported model's referenced `.smat`.
  const before = await engine.call<InspectResult>("inspect", { entity: e.id });
  const slots = (before.components.MaterialSet as { slots?: { material: string }[] }).slots ?? [];
  expect(slots.length).toBeGreaterThan(0);
  const src = await engine.call<{ albedoTexture: string }>("material-get", {
    material: slots[0].material,
  });
  const tex = src.albedoTexture;
  expect(tex).toBeDefined();
  expect(tex).not.toBe("0");

  // assign-asset(normal) writes a slot-0 `normalTexture` override; round-trip the project.
  await engine.call("assign-asset", { entity: e.id, slot: "normal", asset: tex });
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();

  const after = await engine.call<InspectResult>("inspect", { entity: e.id });
  const afterSlots =
    (after.components.MaterialSet as { slots?: { overrides?: Record<string, string> }[] }).slots ??
    [];
  expect(afterSlots[0]?.overrides?.normalTexture).toBe(tex);
});
