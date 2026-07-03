// The catalog cache (assets/.cache/catalog.json) is a latency shortcut, never load-bearing: deleting
// or corrupting it must yield the exact same catalog from a cold scan. This is the guard that keeps the
// filesystem — not the cache — the source of truth.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-cache-${process.pid}`;
const cachePath = `${projectDir}/assets/.cache/catalog.json`;
const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.call("load-project", { path: `${projectDir}/project.json` });
  await engine.call("import-model", { path: FIXTURE });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.call("load-project", { path: `${projectDir}/project.json` });
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

async function catalogIds(): Promise<string[]> {
  const assets = await engine.call<AssetList>("list-assets");
  return assets.assets.map((a) => a.id).sort();
}

let baseline: string[] = [];

test("loading a project writes a catalog cache", async () => {
  expect(existsSync(cachePath)).toBe(true);
  baseline = await catalogIds();
  expect(baseline.length).toBeGreaterThan(0);
});

test("deleting the cache yields an identical catalog from a cold scan", async () => {
  rmSync(cachePath, { force: true });
  await engine.call("load-project", { path: `${projectDir}/project.json` });
  await engine.settle();
  expect(await catalogIds()).toEqual(baseline);
  expect(existsSync(cachePath)).toBe(true); // a cold scan rewrites it
});

test("a corrupt cache falls back to a clean full scan", async () => {
  writeFileSync(cachePath, "{ not valid json ]");
  await engine.call("load-project", { path: `${projectDir}/project.json` });
  await engine.settle();
  expect(await catalogIds()).toEqual(baseline);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
