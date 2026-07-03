// Proves the "drag a folder" material importer: material-import scans a directory of PBR textures,
// detects each map's role from its filename suffix, imports them with the right colorspace, and
// assembles a .smat. Uses tiny decodable PNGs named like a real provider's set (Poly Haven /
// ambientCG conventions). Asserts the detected-role proposal covers albedo/normal/roughness/height.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";

let engine: Engine;
let dir: string;
// A 1x1 PNG — decodable by stb_image; the importer only needs the filename role + a valid image.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAVrXBpQAAAABJRU5ErkJggg==",
  "base64",
);

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  dir = mkdtempSync(join(tmpdir(), "saffron-matimport-"));
  for (const f of [
    "rock_diff_4k.png",
    "rock_nor_gl_4k.png",
    "rock_rough_4k.png",
    "rock_disp_4k.png",
    "readme.txt",
  ]) {
    writeFileSync(join(dir, f), f.endsWith(".txt") ? "ignore me" : PNG);
  }
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(dir, { recursive: true, force: true });
});

test("material-import detects PBR roles by filename suffix and builds a .smat", async () => {
  const result = await engine.call<{ id: string; roles: string }>("material-import", {
    path: dir,
    name: "Rock",
  });
  expect(result.id).not.toBe("0");
  expect(result.roles).toContain("albedo");
  expect(result.roles).toContain("normal");
  expect(result.roles).toContain("roughness");
  expect(result.roles).toContain("height");
  expect(engine.validationErrors()).toEqual([]);
});
