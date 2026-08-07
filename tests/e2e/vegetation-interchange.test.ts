// Phase-14 acceptance through the real host: instanced points from a content-creation tool become
// authored anchors, and the same anchors come back out addressing the same plants.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";
import {
  authoredAssets,
  installPlantSources,
  loadFixture,
  vegetationMap,
} from "./vegetation-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

// A Houdini JSON `.geo` point cloud: two oaks and one colour attribute nothing can express.
function scatter(name: string) {
  const numeric = (attribute: string, size: number, tuples: number[][]) => [
    ["scope", "public", "type", "numeric", "name", attribute],
    [
      "size",
      size,
      "storage",
      "fpreal64",
      "values",
      ["size", size, "storage", "fpreal64", "tuples", tuples],
    ],
  ];
  return [
    "fileversion",
    "20.0.0",
    "pointcount",
    2,
    "vertexcount",
    0,
    "primitivecount",
    0,
    "topology",
    ["pointref", ["indices", []]],
    "attributes",
    [
      "pointattributes",
      [
        numeric("P", 3, [
          [12.5, 0, 20.25],
          [30, 0, 8],
        ]),
        numeric("orient", 4, [
          [0, 0, 0, 1],
          [0, 0, 0, 1],
        ]),
        numeric("pscale", 1, [[1], [2]]),
        numeric("id", 1, [[41], [42]]),
        numeric("Cd", 3, [
          [1, 0, 0],
          [0, 1, 0],
        ]),
        [
          ["scope", "public", "type", "string", "name", "name"],
          [
            "size",
            1,
            "storage",
            "int32",
            "strings",
            [name],
            "indices",
            ["size", 1, "storage", "int32", "arrays", [[0, 0]]],
          ],
        ],
      ],
    ],
  ];
}

// A minimal glTF with one instanced node: two placements under a node offset in x, plus an
// attribute the canonical vocabulary cannot express.
function instancedGltf(name: string) {
  const floats = (values: number[]) => {
    const buffer = new ArrayBuffer(values.length * 4);
    new Float32Array(buffer).set(values);
    return new Uint8Array(buffer);
  };
  const uints = (values: number[]) => {
    const buffer = new ArrayBuffer(values.length * 4);
    new Uint32Array(buffer).set(values);
    return new Uint8Array(buffer);
  };
  const translation = floats([12.5, 0, 20.25, 6, 0, 4]);
  const rotation = floats([0, 0, 0, 1, 0, 0, 0, 1]);
  const scale = floats([1, 1, 1, 2, 2, 2]);
  const id = uints([101, 102]);
  const blob = new Uint8Array(translation.length + rotation.length + scale.length + id.length);
  let cursor = 0;
  const views: { byteOffset: number; byteLength: number }[] = [];
  for (const part of [translation, rotation, scale, id]) {
    blob.set(part, cursor);
    views.push({ byteOffset: cursor, byteLength: part.length });
    cursor += part.length;
  }
  return {
    asset: { version: "2.0" },
    extensionsUsed: ["EXT_mesh_gpu_instancing"],
    scene: 0,
    scenes: [{ nodes: [0] }],
    nodes: [
      {
        name,
        translation: [4, 0, 0],
        extensions: {
          EXT_mesh_gpu_instancing: {
            attributes: { TRANSLATION: 0, ROTATION: 1, SCALE: 2, _ID: 3, _WIND: 3 },
          },
        },
      },
    ],
    buffers: [
      {
        byteLength: blob.length,
        uri: `data:application/octet-stream;base64,${Buffer.from(blob).toString("base64")}`,
      },
    ],
    bufferViews: views.map((view) => ({ buffer: 0, ...view })),
    accessors: [
      { bufferView: 0, componentType: 5126, count: 2, type: "VEC3" },
      { bufferView: 1, componentType: 5126, count: 2, type: "VEC4" },
      { bufferView: 2, componentType: 5126, count: 2, type: "VEC3" },
      { bufferView: 3, componentType: 5125, count: 2, type: "SCALAR" },
    ],
  };
}

test("instanced points import as anchors and export back addressing the same plants", async () => {
  const fixture = loadFixture("vegetation-phase3");
  const sources = authoredAssets(cleaner, fixture, "interchange");
  await installPlantSources(engine, fixture);
  for (const path of [sources.plant, sources.biome, sources.map]) {
    await engine.call("import-vegetation-asset", { path });
  }

  const before = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  const scratch = mkdtempSync(join(tmpdir(), "saffron-interchange-"));
  cleaner.defer(() => rmSync(scratch, { recursive: true, force: true }));
  const scatterPath = join(scratch, "oaks.geo");
  writeFileSync(scatterPath, JSON.stringify(scatter("oak")));

  const imported = await engine.call("vegetation-import-points", {
    map: fixture.map,
    layer: fixture.authoredLayer,
    path: scatterPath,
    prototypes: [{ name: "oak", family: fixture.plant }],
    expectedGeneration: vegetationMap(before).generation,
  });
  expect(imported.anchors).toBe(2);
  expect(imported.prototypes).toBe(1);
  // The colour attribute has nowhere to go in the canonical vocabulary, and says so.
  expect(imported.unsupported).toEqual(["Cd"]);
  expect(Number(imported.generation)).toBeGreaterThan(Number(vegetationMap(before).generation));

  const exportPath = join(scratch, "oaks-back.geo");
  const exported = await engine.call("vegetation-export-points", {
    map: fixture.map,
    layer: fixture.authoredLayer,
    path: exportPath,
  });
  expect(exported.instances).toBe(2);
  expect(exported.prototypes).toBe(1);

  // The export names prototypes by their families' catalog names, which is what an artist sees.
  const written = JSON.parse(readFileSync(exportPath, "utf8")) as unknown[];
  const exportedName = JSON.stringify(written).match(/"strings",\["([^"]+)"\]/)?.[1];
  expect(exportedName).toBeTruthy();

  // Re-importing what was exported re-addresses the same plants rather than adding more.
  const after = await engine.call("vegetation-asset-summary", { asset: fixture.map });
  const again = await engine.call("vegetation-import-points", {
    map: fixture.map,
    layer: fixture.authoredLayer,
    path: exportPath,
    prototypes: [{ name: exportedName!, family: fixture.plant }],
    expectedGeneration: vegetationMap(after).generation,
  });
  expect(again.anchors).toBe(2);
  expect(again.tiles).toBe(imported.tiles);
  expect(again.unsupported).toEqual([]);

  // A glTF carrying EXT_mesh_gpu_instancing says the same thing in another file, and lands in the
  // same vocabulary.
  {
    const gltfPath = join(scratch, "scatter.gltf");
    writeFileSync(gltfPath, JSON.stringify(instancedGltf("oakScatter")));
    const generation = again.generation;
    const fromGltf = await engine.call("vegetation-import-points", {
      map: fixture.map,
      layer: fixture.authoredLayer,
      path: gltfPath,
      prototypes: [{ name: "oakScatter", family: fixture.plant }],
      expectedGeneration: generation,
    });
    expect(fromGltf.anchors).toBe(2);
    expect(fromGltf.prototypes).toBe(1);
    // The wind attribute has nowhere to go, and says so.
    expect(fromGltf.unsupported).toEqual(["_WIND"]);
  }

  // A USD PointInstancer says it a third way, and a USD export reads back through the same door.
  {
    const usdaOut = join(scratch, "plants.usda");
    const exportedUsd = await engine.call("vegetation-export-points", {
      map: fixture.map,
      layer: fixture.authoredLayer,
      path: usdaOut,
    });
    expect(exportedUsd.instances).toBeGreaterThan(0);
    const stage = readFileSync(usdaOut, "utf8");
    expect(stage).toContain("def PointInstancer");
    expect(stage).toContain("int64[] ids");

    const summary = await engine.call("vegetation-asset-summary", {
      asset: fixture.map,
    });
    const roundTripped = await engine.call("vegetation-import-points", {
      map: fixture.map,
      layer: fixture.authoredLayer,
      path: usdaOut,
      prototypes: [{ name: exportedName!, family: fixture.plant }],
      expectedGeneration: vegetationMap(summary).generation,
    });
    expect(roundTripped.anchors).toBe(exportedUsd.instances);
    expect(roundTripped.unsupported).toEqual([]);
  }

  // An unknown extension is refused by name rather than guessed at.
  await expect(
    engine.call("vegetation-export-points", {
      map: fixture.map,
      layer: fixture.authoredLayer,
      path: join(scratch, "plants.fbx"),
    }),
  ).rejects.toThrow();

  // A prototype the caller never bound has no family to place.
  await expect(
    engine.call("vegetation-import-points", {
      map: fixture.map,
      layer: fixture.authoredLayer,
      path: scatterPath,
      prototypes: [{ name: "birch", family: fixture.plant }],
      expectedGeneration: again.generation,
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});
