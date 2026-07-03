// Proves the imported metallic-roughness texture actually reaches the GPU and changes the
// shaded result — not just that the Uuid is stored. Renders the mapped-material fixture (a
// single material with a metallic-roughness map) under fixed lighting, screenshots it, then
// clears the MR texture so the surface falls back to the scalar factors, and asserts the two
// frames differ. A fixed camera + scene makes the only variable the MR texture, so a byte
// difference is caused by it. The validation log must stay clean (the second sampled texture
// / bindless index introduces no Vulkan errors).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef, InspectResult } from "@saffron/protocol";

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

test("the imported metallic-roughness texture changes the shaded pixels", async () => {
  const e = await engine.importEntity(MAPPED);
  // Frame the fixture's triangle (spans x,y in [0,1], facing +Z) head-on.
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);

  const info = await engine.call<InspectResult>("inspect", { entity: e.id });
  const mr = (info.components.Material as { metallicRoughnessTexture?: string }).metallicRoughnessTexture;
  expect(mr).toBeDefined();
  expect(mr).not.toBe("0");
  const withTexture = await screenshot("with");

  // Clear the MR texture: the surface now samples the default white (factors only), a
  // different roughness than the smooth map → the shaded result must change.
  await engine.call("assign-asset", { entity: e.id, slot: "metallic-roughness", asset: "0" });
  await engine.settle(300);
  const withoutTexture = await screenshot("without");

  expect(withoutTexture.equals(withTexture)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});
