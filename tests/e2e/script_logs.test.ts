// sa.log capture: a script's sa.log(...) lands in the drain-script-logs ring tagged with the logging
// entity, drains via a seq cursor (like drain-script-errors), and — unlike an error — never pauses play.
// The line is authored into the auto project's src/ on the fly.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { isAbsolute, join } from "node:path";
import { mkdirSync, writeFileSync } from "node:fs";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let srcDir: string;

interface Ref {
  id: string;
  name: string;
}
interface PlayState {
  state: string;
}
interface ScriptLogs {
  events: { seq: number; entity: string; message: string; epochMs: number; tick: number }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);
  srcDir = join(root, "src");
  mkdirSync(srcDir, { recursive: true });

  // on_create fires once per instance (deterministic, no per-tick spam); the empty on_update is the
  // required method that makes the class instantiate (a class without on_update is not a valid script).
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
afterAll(async () => {
  await engine?.shutdown();
});

test("sa.log lands in drain-script-logs tagged with the logging entity, and does not pause play", async () => {
  const robot = await engine.call<Ref>("create-entity", { name: "Robot" });
  await engine.call("add-component", { entity: robot.id, component: "Script" });
  await engine.call("set-component", {
    entity: robot.id,
    component: "Script",
    json: { scripts: [{ scriptPath: "logger.lua", overrides: {} }] },
  });

  await engine.call("play");
  await engine.settle();

  const drained = await engine.call<ScriptLogs>("drain-script-logs", { since: 0 });
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
});

test("the script-logs run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
