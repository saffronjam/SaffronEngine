// Thumbnails are cached in an app-level, content-addressed store at
// <appDataRoot>/thumbnail-cache/<contentHash>-<size>.png, shared across projects and surviving a
// restart. The key is a hash of the asset's baked content, not a file stat — so a bare touch
// (mtime bump) still hits, and only a real content change mints a new key and regenerates.

import { afterAll, beforeAll, expect, test } from "bun:test";
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
  await e1.call("new-project", { name: "cachetest", root });
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
  await e2.call("open-project", { path: root });
  const t2 = await e2.getThumbnail<{ base64: string; width: number; height: number }>("get-thumbnail", {
    asset: id,
    size: 128,
  });
  expect(Buffer.from(t2.base64, "base64").equals(sentinel)).toBe(true); // served from disk cache
  const dims = pngSize(sentinel);
  expect(t2.width).toBe(dims.width); // dimensions read truthfully from the cached PNG header
  expect(t2.height).toBe(dims.height);

  // A touch with no content change must NOT invalidate — the key is the content hash, not the
  // file stat, so the sentinel is still served (the old mtime-keyed cache would have regenerated).
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
  await e2.call("reload-project", {});
  const t4 = await e2.getThumbnail<{ base64: string }>("get-thumbnail", { asset: id, size: 128 });
  expect(Buffer.from(t4.base64, "base64").equals(sentinel)).toBe(false); // regenerated under a new key
  expect(cachePngs().length).toBeGreaterThanOrEqual(2); // a new content-key file was written
  expect(e2.validationErrors()).toEqual([]);
  await e2.shutdown();
});
