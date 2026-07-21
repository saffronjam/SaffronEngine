// Shared spatial-world inspection over the real host: exact negative cell ownership, live
// static-mesh providers, canonical field samples, and the editor-camera residency source.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type {
  EntityRef,
  SpatialCellResult,
  SpatialResidencyResult,
  SpatialSampleResult,
  SurfaceProvidersResult,
} from "@saffron/protocol";

let engine: Engine;
let cube: EntityRef;

beforeAll(async () => {
  engine = await Engine.boot();
  cube = await engine.call<EntityRef>("add-entity", { preset: "cube" });
  await engine.settle();
});

afterAll(async () => {
  await engine?.shutdown();
});

test("exact negative ticks have one half-open owner at every requested level", async () => {
  const result = await engine.call<SpatialCellResult>("spatial-cell", {
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
  const result = await engine.call<SurfaceProvidersResult>("spatial-providers");
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
  const sample = await engine.call<SpatialSampleResult>("spatial-sample", {
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
  const result = await engine.call<SpatialResidencyResult>("spatial-residency");
  expect(result.sources.length).toBeGreaterThan(0);
  expect(result.sources[0]?.facets).toEqual(["render", "editing"]);
  expect(result.cells.length).toBeGreaterThan(0);
  expect(result.cells.some((cell) => cell.referenceCounts.render > 0)).toBe(true);
  expect(result.cells.some((cell) => cell.referenceCounts.editing > 0)).toBe(true);
});

test("spatial inspection is validation-clean", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
