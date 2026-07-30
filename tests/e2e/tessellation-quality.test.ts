// The displacement-tessellation path, driven over the wire on a genuinely displaced entity: a plane
// bound to a `.smat` whose height map is set and whose height mode is Displacement, so the
// factor/scan/finalize/emit chain amplifies real geometry into the frame's arena and the executor
// rasterizes it through the binned cut.
//
// Two things are asserted. `set-tessellation-quality` echoes the APPLIED, clamped budget, so
// "ok:true" alone is not the check. And the amplified geometry reaches the framebuffer: the suite
// screenshots the viewport with displacement on and off and compares the pixels, which is the only
// assertion here that fails if the displaced draw stops drawing.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import type { SetDisplacementResult, SetTessellationQualityResult } from "@saffron/protocol";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";
import { decodeRgb8Png, encodeRgba8Png, meanAbsoluteDifference } from "./image.ts";

let engine: Engine;
const cleaner = new Cleaner();

// A height map with a coarse checker relief. A smooth ramp would displace the surface without
// changing its shading much; alternating plateaus move both the silhouette and the re-derived
// normals, which is what a claim about amplified geometry needs to be visible.
function reliefPng(edge: number): Buffer {
  const cell = edge / 4;
  return encodeRgba8Png(edge, edge, (x, y) => {
    const high = (Math.floor(x / cell) + Math.floor(y / cell)) % 2 === 0;
    const level = high ? 255 : 0;
    return [level, level, level, 255];
  });
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine, {
    camera: { position: { x: 0, y: 0.55, z: 0.9 }, yaw: 0, pitch: -32, fov: 60 },
  });

  const scratch = mkdtempSync(join(tmpdir(), "saffron-tess-"));
  cleaner.defer(() => rmSync(scratch, { recursive: true, force: true }));
  const heightPath = join(scratch, "relief.png");
  writeFileSync(heightPath, reliefPng(64));
  const height = await engine.call<{ texture: string }>("import-texture", {
    path: heightPath,
    role: "height",
  });

  // Two calls, not one: `material-update` applies the fields it is given, and the height mode has
  // to be Displacement for the height map to amplify rather than shade.
  const material = await engine.call<{ id: string }>("material-create", { name: "Relief" });
  await engine.call("material-update", {
    material: material.id,
    heightTexture: height.texture,
    heightScale: 0.35,
  });
  await engine.call("material-update", { material: material.id, heightMode: "displacement" });

  const plane = await engine.call<{ id: string }>("add-entity", { preset: "plane" });
  await engine.call("material-assign", { entity: plane.id, material: material.id });
  await engine.call("set-displacement", { enabled: true });
  await engine.settle(600);
});
afterAll(async () => {
  await cleaner.cleanup();
});

const setQuality = (params: Record<string, unknown>) =>
  engine.call<SetTessellationQualityResult>("set-tessellation-quality", params);

// Captures the viewport once the loop reports the frame converged, so a capture never samples a
// temporal accumulator mid-flight.
async function settledShot(tag: string): Promise<Buffer> {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    if ((await engine.call<{ converged: boolean }>("render-stats")).converged) {
      break;
    }
    await engine.settle(100);
  }
  return captureViewport(engine, cleaner, tag);
}

test("set-tessellation-quality applies and echoes the budget", async () => {
  const r = await setQuality({ factorCap: 24, minFactor: 2, edgeLengthTarget: 8 });
  expect(r.factorCap).toBe(24);
  expect(r.minFactor).toBe(2);
  expect(r.edgeLengthTarget).toBe(8);
});

test("a partial request tunes only the named knobs", async () => {
  // Establish a known budget, then change only the edge-length target.
  await setQuality({ factorCap: 16, minFactor: 1, edgeLengthTarget: 12 });
  const r = await setQuality({ edgeLengthTarget: 5 });
  expect(r.edgeLengthTarget).toBe(5);
  expect(r.factorCap).toBe(16); // unchanged
  expect(r.minFactor).toBe(1); // unchanged
});

test("out-of-range values are clamped, not rejected", async () => {
  const r = await setQuality({ factorCap: 9999, minFactor: 0.1, edgeLengthTarget: 0 });
  expect(r.factorCap).toBe(2048); // cap ∈ [1, 2048] (the budget, not the cap, bounds dense scenes)
  expect(r.minFactor).toBe(1); // min ∈ [1, cap]
  expect(r.edgeLengthTarget).toBe(1); // edge target ≥ 1
});

test("a min factor above the cap is pinned to the cap", async () => {
  const r = await setQuality({ factorCap: 4, minFactor: 10 });
  expect(r.factorCap).toBe(4);
  expect(r.minFactor).toBe(4);
});

test("the amplified geometry reaches the framebuffer", async () => {
  await setQuality({ factorCap: 32, minFactor: 4, edgeLengthTarget: 4 });
  await engine.call("set-displacement", { enabled: true });

  // A control pair first — two captures of an unchanged scene. Whatever they differ by is the
  // residual temporal accumulation, the noise floor the toggle below has to clear; without it a
  // "the picture changed" assertion passes on an accumulator that had simply not converged.
  const onA = await settledShot("tess-on-a");
  const onB = await settledShot("tess-on-b");
  const floor = meanAbsoluteDifference(decodeRgb8Png(onA), decodeRgb8Png(onB));

  const off = await engine.call<SetDisplacementResult>("set-displacement", { enabled: false });
  expect(off.displacement).toBe(false);
  const flat = await settledShot("tess-off");
  expect(flat.equals(onB)).toBe(false);
  const spread = meanAbsoluteDifference(decodeRgb8Png(onB), decodeRgb8Png(flat));
  expect(spread).toBeGreaterThan(Math.max(4 * floor, 1));

  // And back: the difference tracks the toggle in both directions, so it belongs to the
  // displacement path rather than to anything drifting across the capture sequence.
  const back = await engine.call<SetDisplacementResult>("set-displacement", { enabled: true });
  expect(back.displacement).toBe(true);
  const restored = await settledShot("tess-on-c");
  expect(meanAbsoluteDifference(decodeRgb8Png(restored), decodeRgb8Png(flat))).toBeGreaterThan(
    Math.max(4 * floor, 1),
  );

  expect(engine.validationErrors()).toEqual([]);
});

test("driving the tessellation budget stays validation-clean", async () => {
  // Sweep the budget while the displaced plane re-dices; the extra frames give the validation
  // layers something to flag if a budget change desyncs the transient reservation.
  await setQuality({ factorCap: 32, minFactor: 1, edgeLengthTarget: 4 });
  await engine.call("render-stats");
  await setQuality({ factorCap: 8, minFactor: 1, edgeLengthTarget: 16 });
  await engine.call("render-stats");
  expect(engine.validationErrors()).toEqual([]);
});
