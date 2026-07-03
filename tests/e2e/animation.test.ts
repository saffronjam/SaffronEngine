// Skeletal clip import over the control plane: a rigged+animated glTF bakes into one `.smodel` whose
// clip is an embedded Animation sub-asset (container-linked) in the catalog, and saving the project
// persists that entry. The clip sampling/IO math is covered by the engine's animation + geometry
// self-tests; this file proves the import + catalog wire.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
const FIXTURE = join(REPO, "engine", "assets", "models", "animated-strip.gltf");

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

interface Asset {
  id: string;
  name: string;
  type: string;
  duration?: number;
  container?: string;
}

/// Files of one extension anywhere under a directory.
function findByExt(dir: string, ext: string): string[] {
  return readdirSync(dir, { recursive: true })
    .map((entry) => entry.toString())
    .filter((entry) => entry.endsWith(ext));
}

test("importing a rigged+animated glTF registers an Animation clip asset", async () => {
  await engine.call("import-model", { path: FIXTURE });
  await engine.settle();

  const { assets } = await engine.call<{ assets: Asset[] }>("list-assets");
  const clips = assets.filter((a) => a.type === "animation");
  expect(clips.length).toBe(1);
  expect(clips[0].name).toBe("Bend");
});

test("the clip is an embedded chunk of one .smodel, not a loose .sanim sidecar", async () => {
  // Exactly one container on disk; no loose .sanim — the clip lives as a SANM chunk inside it.
  expect(findByExt(engine.appdata, ".smodel").length).toBe(1);
  expect(findByExt(engine.appdata, ".sanim").length).toBe(0);
  // The Animation catalog row is a sub-asset of the model (container-linked), not a standalone file.
  const { assets } = await engine.call<{ assets: Asset[] }>("list-assets");
  const clip = assets.find((a) => a.type === "animation");
  expect(clip?.container).toBeDefined();
});

test("save-project persists the Animation entry to project.json", async () => {
  await engine.call("save-project");
  const projectFile = findByExt(engine.appdata, "project.json")[0];
  expect(projectFile).toBeDefined();
  const doc = JSON.parse(readFileSync(join(engine.appdata, projectFile), "utf8"));
  const clips = (doc.assets ?? []).filter((a: Asset) => a.type === "animation");
  expect(clips.length).toBe(1);
  expect(clips[0].name).toBe("Bend");
  // catalogToJson carries the clip length on animation rows.
  expect(clips[0].duration).toBeCloseTo(1.0, 3);
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
