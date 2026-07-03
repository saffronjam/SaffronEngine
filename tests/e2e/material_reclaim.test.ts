// Phase 15 (bindless slot reclamation): a destroyed GpuTexture returns its bindless slot to a shared
// free-list, which the next upload reuses — so a hot-reloaded / churny scene keeps the pool bounded
// instead of marching nextBindlessIndex to the 1024 limit. new-project clears the asset caches, which
// drops the imported textures' Refs and reclaims their slots; render-stats exposes the counts.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { EntityRef } from "@saffron/protocol";

let engine: Engine;
const MAPPED = join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb");
const root = `/tmp/saffron-e2e-reclaim-${process.pid}`;

interface Stats {
  bindlessTextures: number;
  bindlessFree: number;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(root, { recursive: true, force: true });
});

test("destroyed textures return their bindless slots to the free-list", async () => {
  await engine.importEntity(MAPPED);
  await engine.settle(300);
  const before = await engine.call<Stats>("render-stats");
  expect(before.bindlessTextures).toBeGreaterThan(0);

  // A fresh project clears the asset caches → the imported textures are destroyed → their slots free.
  await engine.call("new-project", { name: "reclaim-test", root });
  await engine.settle(400);
  const after = await engine.call<Stats>("render-stats");

  expect(after.bindlessFree).toBeGreaterThan(before.bindlessFree); // slots were reclaimed
  expect(after.bindlessTextures).toBeLessThanOrEqual(before.bindlessTextures); // high-water didn't grow
});
