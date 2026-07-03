// A thumbnail of an HDR asset is tonemapped, not clamped. An HDR's radiance runs past 1.0, so the
// old [0,1]×255 clamp blew the preview out to white. Tonemapping (Reinhard + gamma) must keep
// per-pixel detail: the decoded PNG is not uniformly white and shows real variance.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import { decodePng, makeHdr } from "./imggen.ts";

let engine: Engine;
let dir: string;
let id: string;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  dir = mkdtempSync(join(tmpdir(), "saffron-hdrthumb-"));
  // 6x6 (stb reads flat RGBE under width 8). Radiance ramps from ~0.05 to ~3.2 across the image,
  // tinted per channel — many pixels exceed 1.0, which a clamp would crush to white.
  const hdr = makeHdr(6, 6, (x, y) => {
    const v = 0.05 + (x + y * 6) * 0.09;
    return [v, v * 0.6, v * 0.3];
  });
  writeFileSync(join(dir, "sky.hdr"), hdr);
  const r = await engine.call<{ texture: string }>("import-texture", { path: join(dir, "sky.hdr") });
  id = r.texture;
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(dir, { recursive: true, force: true });
});

test("get-thumbnail tonemaps an HDR asset instead of clamping to white", async () => {
  const t = await engine.getThumbnail<{ base64: string }>("get-thumbnail", { asset: id, size: 128 });
  const { width, height, data } = decodePng(Buffer.from(t.base64, "base64"));
  expect(width).toBe(6);
  expect(height).toBe(6);

  let sum = 0;
  let min = 255;
  let max = 0;
  for (let i = 0; i < data.length; i++) {
    sum += data[i];
    min = Math.min(min, data[i]);
    max = Math.max(max, data[i]);
  }
  const mean = sum / data.length;
  expect(mean).toBeLessThan(240); // not a blown-out white square
  expect(max - min).toBeGreaterThan(20); // real per-pixel variance survives
  expect(engine.validationErrors()).toEqual([]);
});
