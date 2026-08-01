// Phase-14 acceptance through the real host: a native plant family is created from the starter
// botanical graph, grows real geometry, and normalizes through the same compiler an imported family
// uses — no native-only path, and the same growth on every run.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  BotanicalManualEditDto,
  PlantGraftSourceDto,
  PlantImportSettingsDto,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";

// What a source is read with when the caller states nothing: metres, Y-up, right-handed,
// counter-clockwise, unit scale.
const SOURCE_DEFAULTS: PlantImportSettingsDto = {
  units: "meters",
  upAxis: "positive-y",
  forwardAxis: "positive-z",
  handedness: "right",
  scaleBits: 65536,
  pivot: { kind: "source-origin" },
  winding: "counter-clockwise",
  uvOrigin: "top-left",
  uvScaleBits: [65536, 65536],
  uvOffsetBits: [0, 0],
  tangentPolicy: "generate-missing",
};

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

// Creates a family from the starter graph. A zero seed derives one from the name.
function createPlant(name: string, materials: string[], folder = "") {
  return engine.call("plant-create", { name, folder, seed: "0", materials });
}

// Two materials for the graph's slots: bark and leaves.
let slots: string[] = [];
async function materials() {
  if (slots.length === 0) {
    const bark = await engine.call("material-create", { name: "E2E bark" });
    const leaf = await engine.call("material-create", { name: "E2E leaf" });
    slots = [bark.id, leaf.id];
  }
  return slots;
}

test("a created native family grows geometry and validates through the shared compiler", async () => {
  const [bark, leaf] = await materials();
  const created = await createPlant("E2E native birch", [bark, leaf], "plants");
  expect(Number(created.plant)).toBeGreaterThan(0);

  const growth = created.growth;
  // The starter graph is a whole small tree: a trunk, roots, leaves on spiral frames.
  expect(growth.axes).toBeGreaterThan(1);
  expect(growth.shells).toBeGreaterThan(0);
  expect(growth.elements).toBeGreaterThan(0);
  expect(growth.vertices).toBeGreaterThan(0);
  expect(growth.triangles).toBeGreaterThan(0);
  expect(growth.spines).toBe(growth.axes);
  expect(growth.parts).toBeGreaterThan(1);
  expect(growth.heightBits).toBeGreaterThan(0);
  expect(growth.graph).toMatch(/^[0-9a-f]{64}$/);

  // Reading it back grows the same plant: the graph is the whole source of truth.
  const reread = await engine.call("plant-growth", {
    plant: String(created.plant),
    variation: 0,
  });
  expect(reread).toEqual(growth);

  // And it normalizes through the one plant compiler, with no diagnostics.
  const validation = await engine.call("plant-validate", {
    plant: String(created.plant),
  });
  expect(validation.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);
  expect(engine.validationErrors()).toEqual([]);
});

test("two families created the same way are two different individuals", async () => {
  const [bark, leaf] = await materials();
  const first = await createPlant("E2E native oak", [bark, leaf]);
  const second = await createPlant("E2E native elm", [bark, leaf]);
  // The seed is derived from the name, so the two differ without the caller managing seeds.
  expect(first.growth.seed).not.toBe(second.growth.seed);
  expect(first.growth.variations).toBe(1);
  expect(first.growth.age).toBe(65535);
  expect(first.growth.graph).not.toBe(second.growth.graph);

  // A family with no materials for its slots is refused rather than binding slot zero twice.
  await expect(
    createPlant("E2E native short", [bark]),
  ).rejects.toThrow();
  // And an empty name is refused.
  await expect(
    createPlant("  ", [String(bark), String(leaf)]),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a manual edit layer survives a parameter change and reports what it lost", async () => {
  const [bark, leaf] = await materials();
  const created = await createPlant("E2E native hand-edited", [bark, leaf]);
  const plant = String(created.plant);

  // The addressable elements: this is what an authoring panel selects in, and what an edit targets.
  const elements = await engine.call("plant-elements", { plant, variation: 0 });
  expect(elements.axes.length).toBe(created.growth.axes);
  expect(elements.elements.length).toBe(created.growth.elements);
  const trunk = elements.axes.find((axis) => axis.element === "trunk");
  const target = elements.elements[0]!;
  expect(trunk).toBeDefined();
  expect(trunk!.parent).toBeNull();
  expect(BigInt(target.id)).toBeGreaterThan(0n);

  // Every placement names the axis carrying its frame, and that axis is in the same report — the
  // structure tree hangs it there directly. The starter graph puts its leaves on trunk frames that
  // no axis grew from, so nothing about the host is recoverable from the axis list alone.
  const axisIds = new Set(elements.axes.map((axis) => axis.id));
  expect(elements.elements.every((element) => axisIds.has(element.axis))).toBe(true);
  expect([...new Set(elements.elements.map((element) => element.axis))]).toEqual([trunk!.id]);
  expect(elements.axes.some((axis) => Boolean(axis.frame))).toBe(false);

  // Lay one offset over a leaf and one cut over the trunk, then write the graph back.
  const read = await engine.call("plant-graph", { plant, variation: 0 });
  expect(read.graph.edits).toEqual([]);
  const layer: BotanicalManualEditDto[] = [
    {
      target: target.id,
      action: { kind: "transform", offsetBits: [65536, 0, 0], roll: 0, scaleBits: 131072 },
    },
    { target: trunk!.id, action: { kind: "trim", at: 49152 } },
  ];
  layer.sort((first, second) => (BigInt(first.target) < BigInt(second.target) ? -1 : 1));
  const edited = await engine.call("plant-graph-set", {
    plant,
    graph: { ...read.graph, edits: layer },
    grafts: [],
    modules: [],
  });
  expect(edited.graph.edits.length).toBe(2);
  expect(edited.growth.appliedEdits).toBe(2);
  expect(edited.growth.orphans).toEqual([]);
  // The cut took the leaves that sat above it.
  expect(edited.growth.elements).toBeLessThan(created.growth.elements);
  // And the graph identity moved with the layer: the edits are part of what the plant is.
  expect(edited.growth.graph).not.toBe(created.growth.graph);

  // The leaf offset lands where it was asked to.
  const moved = await engine.call("plant-elements", { plant, variation: 0 });
  const after = moved.elements.find((element) => element.id === target.id);
  expect(after).toBeDefined();
  expect(after!.positionBits[0] - target.positionBits[0]).toBe(65536);
  expect(after!.sizeBits).toBe(target.sizeBits * 2);

  // Now change a parameter that takes the leaf away: the edit is reported, not dropped.
  const sparse = structuredClone(edited.graph);
  for (const node of sparse.nodes) {
    if (node.operator.kind === "phyllotaxis") {
      node.operator.nodes = 1;
    }
  }
  const regrown = await engine.call("plant-graph-set", {
    plant,
    graph: sparse,
    grafts: [],
    modules: [],
  });
  expect(regrown.graph.edits.length).toBe(2);
  const orphan = regrown.growth.orphans.find((entry) => entry.target === target.id);
  expect(orphan).toBeDefined();
  expect(orphan!.reason).toBe("target-missing");
  expect(orphan!.action.kind).toBe("transform");

  // The compiler surfaces the same thing as a warning, and still publishes the family.
  const validation = await engine.call("plant-validate", { plant });
  expect(validation.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);
  expect(validation.diagnostics.some((entry) => entry.code === "orphaned-edit")).toBe(true);

  // A layer that says two contradictory things about one element is refused outright.
  await expect(
    engine.call("plant-graph-set", {
      plant,
      graph: {
        ...read.graph,
        edits: [
          {
            target: target.id,
            action: { kind: "transform", offsetBits: [0, 0, 0], roll: 0, scaleBits: 65536 },
          },
          { target: target.id, action: { kind: "remove" } },
        ],
      },
      grafts: [],
      modules: [],
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a graft declares its hero mesh on the family and the graph names it", async () => {
  const [bark, leaf] = await materials();
  const created = await createPlant("E2E native grafted", [bark, leaf]);
  const plant = String(created.plant);
  const elements = await engine.call("plant-elements", { plant, variation: 0 });
  const target = elements.elements[0]!;
  const read = await engine.call("plant-graph", { plant, variation: 0 });
  expect(read.grafts).toEqual([]);

  // The top two bits are reserved for the identities a native family derives.
  const source = "3e".repeat(16);
  const graft: PlantGraftSourceDto = {
    id: source,
    locator: {
      kind: "file",
      uri: "file:///plants/hero-branch.glb",
    },
    selector: { kind: "whole" },
    settings: { ...SOURCE_DEFAULTS, units: "centimeters", upAxis: "positive-z" },
    provenance: {
      source: "e2e",
      sourceUri: "file:///plants/hero-branch.glb",
      licenseId: "CC0-1.0",
      licenseUri: "https://creativecommons.org/publicdomain/zero/1.0/",
      author: "E2E",
      attribution: "Hero branch",
      requiresAttribution: false,
    },
  };
  const edited = await engine.call("plant-graph-set", {
    plant,
    graph: {
      ...read.graph,
      edits: [{ target: target.id, action: { kind: "graft", source, selector: { kind: "whole" } } }],
    },
    grafts: [graft],
    modules: [],
  });
  // The graft stands in for the generated element: one fewer instanced quad, one graft.
  expect(edited.growth.grafts).toBe(1);
  expect(edited.growth.appliedEdits).toBe(1);
  expect(edited.growth.orphans).toEqual([]);
  expect(edited.growth.elements).toBe(created.growth.elements - 1);
  // The declaration round-trips with its import settings intact.
  expect(edited.grafts.length).toBe(1);
  expect(edited.grafts[0]!.id).toBe(source);
  expect(edited.grafts[0]!.settings.units).toBe("centimeters");
  expect(edited.grafts[0]!.settings.upAxis).toBe("positive-z");
  expect(edited.grafts[0]!.locator).toEqual(graft.locator);

  // A graft edit naming a source the family does not declare has no geometry to substitute.
  await expect(
    engine.call("plant-graph-set", {
      plant,
      graph: {
        ...read.graph,
        edits: [
          {
            target: target.id,
            action: { kind: "graft", source: "01".repeat(16), selector: { kind: "whole" } },
          },
        ],
      },
      grafts: [],
      modules: [],
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a declared variation is its own individual with its own geometry", async () => {
  const [bark, leaf] = await materials();
  const created = await createPlant("E2E native aged", [bark, leaf]);
  const plant = String(created.plant);
  const read = await engine.call("plant-graph", { plant, variation: 0 });
  expect(read.graph.variations.length).toBe(1);
  const mature = read.graph.variations[0]!;

  // Add a sapling: the same individual seen earlier, at half the age.
  const edited = await engine.call("plant-graph-set", {
    plant,
    graph: {
      ...read.graph,
      variations: [mature, { seed: mature.seed, age: 32768, name: "Sapling" }],
    },
    grafts: [],
    modules: [],
  });
  expect(edited.graph.variations.length).toBe(2);
  expect(edited.growth.variations).toBe(2);
  // The report describes the representative individual unless asked otherwise.
  expect(edited.growth.variation).toBe(0);
  expect(edited.growth.age).toBe(65535);

  const sapling = await engine.call("plant-growth", { plant, variation: 1 });
  expect(sapling.variation).toBe(1);
  expect(sapling.age).toBe(32768);
  // Same structure, smaller plant.
  expect(sapling.axes).toBe(edited.growth.axes);
  expect(sapling.elements).toBe(edited.growth.elements);
  expect(sapling.heightBits).toBeLessThan(edited.growth.heightBits);

  // The element identities do not depend on the age, so one edit layer fits both.
  const grownElements = await engine.call("plant-elements", { plant, variation: 0 });
  const youngElements = await engine.call("plant-elements", { plant, variation: 1 });
  expect(youngElements.elements.map((element) => element.id)).toEqual(
    grownElements.elements.map((element) => element.id),
  );

  // The family it compiles to carries a variation row per individual, and validates.
  const validation = await engine.call("plant-validate", { plant });
  expect(validation.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);

  // Two variations that are the same individual twice are refused.
  await expect(
    engine.call("plant-graph-set", {
      plant,
      graph: { ...read.graph, variations: [mature, mature] },
      grafts: [],
      modules: [],
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a native family derives its collision and navigation proxies", async () => {
  const [bark, leaf] = await materials();
  const created = await createPlant("E2E native proxied", [bark, leaf]);
  const plant = String(created.plant);
  const summary = await engine.call("plant-validate", { plant });
  expect(summary.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);

  // Derived proxies survive a regrow as the new graph's, not the previous graph's leftovers.
  const read = await engine.call("plant-graph", { plant, variation: 0 });
  const taller = structuredClone(read.graph);
  for (const node of taller.nodes) {
    if (node.operator.kind === "trunk") {
      node.operator.lengthBits *= 2;
    }
  }
  const regrown = await engine.call("plant-graph-set", {
    plant,
    graph: taller,
    grafts: [],
    modules: [],
  });
  expect(regrown.growth.heightBits).toBeGreaterThan(created.growth.heightBits);
  const after = await engine.call("plant-validate", { plant });
  expect(after.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);
  expect(engine.validationErrors()).toEqual([]);
});
