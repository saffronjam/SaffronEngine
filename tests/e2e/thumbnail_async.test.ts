// Phase 7: a cold-cache thumbnail is generated on a worker thread, so get-thumbnail replies `pending`
// immediately instead of blocking the control/frame loop on the (~1s for a 4k HDR) decode + upload +
// render. The editor (and the harness helper) retries the pending reply until the worker finishes and
// the PNG lands in the disk cache.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "./harness.ts";
import { makeHdr } from "./imggen.ts";

let engine: Engine;
let root: string;
let id: string;

beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "saffron-thumbasync-"));
  engine = await Engine.boot();
  await engine.newProject({ name: "async", root });
  // A 4096x2048 HDR: ~1s of decode + 33.5M floatToHalf + 64MB upload if it ran synchronously.
  writeFileSync(
    join(root, "sky.hdr"),
    makeHdr(4096, 2048, (x, y) => {
      const v = 0.1 + ((x + y) % 512) * 0.02;
      return [v, v * 0.7, v * 0.4];
    }),
  );
  const r = await engine.call<{ texture: string }>("import-texture", { path: join(root, "sky.hdr") });
  id = r.texture;
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(root, { recursive: true, force: true });
});

test("a cold thumbnail replies pending immediately, then resolves on retry", async () => {
  // The first request must return promptly with pending — NOT after a ~1s synchronous generation.
  const t0 = performance.now();
  const first = await engine.call<{ pending: boolean; base64: string }>("get-thumbnail", {
    asset: id,
    size: 128,
  });
  const firstMs = performance.now() - t0;
  expect(first.pending).toBe(true);
  expect(first.base64).toBe("");
  expect(firstMs).toBeLessThan(300); // enqueued + returned, did not block on the generation

  // The worker finishes and the retry is a disk-cache hit with a real PNG.
  const resolved = await engine.getThumbnail<{ pending: boolean; base64: string; width: number }>(
    "get-thumbnail",
    { asset: id, size: 128 },
  );
  expect(resolved.pending).toBe(false);
  expect(resolved.base64.startsWith("iVBORw0KGgo")).toBe(true);
  expect(resolved.width).toBe(128);
  expect(engine.validationErrors()).toEqual([]);
}, 30000);

test("control stays responsive while the worker generates", async () => {
  // A second, larger size is a fresh cold miss. Poll a cheap command while it generates; the drain
  // must keep answering quickly (a synchronous generation would stall every round-trip for ~1s).
  const first = await engine.call<{ pending: boolean }>("view-asset", { asset: id, size: 512 });
  expect(first.pending).toBe(true);

  let maxPollMs = 0;
  for (let i = 0; i < 8; i++) {
    const p = performance.now();
    await engine.call("render-stats");
    maxPollMs = Math.max(maxPollMs, performance.now() - p);
    await new Promise((r) => setTimeout(r, 40));
  }
  // Generous bound: even on the llvmpipe software GPU (which serializes the worker's blit with the
  // frame on one queue) a single round-trip stays well under a synchronous generation's ~1s stall.
  expect(maxPollMs).toBeLessThan(600);

  const resolved = await engine.getThumbnail<{ pending: boolean }>("view-asset", { asset: id, size: 512 });
  expect(resolved.pending).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
}, 30000);
