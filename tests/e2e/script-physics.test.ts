// The physics bridges a script drives: an impulse on a dynamic body, a character controller walked
// by move_character, and a spherecast against the live world.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import type { Engine } from "./harness.ts";
import { Cleaner } from "./test-utils.ts";
import {
  attachScripts,
  bootScriptEngine,
  stopIfPlaying,
  type Inspect,
  type PlayState,
  type Ref,
} from "./script-utils.ts";

const SCRIPTS = {
  "pusher.lua": `local Pusher = {}
function Pusher.on_update(self, dt)
  if not self.pushed then self.pushed = true self.entity:apply_impulse(sa.vec3(0, 0, 12)) end
end
return Pusher
`,
  "walker.lua": `local Walker = {}
function Walker.on_update(self, dt) self.entity:move_character(sa.vec3(3, 0, 0), false) end
return Walker
`,
  "caster.lua": `local Caster = {}
function Caster.on_update(self, dt)
  if not self.done then
    self.done = true
    local hit = sa.spherecast(0, 5, 0, 0, -1, 0, 0.5, 20)
    if hit.hit then self.entity:set_position(sa.vec3(1, hit.point.y, 0)) end
  end
end
return Caster
`,
};

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  ({ engine } = await bootScriptEngine(cleaner, SCRIPTS));
});

afterEach(async () => {
  await stopIfPlaying(engine);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("physics bindings: impulse pushes a body, move_character walks, spherecast hits", async () => {
  // A static floor for everyone to interact with.
  const floor = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await engine.call("set-transform", { entity: floor.id, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: floor.id, component: "Collider" });
  await engine.call("set-component-field", {
    entity: floor.id,
    component: "Collider",
    field: "halfExtents",
    value: { x: 30, y: 0.1, z: 30 },
  });

  // A dynamic box pushed +Z by an impulse from Lua.
  const box = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await engine.call("set-transform", { entity: box.id, translation: { x: 0, y: 2, z: 0 } });
  await engine.call("add-component", { entity: box.id, component: "Collider" });
  await engine.call("add-component", { entity: box.id, component: "Rigidbody" });
  await attachScripts(engine, box.id, ["pusher.lua"]);

  // A capsule character walked +X by move_character from Lua.
  const character = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await engine.call("set-transform", { entity: character.id, translation: { x: 0, y: 1, z: 5 } });
  await engine.call("add-component", { entity: character.id, component: "Collider" });
  await engine.call("set-component-field", {
    entity: character.id,
    component: "Collider",
    field: "shape",
    value: "capsule",
  });
  await engine.call("add-component", { entity: character.id, component: "CharacterController" });
  await attachScripts(engine, character.id, ["walker.lua"]);

  // A probe that spherecasts down onto the floor.
  const probe = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, probe.id, ["caster.lua"]);

  await engine.call("play");
  await engine.settle(900);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");

  const translation = async (entity: string) =>
    (await engine.call<Inspect>("inspect", { entity })).components.Transform.translation;
  expect((await translation(box.id)).z).toBeGreaterThan(0.5); // the impulse pushed it +Z
  expect((await translation(character.id)).x).toBeGreaterThan(0.3); // move_character walked it +X
  expect((await translation(probe.id)).x).toBeCloseTo(1); // spherecast hit the floor

  await engine.call("stop");
  for (const entity of [floor, box, character, probe]) {
    await engine.call("destroy-entity", { entity: entity.id });
  }
});

test("the script physics cases leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
