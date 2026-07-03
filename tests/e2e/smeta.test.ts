// A foreign/headerless file (a raw .png dropped into assets/, with no room in its own bytes for an id)
// gets a stable identity + colorspace from a `.smeta` sidecar the scan mints on first sight. The id
// survives a reload, and editing the sidecar's colorspace is what a later upload reads.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { Engine } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-smeta-${process.pid}`;
const foreignPng = `${projectDir}/assets/textures/wood.png`;
const foreignSmeta = `${foreignPng}.smeta`;

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.loadProject(`${projectDir}/project.json`);
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

let textureId = "";

test("a dropped foreign .png gets a minted .smeta with a stable id on scan", async () => {
  mkdirSync(`${projectDir}/assets/textures`, { recursive: true });
  // The scan does not decode; arbitrary bytes are enough to exercise identity + sidecar minting.
  writeFileSync(foreignPng, Buffer.from([0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4]));

  await engine.call("scan-assets");
  await engine.settle();

  expect(existsSync(foreignSmeta)).toBe(true);
  const smeta = JSON.parse(readFileSync(foreignSmeta, "utf8")) as { id: string; colorspace: string; type: string };
  expect(smeta.type).toBe("texture");
  expect(typeof smeta.colorspace).toBe("string");
  expect(smeta.id).not.toBe("0");

  const assets = await engine.call<AssetList>("list-assets");
  const wood = assets.assets.find((a) => a.name === "wood");
  expect(wood).toBeDefined();
  expect(wood?.id).toBe(smeta.id);
  textureId = smeta.id;
});

test("the .smeta identity is stable across a reload", async () => {
  await engine.loadProject(`${projectDir}/project.json`);
  await engine.settle();
  const assets = await engine.call<AssetList>("list-assets");
  const wood = assets.assets.find((a) => a.name === "wood");
  expect(wood?.id).toBe(textureId);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
