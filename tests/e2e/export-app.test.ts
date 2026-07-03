// App export over the control plane: `export-app` cooks the loaded project into a standalone
// folder (the player binary + project data + engine shaders + an `app.json` manifest), and the
// exported `saffron-player` boots that folder on its own. This drives the whole Phase 4 pipeline
// against a real headless engine, then runs the staged player headless-offscreen and asserts a
// validation-clean run — the proof that an exported app actually runs without the editor.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { Engine, ENGINE_BIN } from "./harness.ts";

let engine: Engine;
let outDir: string | undefined;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
  if (outDir) {
    rmSync(outDir, { recursive: true, force: true });
  }
});

interface ExportResult {
  path: string;
  warnings: string[];
}

test("export-app stages a runnable app folder that the player boots clean", async () => {
  outDir = mkdtempSync(join(tmpdir(), "saffron-export-"));
  const app = { title: "E2E App", width: 800, height: 600, fullscreen: false, vsync: true };

  const result = await engine.call<ExportResult>("export-app", { outputDir: outDir, app });
  expect(result.path).toBe(outDir);

  // The staged folder is complete: the player binary, the manifest, the project, the data dirs,
  // and the bundled C++ runtime libs (so the folder runs on a host without the toolbox's libc++).
  for (const file of ["saffron-player", "app.json", "project.json", "libc++.so.1", "libc++abi.so.1"]) {
    expect(existsSync(join(outDir, file)), `staged ${file}`).toBe(true);
  }
  expect(statSync(join(outDir, "assets")).isDirectory(), "staged assets/").toBe(true);
  expect(statSync(join(outDir, "shaders")).isDirectory(), "staged shaders/").toBe(true);

  // app.json round-trips the manifest the editor passed.
  const manifest = JSON.parse(readFileSync(join(outDir, "app.json"), "utf8"));
  expect(manifest.title).toBe("E2E App");
  expect(manifest.width).toBe(800);
  expect(manifest.height).toBe(600);

  // The exported player boots the staged folder headless-offscreen for a few frames, loading the
  // project and running a validation-clean frame loop — no editor, no control plane.
  const player = join(dirname(ENGINE_BIN), "saffron-player");
  const run = spawnSync(player, [], {
    env: {
      ...process.env,
      SAFFRON_PROJECT: outDir,
      SAFFRON_EDITOR_NATIVE_VIEWPORT: "1",
      SAFFRON_EXIT_AFTER_FRAMES: "8",
    },
    encoding: "utf8",
    timeout: 90_000,
  });
  const log = `${run.stdout ?? ""}${run.stderr ?? ""}`;
  expect(run.status, `player exit (log below)\n${log}`).toBe(0);
  expect(log, "player loaded the staged project").toContain("loaded project");
  expect(
    log.split("\n").filter((l) => /ERROR\s+vulkan\s+\[validation\]/.test(l)),
    "the staged player runs validation-clean",
  ).toEqual([]);
}, 120_000);
