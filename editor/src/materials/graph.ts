// The material node-graph model shared between the engine wire format and the React Flow editor.
//
// Wire format (what the engine's emitGraphSurface / lowerGraphToParams consume, and what
// material-get / material-set-graph carry):
//   { nodes: [{ id, type, props? }], edges: [{ from: [nodeId, pin], to: [nodeId, pin] }] }
// The node `type` strings match the engine emitter's switch. Editor-only data (node
// canvas position) rides along in `props.editorPos`; the engine ignores unknown props.

import type { Edge, Node } from "@xyflow/react";

export interface GraphNode {
  id: string;
  type: string;
  props?: Record<string, unknown>;
}

export interface GraphEdge {
  from: [string, string];
  to: [string, string];
}

export interface MaterialGraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export type NodeCategory = "input" | "math" | "output";

export interface NodeSpec {
  type: string;
  label: string;
  category: NodeCategory;
  inputs: string[];
  outputs: string[];
  defaultProps?: Record<string, unknown>;
}

/// The palette: every node type the engine codegen emitter understands, with its pins. Keep this in
/// sync with the engine's emitGraphSurface.
export const NODE_SPECS: Record<string, NodeSpec> = {
  constant: {
    type: "constant",
    label: "Constant",
    category: "input",
    inputs: [],
    outputs: ["rgba"],
    defaultProps: { value: [1, 1, 1, 1] },
  },
  textureSlot: {
    type: "textureSlot",
    label: "Texture Slot",
    category: "input",
    inputs: [],
    outputs: ["rgba"],
    defaultProps: { slot: "albedo" },
  },
  uv: { type: "uv", label: "UV", category: "input", inputs: [], outputs: ["out"] },
  multiply: {
    type: "multiply",
    label: "Multiply",
    category: "math",
    inputs: ["a", "b"],
    outputs: ["rgba"],
  },
  add: { type: "add", label: "Add", category: "math", inputs: ["a", "b"], outputs: ["rgba"] },
  subtract: {
    type: "subtract",
    label: "Subtract",
    category: "math",
    inputs: ["a", "b"],
    outputs: ["rgba"],
  },
  divide: {
    type: "divide",
    label: "Divide",
    category: "math",
    inputs: ["a", "b"],
    outputs: ["rgba"],
  },
  lerp: {
    type: "lerp",
    label: "Lerp",
    category: "math",
    inputs: ["a", "b", "t"],
    outputs: ["rgba"],
  },
  smoothstep: {
    type: "smoothstep",
    label: "Smoothstep",
    category: "math",
    inputs: ["a", "b", "t"],
    outputs: ["rgba"],
  },
  step: { type: "step", label: "Step", category: "math", inputs: ["a", "b"], outputs: ["rgba"] },
  dot: { type: "dot", label: "Dot", category: "math", inputs: ["a", "b"], outputs: ["rgba"] },
  saturate: {
    type: "saturate",
    label: "Saturate",
    category: "math",
    inputs: ["a"],
    outputs: ["rgba"],
  },
  oneMinus: {
    type: "oneMinus",
    label: "One Minus",
    category: "math",
    inputs: ["a"],
    outputs: ["rgba"],
  },
  sin: { type: "sin", label: "Sin", category: "math", inputs: ["a"], outputs: ["rgba"] },
  cos: { type: "cos", label: "Cos", category: "math", inputs: ["a"], outputs: ["rgba"] },
  frac: { type: "frac", label: "Frac", category: "math", inputs: ["a"], outputs: ["rgba"] },
  materialOutput: {
    type: "materialOutput",
    label: "Material Output",
    category: "output",
    inputs: [
      "baseColor",
      "metallic",
      "roughness",
      "normal",
      "emissive",
      "occlusion",
      "opacity",
      "frontAlbedoResponse",
      "backAlbedoResponse",
      "thickness",
      "absorption",
      "transmission",
      "energyLimit",
    ],
    outputs: [],
  },
};

/// The texture slots a `textureSlot` node can sample, matching the engine emitter's slot switch.
/// The editor offers exactly these so a graph can't name an unknown slot.
export const TEXTURE_SLOTS = [
  "albedo",
  "metallicRoughness",
  "normal",
  "emissive",
  "occlusion",
  "height",
] as const;

export interface SaffronNodeData extends Record<string, unknown> {
  spec: NodeSpec;
  props: Record<string, unknown>;
}

export type FlowNode = Node<SaffronNodeData, "saffron">;

function posOf(
  props: Record<string, unknown> | undefined,
  fallbackIndex: number,
): { x: number; y: number } {
  const p = props?.editorPos;
  if (Array.isArray(p) && typeof p[0] === "number" && typeof p[1] === "number") {
    return { x: p[0], y: p[1] };
  }
  // Auto-layout when a graph carries no stored positions: a loose staggered column.
  return { x: 60 + (fallbackIndex % 3) * 240, y: 40 + Math.floor(fallbackIndex / 3) * 160 };
}

/// Wire graph -> React Flow nodes/edges. Unknown node types fall back to a bare spec so they still show.
export function graphToFlow(graph: MaterialGraph | undefined): {
  nodes: FlowNode[];
  edges: Edge[];
} {
  const nodes: FlowNode[] = (graph?.nodes ?? []).map((n, i) => {
    const spec = NODE_SPECS[n.type] ?? {
      type: n.type,
      label: n.type,
      category: "math" as NodeCategory,
      inputs: ["a"],
      outputs: ["rgba"],
    };
    const props = { ...(n.props ?? {}) };
    delete props.editorPos;
    return {
      id: n.id,
      type: "saffron",
      position: posOf(n.props, i),
      data: { spec, props },
    };
  });
  const edges: Edge[] = (graph?.edges ?? []).map((e, i) => ({
    id: `e${i}-${e.from[0]}.${e.from[1]}-${e.to[0]}.${e.to[1]}`,
    source: e.from[0],
    sourceHandle: e.from[1],
    target: e.to[0],
    targetHandle: e.to[1],
  }));
  return { nodes, edges };
}

/// React Flow nodes/edges -> wire graph, folding each node's canvas position back into props.editorPos.
export function flowToGraph(nodes: FlowNode[], edges: Edge[]): MaterialGraph {
  return {
    nodes: nodes.map((n) => ({
      id: n.id,
      type: n.data.spec.type,
      props: { ...n.data.props, editorPos: [Math.round(n.position.x), Math.round(n.position.y)] },
    })),
    edges: edges
      .filter((e) => e.sourceHandle && e.targetHandle)
      .map((e) => ({
        from: [e.source, e.sourceHandle as string],
        to: [e.target, e.targetHandle as string],
      })),
  };
}

let nodeCounter = 0;
/// A unique-enough node id for a freshly added node (graph-local, not a project Uuid).
export function freshNodeId(type: string): string {
  nodeCounter += 1;
  return `${type}_${nodeCounter}`;
}
