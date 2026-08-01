// Selection picking: the `pick` command answers from the GPU selection-ID readback over the
// frame's binned cut, so what a click resolves to is what the frame drew at that pixel.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

// A cube parked at the origin under the default camera fills the middle of the frame and leaves
// the top-left corner empty, so one scene gives both a hit and a miss.
describe("gpu selection-id picking", () => {
  test("resolves the drawn entity at the pixel, and misses where nothing is drawn", async () => {
    const cube = await engine.call("add-entity", { preset: "cube" });
    await engine.settle(400);

    const hit = await engine.call("pick", { u: 0.5, v: 0.5 });
    expect(hit.hit).toBe(true);
    expect(hit.kind).toBe("mesh");
    expect(hit.id).toBe(cube.id);
    // The identity attachment rides with a world-space surface point and normal read out of the
    // same one-texel replay, so a pick answers where it landed as well as what it landed on.
    expect(hit.position).toHaveLength(3);
    expect(hit.normal).toHaveLength(3);
    const normal = hit.normal ?? [0, 0, 0];
    const length = Math.hypot(normal[0], normal[1], normal[2]);
    expect(length).toBeCloseTo(1, 3);

    const miss = await engine.call("pick", { u: 0.01, v: 0.01 });
    expect(miss.hit).toBe(false);
    expect(miss.id).toBeUndefined();
    expect(miss.kind).toBeUndefined();

    expect(engine.validationErrors()).toEqual([]);
  });

  // Deleting the entity removes its records from the cut, so the very next pick at the same pixel
  // must miss — proof the answer comes from the frame rather than from a retained CPU snapshot.
  test("stops hitting an entity once it leaves the frame's cut", async () => {
    const cube = await engine.call("add-entity", { preset: "sphere" });
    await engine.settle(400);
    expect((await engine.call("pick", { u: 0.5, v: 0.5 })).id).toBe(cube.id);

    await engine.call("destroy-entity", { entity: cube.id });
    await engine.settle(400);
    const after = await engine.call("pick", { u: 0.5, v: 0.5 });
    expect(after.id === cube.id).toBe(false);

    expect(engine.validationErrors()).toEqual([]);
  });
});
