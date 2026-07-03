// glTF alpha `BLEND` end to end: a material set to `translucent` resolves to the blend PSO
// and records into the scene pass's sorted translucent scope. The proof is a validation-clean
// render (the blend pipeline + depth-write-off state build and record without a Vulkan error)
// plus the blend axis round-tripping through the component and a project save/reload.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { InspectResult } from "@saffron/protocol";

let engine: Engine;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

interface Slot {
  blend?: string;
}
async function slot0Blend(id: string): Promise<string | undefined> {
  const info = await engine.call<InspectResult>("inspect", { entity: id });
  const set = info.components.MaterialSet as { slots?: Slot[] } | undefined;
  return set?.slots?.[0]?.blend;
}

let meshId = "";

test("an imported material defaults to opaque", async () => {
  const imported = await engine.importEntity(FIXTURE, "Blended");
  meshId = imported.id;
  await engine.settle();
  expect(await slot0Blend(meshId)).toBe("opaque");
});

test("set-material blend=translucent renders validation-clean", async () => {
  await engine.call("set-material", { entity: meshId, slot: 0, blend: "translucent" });
  await engine.settle();
  expect(await slot0Blend(meshId)).toBe("translucent");
  // The blend PSO (blend on, depth-write off) built and the translucent scope recorded with
  // no Vulkan validation error — the load-bearing correctness signal for the new pass.
  expect(engine.validationErrors()).toEqual([]);
});

test("an invalid blend token is rejected", async () => {
  await expect(
    engine.call("set-material", { entity: meshId, slot: 0, blend: "glassy" }),
  ).rejects.toThrow();
});

test("the translucent blend survives a project save + reload", async () => {
  await engine.call("save-project");
  await engine.reloadProject();
  await engine.settle();
  const list = await engine.call<{ entities: { id: string; name: string }[] }>("list-entities");
  const entity = list.entities.find((e) => e.name === "Blended");
  expect(entity).toBeDefined();
  expect(await slot0Blend(entity!.id)).toBe("translucent");
  expect(engine.validationErrors()).toEqual([]);
});
