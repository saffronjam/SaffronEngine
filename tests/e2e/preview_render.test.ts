// preview-render renders a material on a studio-lit sphere and returns a base64 PNG (the editor's
// material preview pane + cached thumbnails). Two materials with different base colors must yield
// different images — proving the preview reflects the material, not a fixed picture.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("preview-render returns a PNG that reflects the material's color", async () => {
  const a = await engine.call<{ id: string }>("material-create", { name: "PrevA" });
  const b = await engine.call<{ id: string }>("material-create", { name: "PrevB" });
  await engine.call("material-update", { material: b.id, baseColor: { x: 1, y: 0, z: 0, w: 1 } });

  const pa = await engine.call<{ png: string }>("preview-render", { material: a.id, size: 128 });
  const pb = await engine.call<{ png: string }>("preview-render", { material: b.id, size: 128 });

  expect(pa.png.length).toBeGreaterThan(100);
  expect(pa.png.startsWith("iVBORw0KGgo")).toBe(true); // PNG magic, base64
  expect(pa.png).not.toBe(pb.png); // white vs red sphere
  expect(engine.validationErrors()).toEqual([]);
});
