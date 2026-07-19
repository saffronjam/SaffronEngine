// Per-entity Lua scripting over the control plane: a ScriptComponent slot moves its
// entity only inside the play duplicate (the discard guarantee holds), slots on one
// entity run in list order within a tick, and a script error is contained — it lands
// in the drain-script-errors ring with a traceback, pauses play, and never crashes
// the host. Test scripts are authored into the auto project's src/ on the fly.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let srcDir: string;

interface Ref {
  id: string;
  name: string;
}
interface Inspect {
  id: string;
  name: string;
  components: Record<string, any>;
}
interface PlayState {
  state: string;
}
interface ScriptStatus {
  state: string;
  instances: number;
  errorHighWater: number;
}
interface ScriptErrors {
  events: { seq: number; entity: string; script: string; message: string; tick: number }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}
interface ScriptLogs {
  events: { seq: number; entity: string; message: string; epochMs: number; tick: number }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // The auto project's root is relative to the engine's cwd (the repo root).
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);
  srcDir = join(root, "src");
  mkdirSync(srcDir, { recursive: true });

  writeFileSync(
    join(srcDir, "move.lua"),
    `local Mover = {}
function Mover.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(p + sa.vec3(dt, 0, 0))
end
return Mover
`,
  );
  writeFileSync(
    join(srcDir, "player.lua"),
    `local Player = {}
function Player.on_update(self, dt)
  if sa.is_key_down("w") then
    local p = self.entity:get_position()
    self.entity:set_position(p + sa.vec3(dt, 0, 0))
  end
end
return Player
`,
  );
  writeFileSync(
    join(srcDir, "first.lua"),
    `local First = {}
function First.on_update(self, dt)
  self.entity:set_position(sa.vec3(5, 0, 0))
end
return First
`,
  );
  writeFileSync(
    join(srcDir, "second.lua"),
    `local Second = {}
function Second.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(sa.vec3(p.x, p.x * 2, p.z))
end
return Second
`,
  );
  writeFileSync(
    join(srcDir, "boom.lua"),
    `local Boom = {}
function Boom.on_update(self, dt)
  error("boom")
end
return Boom
`,
  );
  // Lua-side asserts fail the tick, which pauses play — the tests detect API
  // breakage as state ~= "playing". Writes derive only from never-written fields
  // so every tick is idempotent.
  writeFileSync(
    join(srcDir, "reader.lua"),
    `local Reader = {}
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
  );
  writeFileSync(
    join(srcDir, "chaser.lua"),
    `local Chaser = {}
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
  );
  writeFileSync(
    join(srcDir, "camera.lua"),
    `local Cam = {}
function Cam.on_update(self, dt)
  local cam = sa.primary_camera()
  if cam:valid() then
    cam:set_position(sa.vec3(0, 5, 10))
  end
end
return Cam
`,
  );
  // Declared fields: defaults live in the .lua; the scene stores only overrides.
  // `weird` is deliberately uninferable (2 numbers, not a vec3) and must be skipped.
  writeFileSync(
    join(srcDir, "turret.lua"),
    `local Turret = {}
Turret.properties = {
  speed = 2.0,
  label = "idle",
  enabled = true,
  offset = sa.vec3(0, 1, 0),
  weird = { 1, 2 },
}
function Turret.on_update(self, dt)
  assert(self.label == "idle" or self.label == "fast", "label: " .. tostring(self.label))
  assert(type(self.enabled) == "boolean", "enabled must be a bool")
  assert(self.offset.y == 1, "offset must inject as an sa.Vec3")
  if self.enabled then
    local p = self.entity:get_position()
    self.entity:set_position(p + sa.vec3(self.speed * dt, 0, 0))
  end
end
return Turret
`,
  );
  // Generic component write via the registry's deserialize, plus the structural-component gate.
  writeFileSync(
    join(srcDir, "writer.lua"),
    `local Writer = {}
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
  );
  // sa.Vec3 operators + math + write-through fields.
  writeFileSync(
    join(srcDir, "vectest.lua"),
    `local VecTest = {}
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
  );
  // Entity lifecycle: spawn, reparent (immediate, relinks), parent/children, find.
  writeFileSync(
    join(srcDir, "life.lua"),
    `local Life = {}
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
  );
  // Deferred destroy: the handle stays valid for the rest of the handler, gone after the flush.
  writeFileSync(
    join(srcDir, "destroyer.lua"),
    `local Destroyer = {}
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
  );
  // Coroutine scheduler: a task waits, then acts; sa.wait in a bare on_update is ignored (no crash).
  writeFileSync(
    join(srcDir, "waiter.lua"),
    `local Waiter = {}
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
  );
  // Messaging: a broadcast reaches a handler; a faulting handler is contained, others still run.
  writeFileSync(
    join(srcDir, "receiver.lua"),
    `local Receiver = {}
function Receiver.on_update(self, dt) end
function Receiver.boom(self, sender, payload) error("msg boom") end
function Receiver.ping(self, sender, payload)
  self.entity:set_position(sa.vec3(payload or 0, 0, 0))
end
return Receiver
`,
  );
  writeFileSync(
    join(srcDir, "sender.lua"),
    `local Sender = {}
function Sender.on_create(self)
  sa.broadcast("boom")        -- faulting handler, contained
  sa.broadcast("ping", 7)     -- still delivered after the boom
end
function Sender.on_update(self, dt) end
return Sender
`,
  );
  // Key edges: count only on the press edge, so a held key increments exactly once.
  writeFileSync(
    join(srcDir, "edges.lua"),
    `local Edges = {}
function Edges.on_create(self) self.count = 0 end
function Edges.on_update(self, dt)
  if sa.is_key_pressed("e") then
    self.count = self.count + 1
    self.entity:set_position(sa.vec3(self.count, 0, 0))
  end
end
return Edges
`,
  );
  // Mouse: position + held left button drive a derived transform.
  writeFileSync(
    join(srcDir, "mouse.lua"),
    `local Mouse = {}
function Mouse.on_update(self, dt)
  local p = sa.mouse_position()
  self.entity:set_position(sa.vec3(p.x, p.y, sa.is_mouse_down("left") and 1 or 0))
end
return Mouse
`,
  );
  // Physics bridges: impulse a dynamic body, drive a character, spherecast the world.
  writeFileSync(
    join(srcDir, "pusher.lua"),
    `local Pusher = {}
function Pusher.on_update(self, dt)
  if not self.pushed then self.pushed = true self.entity:apply_impulse(sa.vec3(0, 0, 12)) end
end
return Pusher
`,
  );
  writeFileSync(
    join(srcDir, "walker.lua"),
    `local Walker = {}
function Walker.on_update(self, dt) self.entity:move_character(sa.vec3(3, 0, 0), false) end
return Walker
`,
  );
  writeFileSync(
    join(srcDir, "caster.lua"),
    `local Caster = {}
function Caster.on_update(self, dt)
  if not self.done then
    self.done = true
    local hit = sa.spherecast(0, 5, 0, 0, -1, 0, 0.5, 20)
    if hit.hit then self.entity:set_position(sa.vec3(1, hit.point.y, 0)) end
  end
end
return Caster
`,
  );
  // sa.log capture: on_create fires once per instance (deterministic, no per-tick spam); the empty
  // on_update is the required method that makes the class instantiate.
  writeFileSync(
    join(srcDir, "logger.lua"),
    `local Logger = {}
function Logger:on_create()
  sa.log("hello from " .. self.entity:name())
end
function Logger:on_update(dt) end
return Logger
`,
  );
});

afterEach(async () => {
  const state = await engine.call<PlayState>("get-play-state");
  if (state.state !== "edit") {
    await engine.call("stop");
  }
});
afterAll(async () => {
  await engine?.shutdown();
});

async function attachScripts(entityId: string, paths: string[]): Promise<void> {
  await engine.call("add-component", { entity: entityId, component: "Script" });
  await engine.call("set-component", {
    entity: entityId,
    component: "Script",
    json: { scripts: paths.map((scriptPath) => ({ scriptPath, overrides: {} })) },
  });
}

test("a script slot moves its entity during play; stop restores the authored scene", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  await attachScripts(cube.id, ["move.lua"]);

  // The slot list is authored data, visible in the inspector wire shape.
  const authored = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(authored.components.Script.scripts).toEqual([{ scriptPath: "move.lua", overrides: {} }]);

  await engine.call("play");
  const status = await engine.call<ScriptStatus>("get-script-status");
  expect(status.state).toBe("playing");
  expect(status.instances).toBe(1);

  await engine.settle(400);
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation.x).toBeGreaterThan(1.05); // drifted +X by ~0.4s of dt
  expect(during.components.Transform.translation.y).toBeCloseTo(2);

  await engine.call("stop");
  const after = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(after.components.Transform.translation).toEqual({ x: 1, y: 2, z: 3 }); // the discard is the restore
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(0);
  await engine.call("destroy-entity", { entity: cube.id });
});

test("script input exposes held keys to Lua", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  await attachScripts(cube.id, ["player.lua"]);

  await engine.call("script-input", { keys: ["w"] });
  await engine.call("play");
  await engine.settle(300);
  const moved = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(moved.components.Transform.translation.x).toBeGreaterThan(1.05);

  await engine.call("script-input", { keys: [] });
  const stoppedAt = (await engine.call<Inspect>("inspect", { entity: cube.id })).components.Transform.translation.x;
  await engine.settle(300);
  const stopped = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(stopped.components.Transform.translation.x).toBeCloseTo(stoppedAt);

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("slots on one entity run in list order within a tick", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["first.lua", "second.lua"]);

  await engine.call("play");
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(2);
  await engine.settle();

  // second.lua reads the x first.lua wrote this same tick: y == x * 2 only if
  // slot order held. Reversed order would leave y stale on every frame.
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation.x).toBeCloseTo(5);
  expect(during.components.Transform.translation.y).toBeCloseTo(10);

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a script error is contained: drained with a traceback, play pauses, the host survives", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["boom.lua"]);

  await engine.call("play");
  await engine.settle();

  expect((await engine.call<PlayState>("get-play-state")).state).toBe("paused");

  const drained = await engine.call<ScriptErrors>("drain-script-errors", { since: 0 });
  expect(drained.events.length).toBeGreaterThan(0);
  const event = drained.events[0]!;
  expect(event.script).toBe("boom.lua");
  expect(event.message).toContain("boom");
  expect(event.message).toContain("stack traceback");
  expect(event.entity).toBe(cube.id);
  expect(drained.highWaterSeq).toBeGreaterThanOrEqual(event.seq);

  // The cursor protocol: draining from the high-water returns nothing new.
  const again = await engine.call<ScriptErrors>("drain-script-errors", { since: drained.highWaterSeq });
  expect(again.events).toEqual([]);

  // The host is alive and play still stops cleanly.
  await engine.call("ping");
  expect((await engine.call<PlayState>("stop")).state).toBe("edit");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("component snapshots, name(), and the rotation/scale setters work from Lua", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: cube.id, name: "Reader Cube" });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  await attachScripts(cube.id, ["reader.lua"]);

  await engine.call("play");
  await engine.settle();
  // Any failed Lua assert pauses play, so "playing" means the whole API behaved.
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
  await attachScripts(cube.id, ["writer.lua"]);

  await engine.call("play");
  await engine.settle();
  // Every Lua assert (the gate refusals, has_component) passing means state stays "playing".
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
  await attachScripts(cube.id, ["vectest.lua"]);

  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing"); // all Vec3 asserts held
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation.x).toBeCloseTo(7); // p.x = 7 wrote through the userdata

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a script spawns + reparents entities (gone on stop); deferred destroy stays valid for the handler", async () => {
  const before = (await engine.call<{ entities: Ref[] }>("list-entities")).entities.length;
  const driver = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(driver.id, ["life.lua"]);

  await engine.call("play");
  await engine.settle();
  // Every Lua assert (reparent, parent/children, find) passing keeps play "playing".
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

  // Deferred destroy: a spawned entity stays valid until the tick flush, invalid after.
  const driver2 = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(driver2.id, ["destroyer.lua"]);
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: driver2.id });
});

test("the coroutine scheduler delays a task; sa.wait in a bare on_update is ignored", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 0, z: 0 } });
  await attachScripts(cube.id, ["waiter.lua"]);

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
  await attachScripts(receiver.id, ["receiver.lua"]);
  const sender = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(sender.id, ["sender.lua"]);

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
  await attachScripts(cube.id, ["edges.lua"]);

  await engine.call("script-input", { keys: [] });
  await engine.call("play");
  await engine.settle(80);
  await engine.call("script-input", { keys: ["e"] }); // press
  await engine.settle(200);
  expect((await engine.call<Inspect>("inspect", { entity: cube.id })).components.Transform.translation.x).toBeCloseTo(
    1,
  ); // is_key_pressed fired once, then false while held
  await engine.call("script-input", { keys: [] }); // release
  await engine.settle(80);
  await engine.call("script-input", { keys: ["e"] }); // press again
  await engine.settle(200);
  expect((await engine.call<Inspect>("inspect", { entity: cube.id })).components.Transform.translation.x).toBeCloseTo(
    2,
  );
  await engine.call("stop");
  await engine.call("script-input", { keys: [] });
  await engine.call("destroy-entity", { entity: cube.id });

  // Mouse: position + left button feed a derived transform.
  const m = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(m.id, ["mouse.lua"]);
  await engine.call("script-input", { keys: [], mouseX: 3, mouseY: 4, mouseButtons: ["left"] });
  await engine.call("play");
  await engine.settle(150);
  const t = (await engine.call<Inspect>("inspect", { entity: m.id })).components.Transform.translation;
  expect(t.x).toBeCloseTo(3);
  expect(t.y).toBeCloseTo(4);
  expect(t.z).toBeCloseTo(1); // left button down
  await engine.call("stop");
  await engine.call("script-input", { keys: [], mouseButtons: [] });
  await engine.call("destroy-entity", { entity: m.id });
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
  await attachScripts(box.id, ["pusher.lua"]);

  // A capsule character walked +X by move_character from Lua.
  const char = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await engine.call("set-transform", { entity: char.id, translation: { x: 0, y: 1, z: 5 } });
  await engine.call("add-component", { entity: char.id, component: "Collider" });
  await engine.call("set-component-field", {
    entity: char.id,
    component: "Collider",
    field: "shape",
    value: "capsule",
  });
  await engine.call("add-component", { entity: char.id, component: "CharacterController" });
  await attachScripts(char.id, ["walker.lua"]);

  // A probe that spherecasts down onto the floor.
  const probe = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(probe.id, ["caster.lua"]);

  await engine.call("play");
  await engine.settle(900);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");

  expect((await engine.call<Inspect>("inspect", { entity: box.id })).components.Transform.translation.z).toBeGreaterThan(
    0.5,
  ); // the impulse pushed it +Z
  expect(
    (await engine.call<Inspect>("inspect", { entity: char.id })).components.Transform.translation.x,
  ).toBeGreaterThan(0.3); // move_character walked it +X
  expect((await engine.call<Inspect>("inspect", { entity: probe.id })).components.Transform.translation.x).toBeCloseTo(
    1,
  ); // spherecast hit the floor

  await engine.call("stop");
  for (const e of [floor, box, char, probe]) {
    await engine.call("destroy-entity", { entity: e.id });
  }
});

test("a script reaches another entity by name and moves it", async () => {
  const target = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: target.id, name: "Target" });
  const driver = await engine.call<Ref>("add-entity", { args: ["empty"] });
  await attachScripts(driver.id, ["chaser.lua"]);

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
  await attachScripts(driver.id, ["camera.lua"]);

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

interface ScriptSchema {
  fields: { name: string; type: string; defaultValue: unknown }[];
}

test("get-script-schema reads declared fields with inferred types, sorted by name", async () => {
  const schema = await engine.call<ScriptSchema>("get-script-schema", { path: "turret.lua" });
  expect(schema.fields).toEqual([
    { name: "enabled", type: "bool", defaultValue: true },
    { name: "label", type: "string", defaultValue: "idle" },
    { name: "offset", type: "vec3", defaultValue: [0, 1, 0] },
    { name: "speed", type: "number", defaultValue: 2 },
  ]); // `weird` (a 2-number table) is skipped, not an error

  await expect(engine.call("get-script-schema", { path: "does-not-exist.lua" })).rejects.toThrow();
  await expect(engine.call("get-script-schema", { path: "../escape.lua" })).rejects.toThrow(/relative/);
});

test("declared defaults drive the script; an override on the slot wins", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["turret.lua"]);

  // No overrides: the turret moves at the declared default (speed = 2).
  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing"); // no Lua assert tripped
  const defaultRun = await engine.call<Inspect>("inspect", { entity: cube.id });
  const defaultX = defaultRun.components.Transform.translation.x;
  expect(defaultX).toBeGreaterThan(0.4);
  await engine.call("stop");

  // Override speed and label on the authored slot; the next session reads them.
  const written = await engine.call<{ scriptPath: string; overrides: Record<string, unknown> }>(
    "set-script-override",
    { entity: cube.id, slot: 0, name: "speed", value: 10 },
  );
  expect(written.overrides).toEqual({ speed: 10 });
  await engine.call("set-script-override", { entity: cube.id, slot: 0, name: "label", value: "fast" });

  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const overriddenX = (await engine.call<Inspect>("inspect", { entity: cube.id })).components.Transform
    .translation.x;
  await engine.call("stop");
  expect(overriddenX).toBeGreaterThan(defaultX * 2); // 5x the rate, generous margin

  // A null value clears the override; a stale key (renamed/removed field) is
  // ignored at injection, never an error.
  const cleared = await engine.call<{ overrides: Record<string, unknown> }>("set-script-override", {
    entity: cube.id,
    slot: 0,
    name: "speed",
    value: null,
  });
  expect(cleared.overrides).toEqual({ label: "fast" });
  await engine.call("set-script-override", { entity: cube.id, slot: 0, name: "renamed_away", value: 99 });
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a new project scaffolds src/ with a runnable starter script", async () => {
  // createProject (which the scratch boot rides) ensures src/ + example.lua.
  const example = join(srcDir, "example.lua");
  expect(existsSync(example)).toBe(true);
  const text = readFileSync(example, "utf8");
  expect(text).toContain("Example.properties");
  expect(text).toContain("on_update");

  // The starter is immediately demonstrable: attach, play, and it orbits the
  // authored spot in the x/y plane. The angle depends on wall-clock timing, but
  // the orbit invariant doesn't: the cube stays `radius` from the circle's
  // center (one radius left of the authored position) at all times.
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["example.lua"]);
  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  const p = during.components.Transform.translation;
  expect(p.y).toBeGreaterThan(0.05); // ~sin(0.4s * speed) * radius, well off the start
  const radius = Math.hypot(p.x - -2, p.y - 0); // center = authored (0,0) - (radius, 0)
  expect(radius).toBeCloseTo(2, 1);
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("create-script writes a runnable class-table boilerplate and rejects duplicates", async () => {
  const created = await engine.call<{ path: string }>("create-script", { name: "spawner" });
  expect(created.path).toBe("spawner.lua"); // .lua appended
  const text = readFileSync(join(srcDir, "spawner.lua"), "utf8");
  expect(text).toContain("local Spawner = {}");
  expect(text).toContain("Spawner.properties");
  expect(text).toContain("function Spawner.on_update(self, dt)");

  await expect(engine.call("create-script", { name: "spawner.lua" })).rejects.toThrow(/exists/);
  await expect(engine.call("create-script", { name: "../escape" })).rejects.toThrow(/invalid/);

  // The boilerplate is valid as written: attach + play stays clean.
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["spawner.lua"]);
  await engine.call("play");
  await engine.settle();
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(1);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a missing script file is a logged skip, not a crash", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(cube.id, ["does-not-exist.lua"]);

  await engine.call("play");
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(0);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

// sa.log(...) lands in the drain-script-logs ring tagged with the logging entity, drains via a seq
// cursor (like drain-script-errors), and — unlike an error — never pauses play.
test("sa.log lands in drain-script-logs tagged with the logging entity, and does not pause play", async () => {
  const cursor = (await engine.call<ScriptLogs>("drain-script-logs", { since: 0 })).highWaterSeq;
  const robot = await engine.call<Ref>("create-entity", { name: "Robot" });
  await engine.call("add-component", { entity: robot.id, component: "Script" });
  await engine.call("set-component", {
    entity: robot.id,
    component: "Script",
    json: { scripts: [{ scriptPath: "logger.lua", overrides: {} }] },
  });

  await engine.call("play");
  await engine.settle();

  const drained = await engine.call<ScriptLogs>("drain-script-logs", { since: cursor });
  const line = drained.events.find((e) => e.message.includes("hello from Robot"));
  expect(line).toBeDefined();
  expect(line!.entity).toBe(robot.id); // tagged with the logging entity (currentSenderUuid)
  expect(line!.epochMs).toBeGreaterThan(0);
  expect(drained.highWaterSeq).toBeGreaterThanOrEqual(line!.seq);
  expect(drained.overflowed).toBe(false);

  // A plain log must NOT pause play (that is the error path's behaviour).
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");

  // The cursor is exhausted: draining from the high-water mark returns nothing new.
  const again = await engine.call<ScriptLogs>("drain-script-logs", { since: drained.highWaterSeq });
  expect(again.events).toEqual([]);

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: robot.id });
});

test("the scripting cases leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
