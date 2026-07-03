// import-texture honors an explicit colorspace so a single data map (normal/roughness/…) imported
// from a store part uploads linear, not sRGB. Asserted via the persisted catalog row's `linear`
// flag in project.json (the connector HTTP/part selection is editor-side and not exercised here).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";

let engine: Engine;
let dir: string;
const projectDir = `/tmp/saffron-e2e-cs-${process.pid}`;
// A real 1x1 truecolor PNG with a correct IDAT CRC (the `image` crate validates CRCs,
// unlike stb_image), so import-texture actually decodes it.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC",
  "base64",
);

interface TextureRow {
  id: string;
  type: string;
  linear?: boolean;
}

let linearId = "";
let srgbId = "";

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.loadProject(`${projectDir}/project.json`);
  dir = mkdtempSync(join(tmpdir(), "saffron-cs-"));
  writeFileSync(join(dir, "rock_rough.png"), PNG);
  writeFileSync(join(dir, "rock_diff.png"), PNG);
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
  rmSync(dir, { recursive: true, force: true });
});

test("a linear import flags the texture linear; the default (auto) does not", async () => {
  linearId = (
    await engine.call<{ texture: string }>("import-texture", {
      path: join(dir, "rock_rough.png"),
      colorspace: "linear",
    })
  ).texture;
  srgbId = (
    await engine.call<{ texture: string }>("import-texture", {
      path: join(dir, "rock_diff.png"),
    })
  ).texture;
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.settle();

  const doc = JSON.parse(readFileSync(`${projectDir}/project.json`, "utf8")) as {
    assets: TextureRow[];
  };
  const linear = doc.assets.find((a) => a.id === linearId);
  const srgb = doc.assets.find((a) => a.id === srgbId);
  expect(linear?.linear).toBe(true);
  expect(srgb?.linear ?? false).toBe(false);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
