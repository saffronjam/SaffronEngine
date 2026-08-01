// Play mode over the control plane: the state machine and its invariants, the camera-handover
// flag, and — the headline property — the discard guarantee. Play duplicates the authored scene,
// every read/write routes to the duplicate, and stop throws it away, so nothing done during play
// touches the authored scene. Each case proves that the way the editor experiences it: over the
// wire, against a real headless engine.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

afterEach(async () => {
  const state = await engine.call("get-play-state");
  if (state.state !== "edit") {
    await engine.call("stop");
  }
});

test("the play state machine accepts only legal transitions", async () => {
  expect((await engine.call("get-play-state")).state).toBe("edit");

  const playing = await engine.call("play");
  expect(playing.state).toBe("playing");
  await expect(engine.call("play")).rejects.toThrow(); // already playing
  await expect(engine.call("step")).rejects.toThrow(); // step requires pause

  const paused = await engine.call("pause");
  expect(paused.state).toBe("paused");
  await expect(engine.call("pause")).rejects.toThrow(); // already paused

  const stepped = await engine.call("step", { frames: 1 });
  expect(stepped.state).toBe("paused"); // step does not change state

  const resumed = await engine.call("play"); // play resumes from paused
  expect(resumed.state).toBe("playing");

  const stopped = await engine.call("stop");
  expect(stopped.state).toBe("edit");
  await expect(engine.call("step")).rejects.toThrow(); // step requires pause
  expect((await engine.call("stop")).state).toBe("edit"); // idempotent in edit

  // playVersion strictly increases across every transition.
  expect(playing.playVersion).toBeLessThan(stopped.playVersion);
});

test("hasPrimaryCamera reflects whether the scene has one", async () => {
  const entities = (await engine.call("list-entities")).entities;
  const inspected = await Promise.all(
    entities.map((entity) => engine.call("inspect", { entity: entity.id })),
  );
  const camera = inspected.find((entity) => entity.components.Camera?.primary === true);
  expect(camera).toBeDefined();

  await engine.call("set-component-field", {
    entity: camera!.id,
    component: "Camera",
    field: "primary",
    value: false,
  });
  const noCamera = await engine.call("play");
  expect(noCamera.hasPrimaryCamera).toBe(false);
  await engine.call("stop");

  await engine.call("set-component-field", {
    entity: camera!.id,
    component: "Camera",
    field: "primary",
    value: true,
  });
  const withCamera = await engine.call("play");
  expect(withCamera.hasPrimaryCamera).toBe(true);
  await engine.call("stop");
});

test("stop discards runtime mutations and restores the authored scene", async () => {
  const cube = await engine.call("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  const countBefore = (await engine.call("list-entities")).entities.length;

  await engine.call("play");
  // Reads and writes route to the play duplicate.
  await engine.call("set-transform", { entity: cube.id, translation: { x: 9, y: 9, z: 9 } });
  const runtime = await engine.call("inspect", { entity: cube.id });
  expect(runtime.components.Transform!.translation).toEqual({ x: 9, y: 9, z: 9 });
  await engine.call("add-entity", { args: ["cube"] }); // a runtime-only entity

  const beforeStop = (await engine.call("get-play-state")).sceneVersion;
  const stopped = await engine.call("stop");
  expect(stopped.sceneVersion).toBeGreaterThan(beforeStop); // the editor-refresh trigger

  const authored = await engine.call("inspect", { entity: cube.id });
  expect(authored.components.Transform!.translation).toEqual({ x: 1, y: 2, z: 3 });
  const after = await engine.call("list-entities");
  expect(after.entities.length).toBe(countBefore); // the runtime entity did not survive
  expect(after.entities.some((e) => e.id === cube.id)).toBe(true);
});

test("selection survives play/stop by uuid; a runtime selection clears on stop", async () => {
  const cube = await engine.call("add-entity", { args: ["cube"] });
  await engine.call("select", { entity: cube.id });

  await engine.call("play");
  expect((await engine.call("get-selection")).entity?.id).toBe(cube.id); // the play twin
  await engine.call("stop");
  expect((await engine.call("get-selection")).entity?.id).toBe(cube.id); // the authored entity

  // A runtime-spawned selection has no authored twin and clears on stop.
  await engine.call("play");
  const runtime = await engine.call("add-entity", { args: ["cube"] }); // add-entity selects it
  expect((await engine.call("get-selection")).entity?.id).toBe(runtime.id);
  await engine.call("stop");
  expect((await engine.call("get-selection")).entity ?? null).toBeNull();
});

test("scene/project swaps are blocked during play", async () => {
  await engine.call("play");
  await expect(engine.call("load-scene", { path: "nope.json" })).rejects.toThrow(/stop play first/);
  await expect(engine.call("load-project", { path: "nope" })).rejects.toThrow(/stop play first/);
  await expect(engine.call("delete-asset", { asset: "anything" })).rejects.toThrow(/stop play first/);
  await engine.call("stop");
});

test("environment edits during play are discarded on stop", async () => {
  // get-environment returns the environment object directly (a Json passthrough).
  const authored = (await engine.call("get-environment")).skyIntensity;

  await engine.call("play");
  await engine.call("set-environment", { skyIntensity: authored + 5 });
  const during = (await engine.call("get-environment")).skyIntensity;
  expect(during).toBeCloseTo(authored + 5);

  await engine.call("stop");
  const back = (await engine.call("get-environment")).skyIntensity;
  expect(back).toBeCloseTo(authored);
});

test("an asset assignment during play is discarded; delete-asset is blocked", async () => {
  await engine.importEntity(join(REPO, "tests", "e2e", "fixtures", "mapped-material.glb"));
  const assets = await engine.call("list-assets");
  const mesh = assets.assets.find((a) => a.type === "mesh");
  expect(mesh).toBeDefined();

  const target = await engine.call("add-entity", { args: ["empty"] }); // no Mesh authored
  expect((await engine.call("inspect", { entity: target.id })).components.Mesh).toBeUndefined();

  await engine.call("play");
  await engine.call("assign-asset", { entity: target.id, slot: "mesh", asset: mesh!.id });
  const during = await engine.call("inspect", { entity: target.id });
  expect(during.components.Mesh?.mesh).toBe(mesh!.id);
  await expect(engine.call("delete-asset", { asset: mesh!.id })).rejects.toThrow(/stop play first/);

  await engine.call("stop");
  expect((await engine.call("inspect", { entity: target.id })).components.Mesh).toBeUndefined();
});

test("a material edit during play is discarded on stop", async () => {
  const cube = await engine.call("add-entity", { args: ["cube"] });
  // Author a slot-0 roughness override on the cube's MaterialSet (the cube instantiates with a
  // single default slot).
  const setRoughness = (id: string, roughness: number) =>
    engine.call("set-component-field", {
      entity: id,
      component: "MaterialSet",
      field: "slots",
      index: 0,
      value: { overrides: { roughness } },
    });
  // The per-object override map is opaque JSON on the wire, so the numeric read is explicit.
  const roughnessOf = async (id: string): Promise<number> =>
    Number(
      (await engine.call("inspect", { entity: id })).components.MaterialSet!.slots[0].overrides
        .roughness,
    );

  await setRoughness(cube.id, 0.2);
  await engine.settle();
  const authored = await roughnessOf(cube.id);
  expect(authored).toBeCloseTo(0.2);

  await engine.call("play");
  await setRoughness(cube.id, 0.9);
  await engine.settle();
  const during = await roughnessOf(cube.id);
  expect(during).toBeGreaterThan(authored + 0.1);

  await engine.call("stop");
  const back = await roughnessOf(cube.id);
  expect(back).toBeCloseTo(authored);
});

test("the play/stop cycles leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
