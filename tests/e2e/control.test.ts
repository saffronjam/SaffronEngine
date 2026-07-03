// Control-plane wire-contract tests for three properties:
//   - entity ids round-trip BY ID as decimal-u64 JSON strings (the precision-safe form;
//     these ids exceed 2^53, so a plain JSON.parse only stays lossless because they are
//     strings);
//   - every scene mutation bumps sceneVersion (observed through get-selection);
//   - render-stats carries live frame timing (frameMs/fps/gpuMs).
// Boots with SAFFRON_SCRATCH_PROJECT so add-entity's cube preset (which imports a model,
// and so needs a loaded project) works; the project lives under the harness temp appdata dir.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { EntityList, EntityRef, InspectResult, RenderStats, Selection } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

const DECIMAL_U64 = /^[0-9]+$/;
const sceneVersion = async () => (await engine.call<Selection>("get-selection")).sceneVersion;
const ids = (list: EntityList) => list.entities.map((e) => e.id);

// The whole lifecycle, addressed BY ID: add → select → copy → rename → destroy, asserting
// the id is a decimal string everywhere it surfaces and that sceneVersion strictly climbs.
test("entity ids round-trip by id and mutations bump sceneVersion", async () => {
  const cube = await engine.call<EntityRef>("add-entity", { args: ["cube"] });
  expect(typeof cube.id).toBe("string");
  expect(cube.id).toMatch(DECIMAL_U64);
  let version = await sceneVersion();

  // select by id, then read it back through get-selection — same id string.
  const selected = await engine.call<EntityRef>("select", { entity: cube.id });
  expect(selected.id).toBe(cube.id);
  const selection = await engine.call<Selection>("get-selection");
  expect(selection.entity?.id).toBe(cube.id);

  // copy by id: a distinct entity with its own decimal-string id, and the list grows by one.
  const before = await engine.call<EntityList>("list-entities");
  const copy = await engine.call<EntityRef>("copy-entity", { entity: cube.id });
  expect(copy.id).toMatch(DECIMAL_U64);
  expect(copy.id).not.toBe(cube.id);
  const afterCopy = await engine.call<EntityList>("list-entities");
  expect(afterCopy.entities.length).toBe(before.entities.length + 1);
  expect(ids(afterCopy)).toContain(cube.id);
  expect(ids(afterCopy)).toContain(copy.id);
  expect(await sceneVersion()).toBeGreaterThan(version);
  version = await sceneVersion();

  // rename by id, reflected in both inspect and list (same id, new name).
  const renamed = await engine.call<EntityRef>("rename-entity", { entity: cube.id, name: "Renamed" });
  expect(renamed.id).toBe(cube.id);
  expect(renamed.name).toBe("Renamed");
  const inspected = await engine.call<InspectResult>("inspect", { entity: cube.id });
  expect(inspected.id).toBe(cube.id);
  expect(inspected.name).toBe("Renamed");
  expect(inspected.components.Name?.name).toBe("Renamed");
  const listed = (await engine.call<EntityList>("list-entities")).entities.find((e) => e.id === cube.id);
  expect(listed?.name).toBe("Renamed");
  expect(await sceneVersion()).toBeGreaterThan(version);
  version = await sceneVersion();

  // destroy by id: the entity leaves the list, the copy stays.
  await engine.call("destroy-entity", { entity: cube.id });
  const afterDestroy = await engine.call<EntityList>("list-entities");
  expect(afterDestroy.entities.length).toBe(afterCopy.entities.length - 1);
  expect(ids(afterDestroy)).not.toContain(cube.id);
  expect(ids(afterDestroy)).toContain(copy.id);
  expect(await sceneVersion()).toBeGreaterThan(version);
});

// set-transform / set-component each count as a scene mutation; get-selection exposes the
// scene version stamp the editor uses to know it must re-poll.
test("set-transform and set-component bump sceneVersion", async () => {
  const e = await engine.call<EntityRef>("add-entity", { args: ["empty"] });
  expect(e.id).toMatch(DECIMAL_U64);

  let version = await sceneVersion();
  await engine.call("set-transform", { entity: e.id, translation: { x: 1, y: 2, z: 3 } });
  const afterTransform = await sceneVersion();
  expect(afterTransform).toBeGreaterThan(version);

  version = afterTransform;
  await engine.call("set-component", { entity: e.id, component: "Material", json: { metallic: 0.5 } });
  expect(await sceneVersion()).toBeGreaterThan(version);
});

// Environment is scene state too; the editor only re-fetches it when sceneVersion moves, so
// set-environment must bump the counter like every other scene mutation.
test("set-environment bumps sceneVersion", async () => {
  const version = await sceneVersion();
  await engine.call("set-environment", { skyIntensity: 2 });
  expect(await sceneVersion()).toBeGreaterThan(version);
});

// render-stats reports live frame timing; assert it is plausible after a few frames.
test("render-stats reports live frame timing", async () => {
  await engine.settle(400);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.frameMs).toBeGreaterThan(0);
  expect(stats.fps).toBeGreaterThan(0);
  expect(stats.gpuMs).toBeGreaterThanOrEqual(0);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
