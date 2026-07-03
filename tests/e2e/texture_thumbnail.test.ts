// get-thumbnail / view-asset GPU-downscale a large texture to fit the requested size before reading
// it back, instead of reading the native extent. A 1024x640 source must come back as a 128x80 PNG
// (get-thumbnail) and a 512x320 PNG (view-asset), with the reply's width/height matching the PNG.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import { makePng, pngSize } from "./imggen.ts";

let engine: Engine;
let dir: string;
let id: string;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  dir = mkdtempSync(join(tmpdir(), "saffron-texthumb-"));
  const png = makePng(1024, 640, (x, y) => [x & 255, y & 255, (x ^ y) & 255]);
  writeFileSync(join(dir, "big.png"), png);
  const r = await engine.call<{ texture: string }>("import-texture", { path: join(dir, "big.png") });
  id = r.texture;
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(dir, { recursive: true, force: true });
});

type Thumb = { base64: string; width: number; height: number; format: string };

test("get-thumbnail downsizes a 1024x640 texture to a 128x80 PNG", async () => {
  const t = await engine.getThumbnail<Thumb>("get-thumbnail", { asset: id, size: 128 });
  expect(t.format).toBe("png");
  const { width, height } = pngSize(Buffer.from(t.base64, "base64"));
  expect(Math.max(width, height)).toBe(128);
  expect(width).toBe(128);
  expect(height).toBe(80);
  expect(t.width).toBe(width); // reply dimensions are truthful, not the requested size
  expect(t.height).toBe(height);
  expect(engine.validationErrors()).toEqual([]);
});

test("view-asset downsizes the same texture to a 512x320 PNG", async () => {
  const t = await engine.getThumbnail<Thumb>("view-asset", { asset: id, size: 512 });
  const { width, height } = pngSize(Buffer.from(t.base64, "base64"));
  expect(Math.max(width, height)).toBe(512);
  expect(width).toBe(512);
  expect(height).toBe(320);
  expect(t.width).toBe(width);
  expect(t.height).toBe(height);
  expect(engine.validationErrors()).toEqual([]);
});
