// Shared vocabulary for the Lua scripting suites: the scratch-project `src/` the test scripts are
// authored into, and slot attachment.

import { mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, type Cleaner } from "./test-utils.ts";

// Boots a scratch-project engine and authors `scripts` into its `src/`, returning both the
// engine and that directory. The auto project's root is relative to the engine's cwd.
export async function bootScriptEngine(
  cleaner: Cleaner,
  scripts: Record<string, string>,
): Promise<{ engine: Engine; srcDir: string }> {
  const engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  const project = await engine.call("get-project");
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
  const state = await engine.call("get-play-state");
  if (state.state !== "edit") {
    await engine.call("stop");
  }
}
