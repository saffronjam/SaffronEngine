// Proves a normal map assigned to a material reaches the GPU and perturbs the shaded normal —
// validating the full path: assign-asset(slot:normal) -> MaterialComponent.normalTexture ->
// resolveEntityMaterials -> the FEATURE_NORMAL bit + the deduped material params -> the übershader's
// derivative-TBN normal mapping (phases 04-06). Reuses the fixture's own albedo texture as the
// normal map (its RGB read as tangent-space normals tilts the surface), so no normal-map fixture is
// needed: the only question is whether the perturbed normal changes the shaded result under a fixed
// directional light. The validation log must stay clean (a third sampled bindless texture).

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
  const path = `/tmp/saffron-e2e-nrm-${process.pid}-${tag}.png`;
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

test("an assigned normal map perturbs the shaded result", async () => {
  const e = await engine.importEntity(MAPPED);
  await engine.call("set-camera", { position: { x: 0.35, y: 0.35, z: 2 }, yaw: 0, pitch: 0 });
  await engine.settle(300);

  // Reuse the fixture's own albedo texture as a (deliberately non-flat) normal map.
  const info = await engine.call<InspectResult>("inspect", { entity: e.id });
  const albedo = (info.components.Material as { albedoTexture?: string }).albedoTexture;
  expect(albedo).toBeDefined();
  expect(albedo).not.toBe("0");

  const flat = await screenshot("flat");

  await engine.call("assign-asset", { entity: e.id, slot: "normal", asset: albedo });
  await engine.settle(300);
  const perturbed = await screenshot("perturbed");

  // The perturbed normals must change the directional-light shading.
  expect(perturbed.equals(flat)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});
