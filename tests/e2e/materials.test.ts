// Multi-material import over the control plane: a glTF whose single mesh has two
// primitives with distinct PBR materials imports as one entity carrying a
// MaterialSetComponent of two slots — each slot preserving its metallic/roughness
// factors. Editing a
// single slot through set-material with `slot` leaves the others untouched, and the
// slots round-trip through project save/reload.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef, InspectResult } from "@saffron/protocol";

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
  baseColor: [number, number, number, number];
  metallic: number;
  roughness: number;
}

async function slotsOf(id: string): Promise<MaterialSlot[]> {
  const info = await engine.call<InspectResult>("inspect", { entity: id });
  const set = info.components.MaterialSet as { slots?: MaterialSlot[] } | undefined;
  return set?.slots ?? [];
}

let meshId = "";

test("a two-material glTF imports as a MaterialSet preserving each slot's factors", async () => {
  const imported = await engine.importEntity(FIXTURE, "Mesh");
  meshId = imported.id;
  await engine.settle();

  const slots = await slotsOf(meshId);
  expect(slots.length).toBe(2);
  // The importer must keep the factors (not the 0.0/1.0 defaults) per source material.
  expect(slots[0].metallic).toBeCloseTo(1.0, 3);
  expect(slots[0].roughness).toBeCloseTo(0.1, 3);
  expect(slots[1].metallic).toBeCloseTo(0.0, 3);
  expect(slots[1].roughness).toBeCloseTo(0.9, 3);
});

test("set-material with a slot edits only that slot", async () => {
  await engine.call("set-material", { entity: meshId, slot: 1, roughness: 0.25 });
  await engine.settle();
  const slots = await slotsOf(meshId);
  expect(slots[1].roughness).toBeCloseTo(0.25, 3);
  expect(slots[0].roughness).toBeCloseTo(0.1, 3); // untouched
});

test("an out-of-range slot is rejected", async () => {
  await expect(engine.call("set-material", { entity: meshId, slot: 9, metallic: 0.5 })).rejects.toThrow();
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
  expect(slots[1].roughness).toBeCloseTo(0.25, 3);
  expect(slots[0].metallic).toBeCloseTo(1.0, 3);
});

// A single-material glTF that maps metalness/roughness through a metallicRoughnessTexture
// (the shape of the Khronos MetalRoughSpheres ball matrix) must import that texture, not
// drop it. The reference is carried on the Material and survives a save/reload round-trip.
function materialOf(components: InspectResult["components"]): { metallicRoughnessTexture?: string } {
  return (components.Material ?? {}) as { metallicRoughnessTexture?: string };
}

test("a glTF metallic-roughness texture is imported onto the Material", async () => {
  const imported = await engine.importEntity(MAPPED);
  await engine.settle();
  const info = await engine.call<InspectResult>("inspect", { entity: imported.id });
  const mr = materialOf(info.components).metallicRoughnessTexture;
  expect(mr).toBeDefined();
  expect(mr).not.toBe("0"); // a real texture id, not the none sentinel

  // Survives save + reload (serde + catalog round-trip; the entry stays a linear texture).
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();
  const list = await engine.call<{ entities: { id: string }[] }>("list-entities");
  let found = "0";
  for (const e of list.entities) {
    const info = await engine.call<InspectResult>("inspect", { entity: e.id });
    const mrAfter = materialOf(info.components).metallicRoughnessTexture;
    if (mrAfter && mrAfter !== "0") {
      found = mrAfter;
    }
  }
  expect(found).not.toBe("0");

  expect(engine.validationErrors?.() ?? []).toEqual([]);
});
