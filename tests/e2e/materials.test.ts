// Multi-material import over the control plane: a glTF whose single mesh has two
// primitives with distinct PBR materials imports as one entity carrying a MaterialSet of
// two slots — each slot *referencing* a baked `.smat` chunk (a non-zero material id, empty
// overrides), one slot per source material. The per-source factors live on the referenced
// `.smat` (read back via material-get). Editing a single slot through set-component-field
// (component MaterialSet, field slots, the slot index) merges a sparse override into just
// that slot and leaves the others untouched, and the slots round-trip through save/reload.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { InspectResult } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

interface MaterialSlot {
  material: string;
  overrides: Record<string, unknown>;
}

async function slotsOf(id: string): Promise<MaterialSlot[]> {
  const info = await engine.call<InspectResult>("inspect", { entity: id });
  const set = info.components.MaterialSet as { slots?: MaterialSlot[] } | undefined;
  return set?.slots ?? [];
}

// The per-source factors now live on the referenced `.smat`, not inline on the slot.
async function materialFactors(id: string): Promise<{ metallic: number; roughness: number }> {
  return engine.call<{ metallic: number; roughness: number }>("material-get", { material: id });
}

let meshId = "";

test("a two-material glTF imports as a MaterialSet referencing a .smat per slot", async () => {
  const imported = await engine.importEntity(FIXTURE, "Mesh");
  meshId = imported.id;
  await engine.settle();

  const slots = await slotsOf(meshId);
  expect(slots.length).toBe(2);
  // Each slot references a baked `.smat` chunk (a real id, empty overrides), not inline factors.
  expect(slots[0].material).not.toBe("0");
  expect(slots[1].material).not.toBe("0");
  expect(slots[0].overrides).toEqual({});
  expect(slots[1].overrides).toEqual({});

  // The importer must bake the factors (not the 0.0/1.0 defaults) onto each referenced `.smat`.
  const m0 = await materialFactors(slots[0].material);
  const m1 = await materialFactors(slots[1].material);
  expect(m0.metallic).toBeCloseTo(1.0, 3);
  expect(m0.roughness).toBeCloseTo(0.1, 3);
  expect(m1.metallic).toBeCloseTo(0.0, 3);
  expect(m1.roughness).toBeCloseTo(0.9, 3);
});

test("set-component-field with a slot index edits only that slot", async () => {
  await engine.call("set-component-field", {
    entity: meshId,
    component: "MaterialSet",
    field: "slots",
    index: 1,
    value: { overrides: { roughness: 0.25 } },
  });
  await engine.settle();
  const slots = await slotsOf(meshId);
  expect(slots[1].overrides.roughness).toBeCloseTo(0.25, 3);
  expect(slots[0].overrides.roughness).toBeUndefined(); // untouched
});

test("an out-of-range slot index is rejected", async () => {
  await expect(
    engine.call("set-component-field", {
      entity: meshId,
      component: "MaterialSet",
      field: "slots",
      index: 9,
      value: { overrides: { metallic: 0.5 } },
    }),
  ).rejects.toThrow();
});

test("the MaterialSet slots survive a project save + reload", async () => {
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();
  // Reload replaces the scene with fresh entities; the model was instantiated as an entity named "Mesh".
  const list = await engine.call<{ entities: { id: string; name: string }[] }>("list-entities");
  const entity = list.entities.find((e) => e.name === "Mesh");
  expect(entity).toBeDefined();
  const slots = await slotsOf(entity!.id);
  expect(slots.length).toBe(2);
  expect(slots[1].overrides.roughness).toBeCloseTo(0.25, 3); // the override survived
  expect(slots[0].material).not.toBe("0"); // the reference survived
  expect((await materialFactors(slots[0].material)).metallic).toBeCloseTo(1.0, 3);
});

// A single-material glTF that maps metalness/roughness through a metallicRoughnessTexture
// (the shape of the Khronos MetalRoughSpheres ball matrix) must import that texture, not
// drop it. The reference is baked onto the slot's referenced `.smat` (as the packed ORM map)
// and survives a save/reload round-trip.
async function ormTextureOf(entityId: string): Promise<string> {
  const slots = await slotsOf(entityId);
  if (slots.length === 0) {
    return "0";
  }
  const m = await engine.call<{ ormTexture: string }>("material-get", { material: slots[0].material });
  return m.ormTexture;
}

test("a glTF metallic-roughness texture is imported onto the material asset", async () => {
  const imported = await engine.importEntity(MAPPED);
  await engine.settle();
  const mr = await ormTextureOf(imported.id);
  expect(mr).toBeDefined();
  expect(mr).not.toBe("0"); // a real texture id, not the none sentinel

  // Survives save + reload (serde + catalog round-trip; the entry stays a linear texture).
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();
  const list = await engine.call<{ entities: { id: string }[] }>("list-entities");
  let found = "0";
  for (const e of list.entities) {
    const mrAfter = await ormTextureOf(e.id);
    if (mrAfter !== "0") {
      found = mrAfter;
    }
  }
  expect(found).not.toBe("0");

  expect(engine.validationErrors?.() ?? []).toEqual([]);
});
