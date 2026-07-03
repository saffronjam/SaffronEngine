// The connector material path: material-import accepts optional store attribution and records
// it on the imported .smat's catalog entry, surfaced by list-assets for the credits view. The
// connector HTTP + zip extraction live editor-side and are not exercised here — this targets the
// host command contract that the ambientCG / Poly Haven material results depend on.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import type { AssetList } from "@saffron/protocol";

let engine: Engine;
let dir: string;
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAVrXBpQAAAABJRU5ErkJggg==",
  "base64",
);
const ATTRIBUTION = {
  licenseId: "cc0",
  requiresAttribution: false,
  licenseUrl: "https://creativecommons.org/publicdomain/zero/1.0/",
  author: "ambientCG",
  sourceUrl: "https://ambientcg.com/view?id=Wood050",
  storeId: "ambientcg",
};

let materialId = "";

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  dir = mkdtempSync(join(tmpdir(), "saffron-store-mat-"));
  for (const f of ["Wood050_1K_Color.png", "Wood050_1K_NormalGL.png", "Wood050_1K_Roughness.png"]) {
    writeFileSync(join(dir, f), PNG);
  }
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(dir, { recursive: true, force: true });
});

test("material-import with attribution builds a .smat and records the source", async () => {
  const result = await engine.call<{ id: string; roles: string }>("material-import", {
    path: dir,
    name: "Wood050",
    attribution: ATTRIBUTION,
  });
  expect(result.id).not.toBe("0");
  materialId = result.id;
});

test("the material's attribution is surfaced by list-assets", async () => {
  const list = await engine.call<AssetList>("list-assets");
  const row = list.assets.find((a) => a.id === materialId) as
    | { attribution?: typeof ATTRIBUTION }
    | undefined;
  expect(row?.attribution?.storeId).toBe("ambientcg");
  expect(row?.attribution?.licenseId).toBe("cc0");
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
