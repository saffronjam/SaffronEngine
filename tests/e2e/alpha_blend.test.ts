// glTF alpha `BLEND` end to end: a slot's `blend` override set to `translucent` resolves to
// the blend PSO and records into the scene pass's sorted translucent scope. The proof is a
// validation-clean render (the blend pipeline + depth-write-off state build and record without
// a Vulkan error) plus the blend override round-tripping through the component and a project
// save/reload. `blend` is a per-object override (opaque passthrough), layered over the slot's
// referenced `.smat` (which defaults to opaque).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

async function slot0Overrides(id: string): Promise<Record<string, unknown>> {
  const info = await engine.call("inspect", { entity: id });
  const set = info.components.MaterialSet as
    | { slots?: { overrides?: Record<string, unknown> }[] }
    | undefined;
  return set?.slots?.[0]?.overrides ?? {};
}

async function slot0Material(id: string): Promise<string> {
  const info = await engine.call("inspect", { entity: id });
  return info.components.MaterialSet?.slots[0]?.material ?? "0";
}

async function setSlot0Blend(id: string, blend: string): Promise<void> {
  await engine.call("set-component-field", {
    entity: id,
    component: "MaterialSet",
    field: "slots",
    index: 0,
    value: { overrides: { blend } },
  });
}

let meshId = "";

test("an imported material has no blend override and its .smat is opaque", async () => {
  const imported = await engine.importEntity(FIXTURE, "Blended");
  meshId = imported.id;
  await engine.settle();
  expect((await slot0Overrides(meshId)).blend).toBeUndefined();
  const m = await engine.call("material-get", {
    material: await slot0Material(meshId),
  });
  expect(m.blend).toBe("opaque");
});

test("an unknown blend override is stored verbatim and renders clean", async () => {
  // Overrides are opaque passthrough — an unknown token is stored as-is; the resolve path is
  // lenient (BlendMode::from_wire coerces it to opaque) so the render stays validation-clean.
  await setSlot0Blend(meshId, "glassy");
  await engine.settle();
  expect((await slot0Overrides(meshId)).blend).toBe("glassy");
  expect(engine.validationErrors()).toEqual([]);
});

test("a translucent slot override renders validation-clean", async () => {
  await setSlot0Blend(meshId, "translucent");
  await engine.settle();
  expect((await slot0Overrides(meshId)).blend).toBe("translucent");
  // The blend PSO (blend on, depth-write off) built and the translucent scope recorded with
  // no Vulkan validation error — the load-bearing correctness signal for the new pass.
  expect(engine.validationErrors()).toEqual([]);
});

test("the translucent blend override survives a project save + reload", async () => {
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();
  const list = await engine.call("list-entities");
  const entity = list.entities.find((e) => e.name === "Blended");
  expect(entity).toBeDefined();
  expect((await slot0Overrides(entity!.id)).blend).toBe("translucent");
  expect(engine.validationErrors()).toEqual([]);
});
