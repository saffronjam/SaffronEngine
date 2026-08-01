// Script authoring over the control plane: declared fields read back as a typed schema, defaults
// and slot overrides driving a run, the `src/` scaffold a fresh project ships, and `create-script`.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Engine } from "./harness.ts";
import { Cleaner } from "./test-utils.ts";
import {
  attachScripts,
  bootScriptEngine,
  stopIfPlaying,
} from "./script-utils.ts";

// Declared fields: defaults live in the .lua; the scene stores only overrides. `weird` is
// deliberately uninferable (2 numbers, not a vec3) and must be skipped.
const SCRIPTS = {
  "turret.lua": `local Turret = {}
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
};

const cleaner = new Cleaner();
let engine: Engine;
let srcDir: string;

beforeAll(async () => {
  ({ engine, srcDir } = await bootScriptEngine(cleaner, SCRIPTS));
});

afterEach(async () => {
  await stopIfPlaying(engine);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("get-script-schema reads declared fields with inferred types, sorted by name", async () => {
  const schema = await engine.call("get-script-schema", { path: "turret.lua" });
  expect(schema.fields).toEqual([
    { name: "enabled", type: "bool", defaultValue: true },
    { name: "label", type: "string", defaultValue: "idle" },
    { name: "offset", type: "vec3", defaultValue: [0, 1, 0] },
    { name: "speed", type: "number", defaultValue: 2 },
  ]); // `weird` (a 2-number table) is skipped, not an error

  await expect(engine.call("get-script-schema", { path: "does-not-exist.lua" })).rejects.toThrow();
  await expect(engine.call("get-script-schema", { path: "../escape.lua" })).rejects.toThrow(
    /relative/,
  );
});

test("declared defaults drive the script; an override on the slot wins", async () => {
  const cube = await engine.call("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["turret.lua"]);

  // No overrides: the turret moves at the declared default (speed = 2).
  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call("get-play-state")).state).toBe("playing");
  const defaultRun = await engine.call("inspect", { entity: cube.id });
  const defaultX = defaultRun.components.Transform!.translation.x;
  expect(defaultX).toBeGreaterThan(0.4);
  await engine.call("stop");

  // Override speed and label on the authored slot; the next session reads them.
  const written = await engine.call("set-script-override", {
    entity: cube.id,
    slot: 0,
    name: "speed",
    value: 10,
  });
  expect(written.overrides).toEqual({ speed: 10 });
  await engine.call("set-script-override", {
    entity: cube.id,
    slot: 0,
    name: "label",
    value: "fast",
  });

  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call("get-play-state")).state).toBe("playing");
  const overriddenX = (await engine.call("inspect", { entity: cube.id })).components
    .Transform!.translation.x;
  await engine.call("stop");
  expect(overriddenX).toBeGreaterThan(defaultX * 2); // 5x the rate, generous margin

  // A null value clears the override; a stale key (renamed/removed field) is ignored at
  // injection, never an error.
  const cleared = await engine.call("set-script-override", {
    entity: cube.id,
    slot: 0,
    name: "speed",
    value: null,
  });
  expect(cleared.overrides).toEqual({ label: "fast" });
  await engine.call("set-script-override", {
    entity: cube.id,
    slot: 0,
    name: "renamed_away",
    value: 99,
  });
  await engine.call("play");
  await engine.settle();
  expect((await engine.call("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("a new project scaffolds src/ with a runnable starter script", async () => {
  const example = join(srcDir, "example.lua");
  expect(existsSync(example)).toBe(true);
  const text = readFileSync(example, "utf8");
  expect(text).toContain("Example.properties");
  expect(text).toContain("on_update");

  // The starter is immediately demonstrable: attach, play, and it orbits the authored spot in the
  // x/y plane. The angle depends on wall-clock timing, but the orbit invariant does not: the cube
  // stays `radius` from the circle's center (one radius left of the authored position) at all times.
  const cube = await engine.call("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["example.lua"]);
  await engine.call("play");
  await engine.settle(400);
  expect((await engine.call("get-play-state")).state).toBe("playing");
  const during = await engine.call("inspect", { entity: cube.id });
  const p = during.components.Transform!.translation;
  expect(p.y).toBeGreaterThan(0.05); // ~sin(0.4s * speed) * radius, well off the start
  const radius = Math.hypot(p.x - -2, p.y - 0); // center = authored (0,0) - (radius, 0)
  expect(radius).toBeCloseTo(2, 1);
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("create-script writes a runnable class-table boilerplate and rejects duplicates", async () => {
  const created = await engine.call("create-script", { name: "spawner" });
  expect(created.path).toBe("spawner.lua"); // .lua appended
  const text = readFileSync(join(srcDir, "spawner.lua"), "utf8");
  expect(text).toContain("local Spawner = {}");
  expect(text).toContain("Spawner.properties");
  expect(text).toContain("function Spawner.on_update(self, dt)");

  await expect(engine.call("create-script", { name: "spawner.lua" })).rejects.toThrow(/exists/);
  await expect(engine.call("create-script", { name: "../escape" })).rejects.toThrow(/invalid/);

  // The boilerplate is valid as written: attach + play stays clean.
  const cube = await engine.call("add-entity", { args: ["cube"] });
  await attachScripts(engine, cube.id, ["spawner.lua"]);
  await engine.call("play");
  await engine.settle();
  expect((await engine.call("get-script-status")).instances).toBe(1);
  expect((await engine.call("get-play-state")).state).toBe("playing");
  await engine.call("stop");
  await engine.call("destroy-entity", { entity: cube.id });
});

test("the script authoring cases leave the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
