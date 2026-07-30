// Shared vocabulary for the Lua scripting suites: the control-plane result shapes, the
// scratch-project `src/` the test scripts are authored into, and slot attachment.

import { mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, type Cleaner } from "./test-utils.ts";

export interface Ref {
  id: string;
  name: string;
}

export interface Inspect {
  id: string;
  name: string;
  components: Record<string, any>;
}

export interface PlayState {
  state: string;
}

export interface ScriptStatus {
  state: string;
  instances: number;
  errorHighWater: number;
}

export interface ScriptErrors {
  events: { seq: number; entity: string; script: string; message: string; tick: number }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

export interface ScriptLogs {
  events: { seq: number; entity: string; message: string; epochMs: number; tick: number }[];
  highWaterSeq: number;
  oldestSeq: number;
  overflowed: boolean;
}

// Boots a scratch-project engine and authors `scripts` into its `src/`, returning both the
// engine and that directory. The auto project's root is relative to the engine's cwd.
export async function bootScriptEngine(
  cleaner: Cleaner,
  scripts: Record<string, string>,
): Promise<{ engine: Engine; srcDir: string }> {
  const engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);
  const srcDir = join(root, "src");
  mkdirSync(srcDir, { recursive: true });
  for (const [name, source] of Object.entries(scripts)) {
    writeFileSync(join(srcDir, name), source);
  }
  return { engine, srcDir };
}

// Attaches an ordered list of script slots to an entity.
export async function attachScripts(
  engine: Engine,
  entityId: string,
  paths: string[],
): Promise<void> {
  await engine.call("add-component", { entity: entityId, component: "Script" });
  await engine.call("set-component", {
    entity: entityId,
    component: "Script",
    json: { scripts: paths.map((scriptPath) => ({ scriptPath, overrides: {} })) },
  });
}

// Returns play mode to edit between cases, whatever state a failure left behind.
export async function stopIfPlaying(engine: Engine): Promise<void> {
  const state = await engine.call<PlayState>("get-play-state");
  if (state.state !== "edit") {
    await engine.call("stop");
  }
}
