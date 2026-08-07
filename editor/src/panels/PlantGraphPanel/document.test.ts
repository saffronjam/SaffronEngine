import { describe, expect, test } from "bun:test";
import type { BotanicalGraphDto, BotanicalNodeDto } from "../../protocol";
import { defaultOperator } from "./botanicalSchema";
import {
  pinsAgree,
  withAddedNode,
  withEdge,
  withEdit,
  withModuleBinding,
  withModuleCall,
  withoutModuleCall,
  withOperator,
  withoutEdge,
  withoutEdits,
  withoutNode,
} from "./document";
import { buildStructure } from "./StructureTree";

/// One grown element identity: a decimal u128 wide enough that a lexicographic compare misreads it.
const ELEMENT = "212893012389084019283740192837401928";

function node(guid: string, kind: Parameters<typeof defaultOperator>[0]): BotanicalNodeDto {
  return { guid, version: 2, semanticRevision: 1, operator: defaultOperator(kind) };
}

/// A trunk feeding phyllotaxis feeding a branch, with the axes and shells reaching the family sink.
/// Nodes and edges are deliberately out of canonical order so every helper has ordering to fix.
function birch(): BotanicalGraphDto {
  return {
    variations: [{ seed: "7", age: 1, name: "Variation 1" }],
    nodes: [node("9", "family"), node("3", "phyllotaxis"), node("1", "trunk"), node("5", "branch")],
    edges: [
      { fromNode: "3", fromPin: "frames", toNode: "5", toPin: "frames" },
      { fromNode: "1", fromPin: "axes", toNode: "3", toPin: "axes" },
    ],
    edits: [],
  };
}

describe("botanical document edits", () => {
  test("every write leaves nodes, edges, and edits in the order the engine requires", () => {
    const written = withEdge(birch(), {
      fromNode: "5",
      fromPin: "axes",
      toNode: "9",
      toPin: "shells",
    });
    expect(written.nodes.map((row) => row.guid)).toEqual(["1", "3", "5", "9"]);
    expect(written.edges.map((row) => `${row.fromNode}->${row.toNode}`)).toEqual([
      "1->3",
      "3->5",
      "5->9",
    ]);

    // An element id is a decimal u128, so target order is numeric — a lexicographic sort would
    // put "10" before "9" and the engine would refuse the document.
    const edited = withEdit(withEdit(written, "10", { kind: "remove" }), "9", {
      kind: "trim",
      at: 100,
    });
    expect(edited.edits.map((row) => row.target)).toEqual(["9", "10"]);
  });

  test("a fresh node lands past every existing guid and starts inside the engine's bounds", () => {
    const added = withAddedNode(birch(), "tropism");
    expect(added.guid).toBe("10");
    const grown = added.graph.nodes.find((row) => row.guid === "10");
    expect(grown?.operator).toEqual({
      kind: "tropism",
      kindOf: "gravitropism",
      strength: 20_000,
      stimulusBits: [0, 65_536, 0],
      planeOffsetBits: 0,
    });
  });

  test("removing a node takes every edge that touched it", () => {
    const pruned = withoutNode(birch(), "3");
    expect(pruned.nodes.map((row) => row.guid)).toEqual(["1", "5", "9"]);
    expect(pruned.edges).toEqual([]);
  });

  test("a second wire into one input replaces the first", () => {
    const rewired = withEdge(birch(), {
      fromNode: "5",
      fromPin: "axes",
      toNode: "3",
      toPin: "axes",
    });
    expect(rewired.edges).toEqual([
      { fromNode: "3", fromPin: "frames", toNode: "5", toPin: "frames" },
      { fromNode: "5", fromPin: "axes", toNode: "3", toPin: "axes" },
    ]);
  });

  test("disconnecting drops exactly the named edge", () => {
    const cut = withoutEdge(birch(), {
      fromNode: "1",
      fromPin: "axes",
      toNode: "3",
      toPin: "axes",
    });
    expect(cut.edges).toEqual([{ fromNode: "3", fromPin: "frames", toNode: "5", toPin: "frames" }]);
  });

  test("only a matching pin domain connects", () => {
    const graph = birch();
    expect(pinsAgree(graph, "1", "axes", "3", "axes")).toBe(true);
    // Phyllotaxis emits frames; the branch takes frames but the family sink takes shells/elements.
    expect(pinsAgree(graph, "3", "frames", "9", "shells")).toBe(false);
    // A pin the operator does not declare is not a connection whatever its name.
    expect(pinsAgree(graph, "1", "frames", "3", "frames")).toBe(false);
    expect(pinsAgree(graph, "1", "axes", "404", "axes")).toBe(false);
  });

  test("a remove supersedes every other edit on the same element", () => {
    const moved = withEdit(birch(), ELEMENT, {
      kind: "transform",
      offsetBits: [0, 65_536, 0],
      roll: 0,
      scaleBits: 65_536,
    });
    const trimmed = withEdit(moved, ELEMENT, { kind: "trim", at: 100 });
    expect(trimmed.edits).toHaveLength(2);
    const removed = withEdit(trimmed, ELEMENT, { kind: "remove" });
    expect(removed.edits).toEqual([{ target: ELEMENT, action: { kind: "remove" } }]);
    expect(withoutEdits(removed, ELEMENT).edits).toEqual([]);
  });

  test("the same action twice on one element keeps the artist's last word", () => {
    const once = withEdit(birch(), ELEMENT, { kind: "trim", at: 100 });
    const twice = withEdit(once, ELEMENT, { kind: "trim", at: 400 });
    expect(twice.edits).toEqual([{ target: ELEMENT, action: { kind: "trim", at: 400 } }]);
  });

  test("a parameter write touches one node and leaves the rest byte-identical", () => {
    const graph = birch();
    const written = withOperator(graph, "1", {
      ...defaultOperator("trunk"),
      lengthBits: 9 * 65_536,
    });
    const trunk = written.nodes.find((row) => row.guid === "1")!.operator;
    expect(trunk.kind === "trunk" && trunk.lengthBits).toBe(9 * 65_536);
    expect(written.nodes.filter((row) => row.guid !== "1")).toEqual(
      [...graph.nodes]
        .sort((a, b) => Number(a.guid) - Number(b.guid))
        .filter((row) => row.guid !== "1"),
    );
  });
});

describe("grown structure", () => {
  /// The shape `plant-create` actually mints: a trunk, roots hanging off it, and leaves on spiral
  /// frames up the trunk that no axis grew from — so the host axis has to come off the wire.
  test("axes nest under their parent and a placement hangs under the axis carrying its frame", () => {
    const roots = buildStructure(
      [
        {
          id: "trunk",
          element: "trunk",
          baseBits: [0, 0, 0],
          tipBits: [0, 262_144, 0],
          baseRadiusBits: 9_830,
          points: 7,
        },
        {
          id: "root0",
          parent: "trunk",
          element: "root",
          baseBits: [0, 0, 0],
          tipBits: [26_214, -11_796, 0],
          baseRadiusBits: 4_915,
          points: 3,
        },
        {
          id: "root1",
          parent: "trunk",
          element: "root",
          baseBits: [0, 0, 0],
          tipBits: [-13_107, -11_796, 22_701],
          baseRadiusBits: 4_915,
          points: 3,
        },
      ],
      ["f0", "f1", "f2", "f3", "f4"].map((frame, index) => ({
        id: `leaf${index}`,
        frame,
        axis: "trunk",
        element: "leaf" as const,
        materialSlot: 1,
        positionBits: [0, 68_157 + index * 34_078, 0] as [number, number, number],
        sizeBits: 6_553,
        roll: 0,
      })),
    );
    expect(roots).toHaveLength(1);
    expect(roots[0]!.id).toBe("trunk");
    expect(roots[0]!.children.map((child) => child.id)).toEqual([
      "root0",
      "root1",
      "leaf0",
      "leaf1",
      "leaf2",
      "leaf3",
      "leaf4",
    ]);
    expect(roots[0]!.children.every((child) => child.depth === 1)).toBe(true);
  });

  test("a placement whose axis the report does not carry roots the tree", () => {
    const roots = buildStructure(
      [
        {
          id: "trunk",
          element: "trunk",
          baseBits: [0, 0, 0],
          tipBits: [0, 262_144, 0],
          baseRadiusBits: 9_830,
          points: 7,
        },
      ],
      [
        {
          id: "leaf",
          frame: "f0",
          axis: "pruned",
          element: "leaf",
          materialSlot: 1,
          positionBits: [0, 131_072, 0],
          sizeBits: 6_553,
          roll: 0,
        },
      ],
    );
    expect(roots.map((row) => row.id)).toEqual(["trunk", "leaf"]);
  });

  test("an axis whose parent is absent from the report roots the tree", () => {
    const roots = buildStructure(
      [
        {
          id: "orphan",
          parent: "pruned",
          element: "branch",
          baseBits: [0, 0, 0],
          tipBits: [0, 65_536, 0],
          baseRadiusBits: 1_000,
          points: 2,
        },
      ],
      [],
    );
    expect(roots.map((row) => row.id)).toEqual(["orphan"]);
  });
});

describe("module calls", () => {
  const MODULE = "00000000-0000-0000-0000-0000000000aa";

  test("adding a module call mints the node and its binding together", () => {
    const added = withModuleCall({ graph: birch(), modules: [] }, MODULE);
    expect(added.modules).toEqual([
      {
        callGuid: "00000000000000000000000000000001",
        plant: MODULE,
        variation: 0,
        scaleBits: 65_536,
      },
    ]);
    const call = added.graph.nodes.find((row) => row.operator.kind === "module-call");
    expect(call?.operator).toEqual({
      kind: "module-call",
      callGuid: "00000000000000000000000000000001",
    });
  });

  test("a second call site takes the next GUID, and the table stays in call order", () => {
    const twice = withModuleCall(withModuleCall({ graph: birch(), modules: [] }, MODULE), MODULE);
    expect(twice.modules.map((row) => row.callGuid)).toEqual([
      "00000000000000000000000000000001",
      "00000000000000000000000000000002",
    ]);
    expect(twice.graph.nodes.filter((row) => row.operator.kind === "module-call")).toHaveLength(2);
  });

  test("removing a call site takes its node with it", () => {
    const added = withModuleCall({ graph: birch(), modules: [] }, MODULE);
    const dropped = withoutModuleCall(added, "00000000000000000000000000000001");
    expect(dropped.modules).toEqual([]);
    expect(dropped.graph.nodes.some((row) => row.operator.kind === "module-call")).toBe(false);
  });

  test("rebinding one call site leaves the graph and every other binding alone", () => {
    const twice = withModuleCall(withModuleCall({ graph: birch(), modules: [] }, MODULE), MODULE);
    const rebound = withModuleBinding(twice, "00000000000000000000000000000002", {
      variation: 3,
      scaleBits: 32_768,
    });
    expect(rebound.graph).toEqual(twice.graph);
    expect(rebound.modules[0]).toEqual(twice.modules[0]!);
    expect(rebound.modules[1]).toEqual({
      callGuid: "00000000000000000000000000000002",
      plant: MODULE,
      variation: 3,
      scaleBits: 32_768,
    });
  });
});
