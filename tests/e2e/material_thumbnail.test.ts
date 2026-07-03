// get-thumbnail renders a material asset's preview sphere (so material tiles in the AssetsPanel +
// the entity material picker show a real preview, not a generic icon).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("get-thumbnail renders a material preview PNG", async () => {
  const m = await engine.call<{ id: string }>("material-create", { name: "Thumb" });
  const thumb = await engine.getThumbnail<{ base64: string; format: string }>("get-thumbnail", {
    asset: m.id,
    size: 96,
  });
  expect(thumb.format).toBe("png");
  expect(thumb.base64.startsWith("iVBORw0KGgo")).toBe(true);
  expect(engine.validationErrors()).toEqual([]);
});
