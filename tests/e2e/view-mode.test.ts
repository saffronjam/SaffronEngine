// View mode: the debug render-output selector (set-view-mode {lit|unlit|wireframe|lit-wireframe|
// detail-lighting|lighting-only|reflections|albedo|normal|roughness|metallic|emissive|depth|
// ambient-occlusion|gi|light-complexity|motion-vectors}). Drives the control command (echo), proves
// the value reads back through render-stats (no get-view-mode), proves a few modes actually change
// the render (llvmpipe supports fillModeNonSolid), and asserts the run stays validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { Engine } from "./harness.ts";

let engine: Engine;
const shots: string[] = [];

interface Ref {
  id: string;
  name: string;
}

interface ViewModeResult {
  viewMode: string;
}

interface RenderStats {
  viewMode: string;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("focus", { entity: cube.id });
  await engine.settle();
});
afterAll(async () => {
  await engine?.shutdown();
  for (const shot of shots) {
    rmSync(shot, { force: true });
  }
});

async function screenshot(tag: string): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-viewmode-${process.pid}-${tag}.png`;
  shots.push(path);
  await engine.call("screenshot", { target: "viewport", path });
  const deadline = Date.now() + 10_000;
  while (!existsSync(path)) {
    if (Date.now() > deadline) {
      throw new Error(`screenshot ${tag} never landed at ${path}`);
    }
    await engine.settle(100);
  }
  await engine.settle(200);
  return readFileSync(path);
}

test("the default view mode is lit", async () => {
  const stats = await engine.call<RenderStats>("render-stats", {});
  expect(stats.viewMode).toBe("lit");
});

test("set-view-mode echoes the mode and reads back through render-stats", async () => {
  const set = await engine.call<ViewModeResult>("set-view-mode", { mode: "wireframe" });
  expect(set.viewMode).toBe("wireframe");
  const stats = await engine.call<RenderStats>("render-stats", {});
  expect(stats.viewMode).toBe("wireframe");
});

test("wireframe changes the render", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit");
  await engine.call("set-view-mode", { mode: "wireframe" });
  await engine.settle(300);
  const wire = await screenshot("wire");
  expect(wire.equals(lit)).toBe(false);
});

test("a buffer channel (albedo) round-trips and changes the render", async () => {
  const set = await engine.call<ViewModeResult>("set-view-mode", { mode: "albedo" });
  expect(set.viewMode).toBe("albedo");
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit2");
  await engine.call("set-view-mode", { mode: "albedo" });
  await engine.settle(300);
  const albedo = await screenshot("albedo");
  expect(albedo.equals(lit)).toBe(false);
});

// Every mode beyond the originals: the in-fragment debug channels (unlit / detail-lighting /
// lighting-only / reflections / depth / ambient-occlusion / gi / light-complexity) plus the two
// dedicated passes (lit-wireframe, motion-vectors). All must echo + read back through
// render-stats; the dedicated passes no-op gracefully when their inputs are absent, but the
// command round-trip is unconditional.
const NEW_MODES = [
  "unlit",
  "lit-wireframe",
  "detail-lighting",
  "lighting-only",
  "reflections",
  "depth",
  "ambient-occlusion",
  "gi",
  "light-complexity",
  "motion-vectors",
];

test("every new view mode echoes and reads back through render-stats", async () => {
  for (const mode of NEW_MODES) {
    const set = await engine.call<ViewModeResult>("set-view-mode", { mode });
    expect(set.viewMode).toBe(mode);
    const stats = await engine.call<RenderStats>("render-stats", {});
    expect(stats.viewMode).toBe(mode);
  }
  await engine.call("set-view-mode", { mode: "lit" });
});

test("detail-lighting changes the render", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit3");
  await engine.call("set-view-mode", { mode: "detail-lighting" });
  await engine.settle(300);
  const detail = await screenshot("detail");
  expect(detail.equals(lit)).toBe(false);
});

test("lit-wireframe overlays edges on the shaded scene", async () => {
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(300);
  const lit = await screenshot("lit4");
  await engine.call("set-view-mode", { mode: "lit-wireframe" });
  await engine.settle(300);
  const litWire = await screenshot("lit-wire");
  expect(litWire.equals(lit)).toBe(false);
});

test("the engine logged no validation errors", () => {
  expect(engine.validationErrors()).toEqual([]);
});
