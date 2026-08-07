// App export over the control plane: `export-app` cooks the loaded project into a standalone
// platform-native application (a macOS bundle or Linux folder), and the exported `saffron-player`
// boots its staged project on its own. The staged player runs headless-offscreen and must exit with
// a validation-clean log.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { Engine } from "./harness.ts";

let engine: Engine;
let scratchRoot: string | undefined;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
  if (scratchRoot) {
    rmSync(scratchRoot, { recursive: true, force: true });
  }
});

test("export-app stages a runnable app folder that the player boots clean", async () => {
  scratchRoot = mkdtempSync(join(tmpdir(), "saffron-export-"));
  const outputDir = join(scratchRoot, "E2E App");
  const appRoot = process.platform === "darwin" ? `${outputDir}.app` : outputDir;
  const app = { title: "E2E App", width: 800, height: 600, fullscreen: false, vsync: true };

  const result = await engine.call("export-app", { outputDir, app });
  expect(result.path).toBe(appRoot);
  expect(result.warnings).toEqual([]);

  const resources =
    process.platform === "darwin" ? join(appRoot, "Contents", "Resources") : appRoot;
  const player =
    process.platform === "darwin"
      ? join(appRoot, "Contents", "MacOS", "saffron-player")
      : join(appRoot, "saffron-player");

  for (const file of [player, join(resources, "app.json"), join(resources, "project.json")]) {
    expect(existsSync(file), `staged ${file}`).toBe(true);
  }
  expect(statSync(join(resources, "assets")).isDirectory(), "staged assets/").toBe(true);
  expect(statSync(join(resources, "shaders")).isDirectory(), "staged shaders/").toBe(true);

  if (process.platform === "darwin") {
    for (const file of [
      join(appRoot, "Contents", "Info.plist"),
      join(appRoot, "Contents", "Frameworks", "libMoltenVK.dylib"),
      join(resources, "licenses", "MoltenVK-LICENSE.txt"),
    ]) {
      expect(existsSync(file), `staged ${file}`).toBe(true);
    }
  } else {
    for (const file of ["libc++.so.1", "libc++abi.so.1"]) {
      expect(existsSync(join(appRoot, file)), `staged ${file}`).toBe(true);
    }
  }

  // app.json round-trips the manifest the editor passed.
  const manifest = JSON.parse(readFileSync(join(resources, "app.json"), "utf8"));
  expect(manifest.title).toBe("E2E App");
  expect(manifest.width).toBe(800);
  expect(manifest.height).toBe(600);

  // The exported player boots the staged folder headless-offscreen for a few frames, loading the
  // project and running a validation-clean frame loop — no editor, no control plane.
  const runEnv: Record<string, string | undefined> = {
    ...process.env,
    SAFFRON_EDITOR_NATIVE_VIEWPORT: "1",
    SAFFRON_EXIT_AFTER_FRAMES: "8",
  };
  delete runEnv.SAFFRON_PROJECT;
  if (process.platform === "darwin") {
    delete runEnv.VK_DRIVER_FILES;
    delete runEnv.VK_ICD_FILENAMES;
  }
  const run = spawnSync(player, [], {
    env: runEnv,
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
