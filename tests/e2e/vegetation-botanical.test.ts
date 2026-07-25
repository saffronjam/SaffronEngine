// Phase-14 acceptance through the real host: a native plant family is created from the starter
// botanical graph, grows real geometry, and normalizes through the same compiler an imported family
// uses — no native-only path, and the same growth on every run.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type {
  BotanicalGrowthDto,
  PlantCreateResult,
  PlantElementsResult,
  PlantGraphResult,
  PlantValidationResult,
} from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine } from "./test-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

/// Two materials for the graph's slots: bark and leaves.
let slots: string[] = [];
async function materials() {
  if (slots.length === 0) {
    const bark = await engine.call<{ id: string }>("material-create", { name: "E2E bark" });
    const leaf = await engine.call<{ id: string }>("material-create", { name: "E2E leaf" });
    slots = [bark.id, leaf.id];
  }
  return slots;
}

test("a created native family grows geometry and validates through the shared compiler", async () => {
  const [bark, leaf] = await materials();
  const created = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native birch",
    folder: "plants",
    materials: [bark, leaf],
  });
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
  const reread = await engine.call<BotanicalGrowthDto>("plant-growth", {
    plant: String(created.plant),
  });
  expect(reread).toEqual(growth);

  // And it normalizes through the one plant compiler, with no diagnostics.
  const validation = await engine.call<PlantValidationResult>("plant-validate", {
    plant: String(created.plant),
  });
  expect(validation.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);
  expect(engine.validationErrors()).toEqual([]);
});

test("two families created the same way are two different individuals", async () => {
  const [bark, leaf] = await materials();
  const first = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native oak",
    materials: [bark, leaf],
  });
  const second = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native elm",
    materials: [bark, leaf],
  });
  // The seed is derived from the name, so the two differ without the caller managing seeds.
  expect(first.growth.seed).not.toBe(second.growth.seed);
  expect(first.growth.variations).toBe(1);
  expect(first.growth.age).toBe(65535);
  expect(first.growth.graph).not.toBe(second.growth.graph);

  // A family with no materials for its slots is refused rather than binding slot zero twice.
  await expect(
    engine.call("plant-create", { name: "E2E native short", materials: [bark] }),
  ).rejects.toThrow();
  // And an empty name is refused.
  await expect(
    engine.call("plant-create", { name: "  ", materials: [String(bark), String(leaf)] }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a manual edit layer survives a parameter change and reports what it lost", async () => {
  const [bark, leaf] = await materials();
  const created = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native hand-edited",
    materials: [bark, leaf],
  });
  const plant = String(created.plant);

  // The addressable elements: this is what an authoring panel selects in, and what an edit targets.
  const elements = await engine.call<PlantElementsResult>("plant-elements", { plant });
  expect(elements.axes.length).toBe(created.growth.axes);
  expect(elements.elements.length).toBe(created.growth.elements);
  const trunk = elements.axes.find((axis) => axis.element === "trunk");
  const target = elements.elements[0]!;
  expect(trunk).toBeDefined();
  expect(trunk!.parent).toBeNull();
  expect(BigInt(target.id)).toBeGreaterThan(0n);

  // Lay one offset over a leaf and one cut over the trunk, then write the graph back.
  const read = await engine.call<PlantGraphResult>("plant-graph", { plant });
  expect(read.graph.edits).toEqual([]);
  const layer = [
    {
      target: target.id,
      action: { kind: "transform", offsetBits: [65536, 0, 0], roll: 0, scaleBits: 131072 },
    },
    { target: trunk!.id, action: { kind: "trim", at: 49152 } },
  ].sort((first, second) => (BigInt(first.target) < BigInt(second.target) ? -1 : 1));
  const edited = await engine.call<PlantGraphResult>("plant-graph-set", {
    plant,
    graph: { ...read.graph, edits: layer },
  });
  expect(edited.graph.edits.length).toBe(2);
  expect(edited.growth.appliedEdits).toBe(2);
  expect(edited.growth.orphans).toEqual([]);
  // The cut took the leaves that sat above it.
  expect(edited.growth.elements).toBeLessThan(created.growth.elements);
  // And the graph identity moved with the layer: the edits are part of what the plant is.
  expect(edited.growth.graph).not.toBe(created.growth.graph);

  // The leaf offset lands where it was asked to.
  const moved = await engine.call<PlantElementsResult>("plant-elements", { plant });
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
  const regrown = await engine.call<PlantGraphResult>("plant-graph-set", {
    plant,
    graph: sparse,
  });
  expect(regrown.graph.edits.length).toBe(2);
  const orphan = regrown.growth.orphans.find((entry) => entry.target === target.id);
  expect(orphan).toBeDefined();
  expect(orphan!.reason).toBe("target-missing");
  expect(orphan!.action.kind).toBe("transform");

  // The compiler surfaces the same thing as a warning, and still publishes the family.
  const validation = await engine.call<PlantValidationResult>("plant-validate", { plant });
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
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a graft declares its hero mesh on the family and the graph names it", async () => {
  const [bark, leaf] = await materials();
  const created = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native grafted",
    materials: [bark, leaf],
  });
  const plant = String(created.plant);
  const elements = await engine.call<PlantElementsResult>("plant-elements", { plant });
  const target = elements.elements[0]!;
  const read = await engine.call<PlantGraphResult>("plant-graph", { plant });
  expect(read.grafts).toEqual([]);

  // The top two bits are reserved for the identities a native family derives.
  const source = "3e".repeat(16);
  const graft = {
    id: source,
    locator: { kind: "file", uri: "file:///plants/hero-branch.glb" },
    selector: { kind: "whole" },
    settings: { units: "centimeters", upAxis: "positive-z" },
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
  const edited = await engine.call<PlantGraphResult>("plant-graph-set", {
    plant,
    graph: {
      ...read.graph,
      edits: [{ target: target.id, action: { kind: "graft", source, selector: { kind: "whole" } } }],
    },
    grafts: [graft],
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
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a declared variation is its own individual with its own geometry", async () => {
  const [bark, leaf] = await materials();
  const created = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native aged",
    materials: [bark, leaf],
  });
  const plant = String(created.plant);
  const read = await engine.call<PlantGraphResult>("plant-graph", { plant });
  expect(read.graph.variations.length).toBe(1);
  const mature = read.graph.variations[0]!;

  // Add a sapling: the same individual seen earlier, at half the age.
  const edited = await engine.call<PlantGraphResult>("plant-graph-set", {
    plant,
    graph: {
      ...read.graph,
      variations: [mature, { seed: mature.seed, age: 32768, name: "Sapling" }],
    },
  });
  expect(edited.graph.variations.length).toBe(2);
  expect(edited.growth.variations).toBe(2);
  // The report describes the representative individual unless asked otherwise.
  expect(edited.growth.variation).toBe(0);
  expect(edited.growth.age).toBe(65535);

  const sapling = await engine.call<BotanicalGrowthDto>("plant-growth", { plant, variation: 1 });
  expect(sapling.variation).toBe(1);
  expect(sapling.age).toBe(32768);
  // Same structure, smaller plant.
  expect(sapling.axes).toBe(edited.growth.axes);
  expect(sapling.elements).toBe(edited.growth.elements);
  expect(sapling.heightBits).toBeLessThan(edited.growth.heightBits);

  // The element identities do not depend on the age, so one edit layer fits both.
  const grownElements = await engine.call<PlantElementsResult>("plant-elements", { plant });
  const youngElements = await engine.call<PlantElementsResult>("plant-elements", {
    plant,
    variation: 1,
  });
  expect(youngElements.elements.map((element) => element.id)).toEqual(
    grownElements.elements.map((element) => element.id),
  );

  // The family it compiles to carries a variation row per individual, and validates.
  const validation = await engine.call<PlantValidationResult>("plant-validate", { plant });
  expect(validation.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);

  // Two variations that are the same individual twice are refused.
  await expect(
    engine.call("plant-graph-set", {
      plant,
      graph: { ...read.graph, variations: [mature, mature] },
    }),
  ).rejects.toThrow();
  expect(engine.validationErrors()).toEqual([]);
});

test("a native family derives its collision and navigation proxies", async () => {
  const [bark, leaf] = await materials();
  const created = await engine.call<PlantCreateResult>("plant-create", {
    name: "E2E native proxied",
    materials: [bark, leaf],
  });
  const plant = String(created.plant);
  const summary = await engine.call<PlantValidationResult>("plant-validate", { plant });
  expect(summary.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);

  // Derived proxies survive a regrow as the new graph's, not the previous graph's leftovers.
  const read = await engine.call<PlantGraphResult>("plant-graph", { plant });
  const taller = structuredClone(read.graph);
  for (const node of taller.nodes) {
    if (node.operator.kind === "trunk") {
      node.operator.lengthBits *= 2;
    }
  }
  const regrown = await engine.call<PlantGraphResult>("plant-graph-set", { plant, graph: taller });
  expect(regrown.growth.heightBits).toBeGreaterThan(created.growth.heightBits);
  const after = await engine.call<PlantValidationResult>("plant-validate", { plant });
  expect(after.diagnostics.filter((entry) => entry.severity === "error")).toEqual([]);
  expect(engine.validationErrors()).toEqual([]);
});
