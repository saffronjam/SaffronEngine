// Thumbnails are cached in an app-level, content-addressed store at
// <appDataRoot>/thumbnail-cache/v<VERSION>-<contentHash>-<size>.png, shared across projects and
// surviving a restart. The key is a hash of the asset's baked content, not a file stat — so a bare
// touch (mtime bump) still hits, and only a real content change (or a version bump) mints a new key
// and regenerates.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import { makePng, pngSize } from "./imggen.ts";

let root: string;
let appdata: string;
let id: string;

function cachePngs(): string[] {
  const dir = join(appdata, "thumbnail-cache");
  return existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".png")) : [];
}

beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), "saffron-thumbcache-"));
  // The two engines share one app-data root so the second sees the first's cache.
  appdata = mkdtempSync(join(tmpdir(), "saffron-thumbcache-appdata-"));
});
afterAll(() => {
  rmSync(root, { recursive: true, force: true });
  rmSync(appdata, { recursive: true, force: true });
});

test("content-addressed thumbnails persist across a restart, survive a touch, and regenerate on a real edit", async () => {
  // First engine: create the project, import a texture, generate its thumbnail.
  const e1 = await Engine.boot({ SAFFRON_APPDATA_DIR: appdata });
  await e1.newProject({ name: "cachetest", root });
  const src = join(root, "src.png");
  writeFileSync(src, makePng(1024, 640, (x, y) => [x & 255, y & 255, (x ^ y) & 255]));
  const imported = await e1.call<{ texture: string }>("import-texture", { path: src });
  id = imported.texture;
  await e1.call("save-project", {}); // persist the catalog (incl. content hashes) for the restart

  const t1 = await e1.getThumbnail<{ base64: string; width: number; height: number }>("get-thumbnail", {
    asset: id,
    size: 128,
  });
  const real = Buffer.from(t1.base64, "base64");
  expect(cachePngs().length).toBe(1); // the miss wrote one content-addressed cache file
  expect(e1.validationErrors()).toEqual([]);
  await e1.shutdown();

  // Replace the cached PNG with a distinct sentinel so a cache HIT is observable (a regenerated
  // thumbnail would be the deterministic `real` bytes, never the sentinel).
  const sentinel = makePng(40, 24, () => [10, 200, 90]);
  const cacheFile = join(appdata, "thumbnail-cache", cachePngs()[0]);
  writeFileSync(cacheFile, sentinel);

  // Second engine, same app-data + project: the thumbnail comes from the shared content cache.
  const e2 = await Engine.boot({ SAFFRON_APPDATA_DIR: appdata });
  await e2.openProject(root);
  const t2 = await e2.getThumbnail<{ base64: string; width: number; height: number }>("get-thumbnail", {
    asset: id,
    size: 128,
  });
  expect(Buffer.from(t2.base64, "base64").equals(sentinel)).toBe(true); // served from disk cache
  const dims = pngSize(sentinel);
  expect(t2.width).toBe(dims.width); // dimensions read truthfully from the cached PNG header
  expect(t2.height).toBe(dims.height);

  // A touch with no content change must NOT invalidate — the key is the content hash, not the
  // file stat, so the sentinel is still served.
  const future = new Date(Date.now() + 4000);
  utimesSync(join(root, "assets", "textures", `${id}.png`), future, future);
  const t3 = await e2.getThumbnail<{ base64: string }>("get-thumbnail", { asset: id, size: 128 });
  expect(Buffer.from(t3.base64, "base64").equals(sentinel)).toBe(true); // touch → still a hit

  // A real content change (different pixels) + reload rescans, re-hashes to a new content key, and
  // regenerates: a new cache file, and no longer the sentinel.
  writeFileSync(
    join(root, "assets", "textures", `${id}.png`),
    makePng(1024, 640, (x, y) => [(x + 7) & 255, (y + 9) & 255, (x & y) & 255]),
  );
  await e2.reloadProject();
  const t4 = await e2.getThumbnail<{ base64: string }>("get-thumbnail", { asset: id, size: 128 });
  expect(Buffer.from(t4.base64, "base64").equals(sentinel)).toBe(false); // regenerated under a new key
  expect(cachePngs().length).toBeGreaterThanOrEqual(2); // a new content-key file was written
  expect(e2.validationErrors()).toEqual([]);
  await e2.shutdown();
});

// Content-addressed cache semantics: deleting an asset leaves the shared cache intact (another
// asset/project may reference the same content bytes); editing a *parent* material reflows the
// instance's resolved-state key so its thumbnail regenerates; and the thumbnail-cache control
// command reports + empties the app-level cache.
describe("cache invalidation semantics", () => {
  let engine: Engine;
  let invalRoot: string;

  function invalCachePngs(): string[] {
    const dir = join(engine.appdata, "thumbnail-cache");
    return existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".png")) : [];
  }

  beforeAll(async () => {
    invalRoot = mkdtempSync(join(tmpdir(), "saffron-thumbinval-"));
    engine = await Engine.boot();
    await engine.newProject({ name: "inval", root: invalRoot });
  });
  afterAll(async () => {
    await engine?.shutdown();
    rmSync(invalRoot, { recursive: true, force: true });
  });

  test("delete-asset leaves the shared content-addressed cache intact", async () => {
    writeFileSync(join(invalRoot, "tex.png"), makePng(256, 256, (x, y) => [x & 255, y & 255, 128]));
    const { texture } = await engine.call<{ texture: string }>("import-texture", {
      path: join(invalRoot, "tex.png"),
    });
    await engine.getThumbnail("get-thumbnail", { asset: texture, size: 128 });
    const before = invalCachePngs();
    expect(before.length).toBeGreaterThan(0); // the miss wrote a content-addressed file

    await engine.call("delete-asset", { asset: texture });
    // The cache is content-addressed and shared across assets/projects, so a delete must NOT purge
    // the bytes — another asset may reference the same content; eviction bounds growth instead.
    expect(invalCachePngs()).toEqual(before);
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
    expect(stats.entries).toBe(invalCachePngs().length);
    expect(stats.bytes).toBeGreaterThan(0);

    const cleared = await engine.call<{ entries: number; bytes: number }>("thumbnail-cache", {
      action: "clear",
    });
    expect(cleared.entries).toBe(stats.entries);
    expect(invalCachePngs().length).toBe(0);
    expect(engine.validationErrors()).toEqual([]);
  });
});
