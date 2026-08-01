// Shared spatial-world inspection over the real host: exact negative cell ownership, live
// static-mesh providers, canonical field samples, and the editor-camera residency source.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { EntityRef } from "@saffron/protocol";

let engine: Engine;
let cube: EntityRef;

beforeAll(async () => {
  engine = await Engine.boot();
  cube = await engine.call("add-entity", { preset: "cube" });
  await engine.settle();
});

afterAll(async () => {
  await engine?.shutdown();
});

test("exact negative ticks have one half-open owner at every requested level", async () => {
  const result = await engine.call("spatial-cell", {
    ticks: { x: "-1", y: "-262144", z: "262144" },
    level: 1,
  });
  expect(result.position.globalTicks).toEqual({ x: "-1", y: "-262144", z: "262144" });
  expect(result.position.cell).toMatchObject({ x: "-1", y: "-1", z: "1", level: 0 });
  expect(result.position.local).toEqual({ x: 262143, y: 0, z: 0 });
  expect(result.selectedCell).toMatchObject({ x: "-1", y: "-1", z: "0", level: 1 });
  expect(result.selectedCell.canonicalHex).toHaveLength(50);
});

test("the built-in cube is one authoritative static-mesh surface provider", async () => {
  const result = await engine.call("spatial-providers");
  const provider = result.providers.find((candidate) => candidate.id === cube.id);
  expect(provider).toBeDefined();
  expect(provider).toMatchObject({
    entity: cube.id,
    name: "Cube",
    primitiveCount: "12",
    maxTagsPerHit: 1,
    capabilities: {
      ray: true,
      project: true,
      nearest: true,
      uv: true,
      authoritativeAttachments: true,
      authoritativeFields: true,
    },
  });
});

test("altitude sampling resolves the nearest surface and returns canonical Q15.16 bits", async () => {
  const sample = await engine.call("spatial-sample", {
    provider: cube.id,
    channel: "altitude",
    position: { x: 0, y: 4, z: 0 },
  });
  expect(sample.provider).toBe(cube.id);
  expect(sample.derivative).toBe("value");
  expect(sample.value).toBe(0.5);
  expect(sample.valueBits).toBe(32768);
});

test("the host exposes its predicted editor-camera residency source and facet counts", async () => {
  const result = await engine.call("spatial-residency");
  expect(result.sources.length).toBeGreaterThan(0);
  expect(result.sources[0]?.facets).toEqual(["render", "editing"]);
  expect(result.cells.length).toBeGreaterThan(0);
  expect(result.cells.some((cell) => cell.referenceCounts.render > 0)).toBe(true);
  expect(result.cells.some((cell) => cell.referenceCounts.editing > 0)).toBe(true);
});

test("a travelling editor camera leads its claim with a measured velocity", async () => {
  // The prediction horizon on a residency source only leads with something when the source
  // reports a velocity, and a viewpoint has no rigidbody to read one from — the host measures the
  // camera's own travel and publishes it. A source that reports rest leaves the predictive half of
  // residency inert, so this waits for a non-zero reading rather than sampling once at a fixed
  // frame: the estimate is smoothed, and how many frames pass between the move and the read is
  // scheduling, not behaviour.
  const deadline = Date.now() + 15_000;
  let fastest = 0;
  for (let step = 1; Date.now() < deadline && fastest <= 0.5; step += 1) {
    await engine.call("set-camera", { position: { x: step * 2, y: 5, z: 9 } });
    const { velocityMps } = (await engine.call("spatial-residency")).sources[0]!;
    fastest = Math.max(fastest, Math.hypot(velocityMps.x, velocityMps.y, velocityMps.z));
  }
  expect(fastest).toBeGreaterThan(0.5);
});

test("spatial inspection is validation-clean", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
