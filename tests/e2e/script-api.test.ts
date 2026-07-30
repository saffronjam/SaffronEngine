// The Lua API surface a script reaches the engine through: component snapshots and setters, the
// generic component write and its structural gate, sa.Vec3, entity lifecycle and deferred destroy,
// the coroutine scheduler, messaging, input edges, cross-entity lookup, and the primary camera.
//
// Every script asserts in Lua, and a failed assert pauses play — so `state == "playing"` after a
// settle is what proves the whole API behaved.

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
  // Writes derive only from never-written fields, so every tick is idempotent.
  "reader.lua": `local Reader = {}
function Reader.on_update(self, dt)
  assert(self.entity:valid(), "self.entity must be valid")
  assert(self.entity:name() == "Reader Cube", "name() mismatch: " .. self.entity:name())
  assert(self.entity:get_component("NoSuchComponent") == nil, "unknown component must be nil")
  local t = self.entity:get_component("Transform")
  assert(t ~= nil, "Transform snapshot missing")
  self.entity:set_position(sa.vec3(t.translation.z * 2, 50, t.translation.z))
  self.entity:set_rotation(sa.vec3(0.5, 0, 0))
  self.entity:set_scale(sa.vec3(2, 2, 2))
end
return Reader
`,
  "chaser.lua": `local Chaser = {}
function Chaser.on_update(self, dt)
  assert(not sa.get_entity_by_name("No Such Entity"):valid(), "missing lookup must be invalid")
  local target = sa.get_entity_by_name("Target")
  if target:valid() then
    local p = target:get_position()
    target:set_position(p + sa.vec3(0, 0, dt))
  end
end
return Chaser
`,
  "camera.lua": `local Cam = {}
function Cam.on_update(self, dt)
  local cam = sa.primary_camera()
  if cam:valid() then
    cam:set_position(sa.vec3(0, 5, 10))
  end
end
return Cam
`,
  // Generic component write via the registry's deserialize, plus the structural-component gate.
  "writer.lua": `local Writer = {}
function Writer.on_update(self, dt)
  if not self.entity:has_component("PointLight") then
    assert(self.entity:add_component("PointLight"), "add_component should succeed")
  end
  assert(self.entity:set_component("PointLight", { intensity = 5.0 }), "set_component should succeed")
  assert(self.entity:set_component("Rigidbody", { mass = 9 }) == false, "structural write must be refused")
  assert(self.entity:add_component("Collider") == false, "structural add must be refused")
  assert(self.entity:has_component("Transform"), "has_component(Transform)")
  assert(self.entity:has_component("Nope") == false, "has_component(unknown)")
end
return Writer
`,
  "vectest.lua": `local VecTest = {}
function VecTest.on_update(self, dt)
  local a = sa.vec3(1, 2, 3)
  assert((a + sa.vec3(0, 1, 0)).y == 3, "add")
  assert((a - sa.vec3(0, 1, 0)).y == 1, "sub")
  assert((a * 2).x == 2, "vec*scalar")
  assert((2 * a).z == 6, "scalar*vec")
  assert(math.abs(sa.vec3(3, 0, 0):length() - 3) < 1e-4, "length")
  assert(a:dot(sa.vec3(1, 0, 0)) == 1, "dot")
  assert(sa.vec3(1, 0, 0):cross(sa.vec3(0, 1, 0)).z == 1, "cross")
  local p = self.entity:get_position()
  p.x = 7
  self.entity:set_position(p)
end
return VecTest
`,
  // Entity lifecycle: spawn, reparent (immediate, relinks), parent/children, find.
  "life.lua": `local Life = {}
function Life.on_update(self, dt)
  if self.done then return end
  self.done = true
  local a = sa.spawn("Alpha")
  local b = sa.spawn("Beta")
  assert(b:set_parent(a), "set_parent should succeed")
  assert(b:set_parent(b) == false, "self-parent must fail")
  assert(b:parent():uuid() == a:uuid(), "b's parent is a")
  local kids = a:children()
  assert(#kids == 1, "a has one child")
  assert(kids[1]:uuid() == b:uuid(), "a's child is b")
  assert(#sa.find_all_by_name("Alpha") >= 1, "find_all_by_name finds Alpha")
  assert(sa.find_by_uuid(a:uuid()):uuid() == a:uuid(), "find_by_uuid round-trips")
end
return Life
`,
  // Deferred destroy: the handle stays valid for the rest of the handler, gone after the flush.
  "destroyer.lua": `local Destroyer = {}
function Destroyer.on_update(self, dt)
  if not self.spawned then
    self.spawned = sa.spawn("Doomed")
  elseif not self.killed then
    assert(self.spawned:valid(), "spawned must be valid")
    self.spawned:destroy()
    assert(self.spawned:valid(), "destroy is deferred; valid until flush")
    self.killed = true
  else
    assert(not self.spawned:valid(), "after flush the entity is invalid")
  end
end
return Destroyer
`,
  "waiter.lua": `local Waiter = {}
function Waiter.on_create(self)
  sa.spawn_task(function()
    sa.wait(0.5)
    self.entity:set_position(sa.vec3(42, 0, 0))
  end)
end
function Waiter.on_update(self, dt)
  sa.wait(0.1)  -- outside a coroutine: logged + ignored, never a tick error
end
return Waiter
`,
  "receiver.lua": `local Receiver = {}
function Receiver.on_update(self, dt) end
function Receiver.boom(self, sender, payload) error("msg boom") end
function Receiver.ping(self, sender, payload)
  self.entity:set_position(sa.vec3(payload or 0, 0, 0))
end
return Receiver
`,
  "sender.lua": `local Sender = {}
function Sender.on_create(self)
  sa.broadcast("boom")        -- faulting handler, contained
  sa.broadcast("ping", 7)     -- still delivered after the boom
end
function Sender.on_update(self, dt) end
return Sender
`,
  // Key edges: count only on the press edge, so a held key increments exactly once.
  "edges.lua": `local Edges = {}
function Edges.on_create(self) self.count = 0 end
function Edges.on_update(self, dt)
  if sa.is_key_pressed("e") then
    self.count = self.count + 1
    self.entity:set_position(sa.vec3(self.count, 0, 0))
  end
end
return Edges
`,
  "mouse.lua": `local Mouse = {}
function Mouse.on_update(self, dt)
  local p = sa.mouse_position()
  self.entity:set_position(sa.vec3(p.x, p.y, sa.is_mouse_down("left") and 1 or 0))
end
return Mouse
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

test("component snapshots, name(), and the rotation/scale setters work from Lua", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: cube.id, name: "Reader Cube" });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  await attachScripts(engine, cube.id, ["reader.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");

  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation).toEqual({ x: 6, y: 50, z: 3 });
  expect(during.components.Transform.rotation.x).toBeCloseTo(0.5);
  expect(during.components.Transform.scale).toEqual({ x: 2, y: 2, z: 2 });

  await engine.call("stop");
  const after = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(after.components.Transform.translation).toEqual({ x: 1, y: 2, z: 3 });
  expect(after.components.Transform.scale).toEqual({ x: 1, y: 1, z: 1 });
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a script writes components generically and the structural gate refuses cache-backed ones", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["writer.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");

  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.PointLight).toBeDefined();
  expect(during.components.PointLight.intensity).toBeCloseTo(5);
  // The structural gate held: no Rigidbody/Collider was added.
  expect(during.components.Rigidbody).toBeUndefined();
  expect(during.components.Collider).toBeUndefined();

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("sa.Vec3 operators, math, and write-through fields work", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["vectest.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation.x).toBeCloseTo(7); // p.x = 7 wrote through the userdata

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a script spawns + reparents entities (gone on stop); deferred destroy stays valid for the handler", async () => {
  const before = (await engine.call<{ entities: Ref[] }>("list-entities")).entities.length;
  const driver = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, driver.id, ["life.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = (await engine.call<{ entities: Ref[] }>("list-entities")).entities;
  expect(during.some((e) => e.name === "Alpha")).toBe(true);
  expect(during.some((e) => e.name === "Beta")).toBe(true);

  await engine.call("stop");
  // The play duplicate (with the spawns) is discarded — back to the authored count.
  const after = (await engine.call<{ entities: Ref[] }>("list-entities")).entities;
  expect(after.some((e) => e.name === "Alpha")).toBe(false);
  expect(after.length).toBe(before + 1); // only the authored driver remains
  await engine.call("destroy-entity", { entity: driver.id });

  const driver2 = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, driver2.id, ["destroyer.lua"]);
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: driver2.id });
});

test("the coroutine scheduler delays a task; sa.wait in a bare on_update is ignored", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 0, z: 0 } });
  await attachScripts(engine, cube.id, ["waiter.lua"]);

  await engine.call("play");
  await engine.settle(80); // < 0.5s of accumulated dt (even a clamped first step is 0.33)
  const early = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(early.components.Transform.translation.x).toBeCloseTo(1); // the task has NOT fired yet
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing"); // bare sa.wait didn't crash

  await engine.settle(900); // now well past 0.5s
  const late = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(late.components.Transform.translation.x).toBeCloseTo(42); // the task resumed and acted

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("broadcast reaches a handler; a faulting message handler is contained, others still run", async () => {
  const receiver = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, receiver.id, ["receiver.lua"]);
  const sender = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, sender.id, ["sender.lua"]);

  await engine.call("play");
  await engine.settle();
  // The boom handler errored (logged, contained) — play keeps playing — and ping still delivered.
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = await engine.call<Inspect>("inspect", { entity: receiver.id });
  expect(during.components.Transform.translation.x).toBeCloseTo(7); // ping payload moved the receiver

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: receiver.id });
  await engine.call("destroy-entity", { entity: sender.id });
});

test("key edges (is_key_pressed) fire once per press; mouse position + buttons reach Lua", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["edges.lua"]);

  const translationX = async (entity: string) =>
    (await engine.call<Inspect>("inspect", { entity })).components.Transform.translation.x;

  await engine.call("script-input", { keys: [] });
  await engine.call("play");
  await engine.settle(80);
  await engine.call("script-input", { keys: ["e"] }); // press
  await engine.settle(200);
  expect(await translationX(cube.id)).toBeCloseTo(1); // fired once, then false while held
  await engine.call("script-input", { keys: [] }); // release
  await engine.settle(80);
  await engine.call("script-input", { keys: ["e"] }); // press again
  await engine.settle(200);
  expect(await translationX(cube.id)).toBeCloseTo(2);
  await engine.call("stop");
  await engine.call("script-input", { keys: [] });
  await engine.call("destroy-entity", { entity: cube.id });

  const mouse = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, mouse.id, ["mouse.lua"]);
  await engine.call("script-input", { keys: [], mouseX: 3, mouseY: 4, mouseButtons: ["left"] });
  await engine.call("play");
  await engine.settle(150);
  const t = (await engine.call<Inspect>("inspect", { entity: mouse.id })).components.Transform
    .translation;
  expect(t.x).toBeCloseTo(3);
  expect(t.y).toBeCloseTo(4);
  expect(t.z).toBeCloseTo(1); // left button down
  await engine.call("stop");
  await engine.call("script-input", { keys: [], mouseButtons: [] });
  await engine.call("destroy-entity", { entity: mouse.id });
});

test("a script reaches another entity by name and moves it", async () => {
  const target = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: target.id, name: "Target" });
  const driver = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, driver.id, ["chaser.lua"]);

  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = await engine.call<Inspect>("inspect", { entity: target.id });
  expect(during.components.Transform.translation.z).toBeGreaterThan(0.05); // chased +Z by ~0.4s of dt

  await engine.call("stop");
  const after = await engine.call<Inspect>("inspect", { entity: target.id });
  expect(after.components.Transform.translation.z).toBe(0);
  await engine.call("destroy-entity", { entity: target.id });
  await engine.call("destroy-entity", { entity: driver.id });
});

test("a script moves the primary camera through its transform", async () => {
  const entities = (await engine.call<{ entities: Ref[] }>("list-entities")).entities;
  const inspected = await Promise.all(
    entities.map((entity) => engine.call<Inspect>("inspect", { entity: entity.id })),
  );
  const camera = inspected.find((entity) => entity.components.Camera?.primary === true);
  expect(camera).toBeDefined();
  const authored = camera!.components.Transform.translation;
  const driver = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(engine, driver.id, ["camera.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = await engine.call<Inspect>("inspect", { entity: camera!.id });
  expect(during.components.Transform.translation).toEqual({ x: 0, y: 5, z: 10 });

  await engine.call("stop");
  const after = await engine.call<Inspect>("inspect", { entity: camera!.id });
  expect(after.components.Transform.translation).toEqual(authored);
  await engine.call("destroy-entity", { entity: driver.id });
});

test("the script API cases leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
