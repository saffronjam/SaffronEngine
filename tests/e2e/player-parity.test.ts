// The exported game renders what the editor renders.
//
// `export-app` packages the scene, its cooked closure, and a copy of `saffron-player`. The export
// tests assert on the files it stages, which says the packaging is right and nothing about whether
// the result draws the same picture. This boots the packaged binary and compares its frame against
// the host's, pixel for pixel.
//
// The comparison must be against play mode: the player renders through the scene's primary camera,
// while the host in edit mode renders through the editor camera, a different pose entirely.
// Comparing against an edit-mode frame measures the distance between two cameras, so the control
// assertion scores that frame precisely and the mistake cannot be made silently.
//
// `SAFFRON_CAPTURE_FRAME` is the player's only output seam — it has no control plane to ask for a
// screenshot — and pairs with `SAFFRON_EXIT_AFTER_FRAMES` to leave exactly one deterministic image
// behind.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene, trackEntity } from "./test-utils.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";

const IS_MACOS = process.platform === "darwin";

// The render size both sides use: the exported app's window size and the host's scene view.
const WIDTH = 640;
const HEIGHT = 360;

// Mean absolute per-channel difference (0-255) allowed between the player's frame and the host's.
// The two run the same passes over the same scene through the same camera, so the measured value
// is 0.0002 — 121 differing bytes in 691,200, all of them one-step rounding along the cube's
// silhouette. The budget covers that quantization, not a render difference; the edit-mode control
// below scores 11.7 for scale.
const PARITY_TOLERANCE = 1.0;

// The editor camera, deliberately away from the scene camera below so the control assertion has a
// real difference to score.
const EDITOR_CAMERA = { position: { x: 0, y: 6, z: 12 }, yaw: 0, pitch: -22 };

const cleaner = new Cleaner();
let engine: Engine;
let playerBinary = "";

// The packaged executable: nested inside the bundle on macOS, at the export root elsewhere.
function locatePlayer(root: string): string {
  if (!IS_MACOS) {
    return join(root, "saffron-player");
  }
  const macos = join(root, "Contents", "MacOS");
  const entries = readdirSync(macos);
  const name = entries[0];
  if (entries.length !== 1 || name === undefined) {
    throw new Error(`expected one executable in ${macos}, found ${entries.length}`);
  }
  return join(macos, name);
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, { width: WIDTH, height: HEIGHT, camera: EDITOR_CAMERA });

  // Something to render, and a scene camera for the player to render it through.
  trackEntity(cleaner, engine, await engine.call("add-entity", { preset: "plane" }));
  const cube = trackEntity(
    cleaner,
    engine,
    await engine.call("add-entity", { preset: "cube" }),
  );
  await engine.call("set-component", {
    entity: cube.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 1, z: 0 },
      scale: { x: 1.5, y: 1.5, z: 1.5 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  const camera = trackEntity(
    cleaner,
    engine,
    await engine.call("add-entity", { preset: "camera" }),
  );
  await engine.call("set-component", {
    entity: camera.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 3, z: 7 },
      scale: { x: 1, y: 1, z: 1 },
      rotation: { x: -0.12, y: 0, z: 0 },
    },
  });
  await engine.settle(600);

  const output = mkdtempSync(join(tmpdir(), "saffron-parity-"));
  cleaner.defer(() => rmSync(output, { recursive: true, force: true }));
  // `export-app` copies `project.json` off disk, so the entities added above must be committed
  // before the package is staged — otherwise the player boots the starter scene and the
  // comparison is between two pictures of nothing.
  await engine.call("save-project", {});
  const exported = await engine.call("export-app", {
    outputDir: join(output, "Parity"),
    app: { title: "Parity", width: WIDTH, height: HEIGHT, vsync: false, fullscreen: false },
  });
  playerBinary = locatePlayer(exported.path);
});

afterAll(async () => {
  await cleaner.cleanup();
});

test(
  "the exported player's frame matches the host's play-mode frame",
  async () => {
    expect(existsSync(playerBinary)).toBe(true);

    // The editor-camera frame, captured before play so it is the pose `prepareScene` set.
    const editFrame = decodeRgb8Png(await captureViewport(engine, cleaner, "parity-edit"));

    // Play switches the host to the scene's primary camera — the same one the player uses.
    await engine.call("play");
    await engine.settle(1200);
    const playFrame = decodeRgb8Png(await captureViewport(engine, cleaner, "parity-play"));

    const capture = join(tmpdir(), `saffron-parity-${process.pid}.png`);
    cleaner.defer(() => rmSync(capture, { force: true }));
    rmSync(capture, { force: true });
    const run = spawnSync(playerBinary, [], {
      cwd: playerBinary.replace(/\/[^/]+$/, ""),
      env: {
        ...process.env,
        SAFFRON_EDITOR_NATIVE_VIEWPORT: "1",
        SAFFRON_EXIT_AFTER_FRAMES: "48",
        SAFFRON_CAPTURE_FRAME: capture,
      },
      encoding: "utf8",
      timeout: 120_000,
    });
    // A packaged player must exit cleanly. Teardown is where it historically did not: the
    // GPU-scene mirror's retained handles kept the device alive into `vkDestroyInstance`, which
    // faulted inside the driver, so a nonzero status here is a real regression and not noise.
    expect(run.status).toBe(0);
    expect(existsSync(capture)).toBe(true);

    const playerFrame = decodeRgb8Png(readFileSync(capture));
    const parity = meanAbsoluteDifference(playerFrame, playFrame);

    // Prove the metric discriminates before trusting it: the edit-mode frame is the same scene
    // through the editor camera, and it must score far above the parity budget. Without this the
    // parity assertion could pass against a metric blind to everything.
    const control = meanAbsoluteDifference(playerFrame, editFrame);
    expect(control).toBeGreaterThan(PARITY_TOLERANCE * 3);

    expect(parity).toBeLessThan(PARITY_TOLERANCE);
    expect(engine.validationErrors()).toEqual([]);
  },
  180_000,
);
