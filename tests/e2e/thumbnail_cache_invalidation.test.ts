// Content-addressed cache semantics: deleting an asset leaves the shared cache intact (another
// asset/project may reference the same content bytes); editing a *parent* material reflows the
// instance's resolved-state key so its thumbnail regenerates; and the thumbnail-cache control
// command reports + empties the app-level cache.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import { makePng } from "./imggen.ts";

let engine: Engine;
let root: string;

function cachePngs(): string[] {
  const dir = join(engine.appdata, "thumbnail-cache");
  return existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".png")) : [];
}

beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "saffron-thumbinval-"));
  engine = await Engine.boot();
  await engine.newProject({ name: "inval", root });
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(root, { recursive: true, force: true });
});

test("delete-asset leaves the shared content-addressed cache intact", async () => {
  writeFileSync(join(root, "tex.png"), makePng(256, 256, (x, y) => [x & 255, y & 255, 128]));
  const { texture } = await engine.call<{ texture: string }>("import-texture", {
    path: join(root, "tex.png"),
  });
  await engine.getThumbnail("get-thumbnail", { asset: texture, size: 128 });
  const before = cachePngs();
  expect(before.length).toBeGreaterThan(0); // the miss wrote a content-addressed file

  await engine.call("delete-asset", { asset: texture });
  // The cache is content-addressed and shared across assets/projects, so a delete must NOT purge
  // the bytes — another asset may reference the same content; eviction bounds growth instead.
  expect(cachePngs()).toEqual(before);
  expect(engine.validationErrors()).toEqual([]);
});

test("editing a parent material regenerates the instance thumbnail", async () => {
  const parent = await engine.call<{ id: string }>("material-create", { name: "Parent" });
  const inst = await engine.call<{ id: string }>("material-create-instance", {
    parent: parent.id,
    name: "Inst",
  });

  const t1 = await engine.getThumbnail<{ base64: string }>("get-thumbnail", { asset: inst.id, size: 96 });
  // Edit the PARENT — the instance's .smat is untouched, but its resolved state (and so its cache
  // key) changes, so its thumbnail must regenerate rather than serve the stale white sphere.
  await engine.call("material-update", { material: parent.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });
  const t2 = await engine.getThumbnail<{ base64: string }>("get-thumbnail", { asset: inst.id, size: 96 });

  expect(t2.base64).not.toBe(t1.base64);
  expect(engine.validationErrors()).toEqual([]);
});

test("thumbnail-cache stats counts the app-level dir and clear empties it", async () => {
  const stats = await engine.call<{ entries: number; bytes: number }>("thumbnail-cache", {
    action: "stats",
  });
  expect(stats.entries).toBe(cachePngs().length);
  expect(stats.bytes).toBeGreaterThan(0);

  const cleared = await engine.call<{ entries: number; bytes: number }>("thumbnail-cache", {
    action: "clear",
  });
  expect(cleared.entries).toBe(stats.entries);
  expect(cachePngs().length).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});
