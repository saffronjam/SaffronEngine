// The scripting lifecycle over the control plane: a ScriptComponent slot moves its entity only
// inside the play duplicate (the discard guarantee holds), slots on one entity run in list order
// within a tick, and a script error is contained — it lands in the drain-script-errors ring with a
// traceback, pauses play, and never crashes the host.

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
  type ScriptErrors,
  type ScriptLogs,
  type ScriptStatus,
} from "./script-utils.ts";

const SCRIPTS = {
  "move.lua": `local Mover = {}
function Mover.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(p + sa.vec3(dt, 0, 0))
end
return Mover
`,
  "player.lua": `local Player = {}
function Player.on_update(self, dt)
  if sa.is_key_down("w") then
    local p = self.entity:get_position()
    self.entity:set_position(p + sa.vec3(dt, 0, 0))
  end
end
return Player
`,
  "first.lua": `local First = {}
function First.on_update(self, dt)
  self.entity:set_position(sa.vec3(5, 0, 0))
end
return First
`,
  "second.lua": `local Second = {}
function Second.on_update(self, dt)
  local p = self.entity:get_position()
  self.entity:set_position(sa.vec3(p.x, p.x * 2, p.z))
end
return Second
`,
  "boom.lua": `local Boom = {}
function Boom.on_update(self, dt)
  error("boom")
end
return Boom
`,
  // sa.log capture: on_create fires once per instance (deterministic, no per-tick spam); the empty
  // on_update is the required method that makes the class instantiate.
  "logger.lua": `local Logger = {}
function Logger:on_create()
  sa.log("hello from " .. self.entity:name())
end
function Logger:on_update(dt) end
return Logger
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

test("a script slot moves its entity during play; stop restores the authored scene", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 1, y: 2, z: 3 } });
  await attachScripts(engine, cube.id, ["move.lua"]);

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
  await attachScripts(engine, cube.id, ["player.lua"]);

  await engine.call("script-input", { keys: ["w"] });
  await engine.call("play");
  await engine.settle(300);
  const moved = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(moved.components.Transform.translation.x).toBeGreaterThan(1.05);

  await engine.call("script-input", { keys: [] });
  const stoppedAt = (await engine.call<Inspect>("inspect", { entity: cube.id })).components
    .Transform.translation.x;
  await engine.settle(300);
  const stopped = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(stopped.components.Transform.translation.x).toBeCloseTo(stoppedAt);

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("slots on one entity run in list order within a tick", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["first.lua", "second.lua"]);

  await engine.call("play");
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(2);
  await engine.settle();

  // second.lua reads the x first.lua wrote this same tick: y == x * 2 only if slot order held.
  // Reversed order would leave y stale on every frame.
  const during = await engine.call<Inspect>("inspect", { entity: cube.id });
  expect(during.components.Transform.translation.x).toBeCloseTo(5);
  expect(during.components.Transform.translation.y).toBeCloseTo(10);

  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a script error is contained: drained with a traceback, play pauses, the host survives", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["boom.lua"]);

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
  const again = await engine.call<ScriptErrors>("drain-script-errors", {
    since: drained.highWaterSeq,
  });
  expect(again.events).toEqual([]);

  // The host is alive and play still stops cleanly.
  await engine.call("ping");
  expect((await engine.call<PlayState>("stop")).state).toBe("edit");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a missing script file is a logged skip, not a crash", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["does-not-exist.lua"]);

  await engine.call("play");
  expect((await engine.call<ScriptStatus>("get-script-status")).instances).toBe(0);
  expect((await engine.call<PlayState>("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("sa.log lands in drain-script-logs tagged with the logging entity, and does not pause play", async () => {
  const cursor = (await engine.call<ScriptLogs>("drain-script-logs", { since: 0 })).highWaterSeq;
  const robot = await engine.call<Ref>("create-entity", { name: "Robot" });
  await attachScripts(engine, robot.id, ["logger.lua"]);

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

test("the scripting lifecycle cases leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
