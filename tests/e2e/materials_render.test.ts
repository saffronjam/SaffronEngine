// Proves an ORM (metallic-roughness) texture actually reaches the GPU and changes the shaded
// result — not just that the Uuid is stored. Renders the mapped-material fixture (a single
// material referencing a `.smat` with a metallic-roughness map) under fixed lighting, then
// assigns a slot-0 `ormTexture` override that encodes a very different metallic/roughness and
// asserts the two frames differ, before clearing the override again. A fixed camera + scene
// makes the only variable the ORM texture, so a byte difference is caused by it. The validation
// log must stay clean (the second sampled texture / bindless index introduces no Vulkan errors).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { InspectResult } from "@saffron/protocol";

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
  const path = `/tmp/saffron-e2e-mr-${process.pid}-${tag}.png`;
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

test("an ORM texture override changes the shaded pixels", async () => {
  const e = await engine.importEntity(MAPPED);
  // Frame the fixture's triangle (spans x,y in [0,1], facing +Z) head-on.
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);

  // The imported metallic-roughness map lives on the slot's referenced `.smat` (packed ORM).
  const info = await engine.call<InspectResult>("inspect", { entity: e.id });
  const slots = (info.components.MaterialSet as { slots?: { material: string }[] }).slots ?? [];
  expect(slots.length).toBeGreaterThan(0);
  const smat = await engine.call<{ ormTexture: string; albedoTexture: string }>("material-get", {
    material: slots[0].material,
  });
  expect(smat.ormTexture).toBeDefined();
  expect(smat.ormTexture).not.toBe("0"); // the imported MR texture is on the referenced .smat
  expect(smat.albedoTexture).not.toBe("0");
  const withSmatOrm = await screenshot("smat");

  // Override slot 0's ORM with the albedo texture — its channels encode a very different
  // metallic/roughness than the smooth MR map, so the shaded result must change.
  await engine.call("assign-asset", {
    entity: e.id,
    slot: "metallic-roughness",
    asset: smat.albedoTexture,
  });
  await engine.settle(300);
  const withOverride = await screenshot("override");
  expect(withOverride.equals(withSmatOrm)).toBe(false);

  // Clearing the override drops back to the referenced `.smat` ORM (exercises the clear path).
  await engine.call("assign-asset", { entity: e.id, slot: "metallic-roughness", asset: "0" });
  await engine.settle(300);

  expect(engine.validationErrors()).toEqual([]);
});
