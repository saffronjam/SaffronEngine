/// The botanical graph on the shared node canvas. Nodes are the document's operators, wired through
/// typed pins; every canvas gesture — add, delete, connect, disconnect — is one semantic document
/// edit committed through the panel's single write path, never a local model the panel later saves.
///
/// The document carries no layout, so columns come from each node's longest path from a source and a
/// dragged node's position is remembered here only.
import { useCallback, useMemo, useState } from "react";
import type { Edge, EdgeChange, NodeChange } from "@xyflow/react";
import type { BotanicalEdgeDto, BotanicalGraphDto } from "../../protocol";
import {
  GraphCanvas,
  type GraphCanvasSchema,
  type GraphFlowNode,
  type GraphNodeSpecBase,
} from "../../components/graph/GraphCanvas";
import { notifyError } from "../../lib/flash";
import {
  BOTANICAL_CATEGORIES,
  BOTANICAL_OPERATORS,
  BOTANICAL_PALETTE,
  type BotanicalOperatorKind,
} from "./botanicalSchema";
import { pinsAgree, withAddedNode, withEdge, withoutEdge, withoutNode } from "./document";

const COLUMN_WIDTH = 210;
const ROW_HEIGHT = 120;

function specOf(kind: BotanicalOperatorKind): GraphNodeSpecBase {
  const operator = BOTANICAL_OPERATORS[kind];
  return {
    type: kind,
    label: operator.label,
    category: operator.category,
    inputs: operator.inputs,
    outputs: operator.outputs,
  };
}

/// The add palette: every operator but the family sink, which a document already carries exactly one
/// of and a second of has no defined result.
const CANVAS_SCHEMA: GraphCanvasSchema = {
  specs: Object.fromEntries(BOTANICAL_PALETTE.map((kind) => [kind, specOf(kind)])),
  categories: BOTANICAL_CATEGORIES,
};

/// An edge id that decodes back to the wire edge, so a canvas removal names the row to drop without
/// a side table to keep in step.
function edgeId(edge: BotanicalEdgeDto): string {
  return [edge.fromNode, edge.fromPin, edge.toNode, edge.toPin].join("|");
}

function decodeEdgeId(id: string): BotanicalEdgeDto | null {
  const [fromNode, fromPin, toNode, toPin] = id.split("|");
  if (!fromNode || !fromPin || !toNode || !toPin) {
    return null;
  }
  return { fromNode, fromPin, toNode, toPin };
}

/// Each node's column: the longest path from a source, relaxed until it settles. A botanical
/// document is acyclic — the engine refuses anything else — so the pass count bounds the walk.
function columns(graph: BotanicalGraphDto): Map<string, number> {
  const depth = new Map(graph.nodes.map((node) => [node.guid, 0]));
  for (let pass = 0; pass < graph.nodes.length; pass += 1) {
    let moved = false;
    for (const edge of graph.edges) {
      const next = (depth.get(edge.fromNode) ?? 0) + 1;
      if (next > (depth.get(edge.toNode) ?? 0)) {
        depth.set(edge.toNode, next);
        moved = true;
      }
    }
    if (!moved) {
      break;
    }
  }
  return depth;
}

export function GraphEditor({
  graph,
  selected,
  onSelect,
  onApply,
  busy,
}: {
  graph: BotanicalGraphDto;
  selected: string | null;
  onSelect: (guid: string | null) => void;
  onApply: (label: string, next: BotanicalGraphDto) => void;
  busy: boolean;
}) {
  const [dragged, setDragged] = useState<ReadonlyMap<string, { x: number; y: number }>>(new Map());

  const nodes = useMemo<GraphFlowNode[]>(() => {
    const depth = columns(graph);
    const filled = new Map<number, number>();
    return graph.nodes.map((node) => {
      const column = depth.get(node.guid) ?? 0;
      const row = filled.get(column) ?? 0;
      filled.set(column, row + 1);
      return {
        id: node.guid,
        type: "saffron",
        position: dragged.get(node.guid) ?? { x: column * COLUMN_WIDTH, y: row * ROW_HEIGHT },
        selected: selected === node.guid,
        data: { spec: specOf(node.operator.kind), props: {} },
      };
    });
  }, [dragged, graph, selected]);

  const edges = useMemo<Edge[]>(
    () =>
      graph.edges.map((edge) => ({
        id: edgeId(edge),
        source: edge.fromNode,
        sourceHandle: edge.fromPin,
        target: edge.toNode,
        targetHandle: edge.toPin,
      })),
    [graph.edges],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange<GraphFlowNode>[]) => {
      const moves = new Map(dragged);
      let moved = false;
      for (const change of changes) {
        if (change.type === "position" && change.position) {
          moves.set(change.id, change.position);
          moved = true;
        } else if (change.type === "select") {
          onSelect(change.selected ? change.id : null);
        } else if (change.type === "remove" && !busy) {
          // The family sink is the document's one required output; dropping it would leave a graph
          // the engine refuses, so the canvas keeps it.
          const node = graph.nodes.find((row) => row.guid === change.id);
          if (!node || node.operator.kind === "family") {
            notifyError("The family sink cannot be removed");
            continue;
          }
          onSelect(null);
          onApply("Remove node", withoutNode(graph, change.id));
        }
      }
      if (moved) {
        setDragged(moves);
      }
    },
    [busy, dragged, graph, onApply, onSelect],
  );

  const onEdgesChange = useCallback(
    (changes: EdgeChange<Edge>[]) => {
      if (busy) {
        return;
      }
      for (const change of changes) {
        if (change.type !== "remove") {
          continue;
        }
        const edge = decodeEdgeId(change.id);
        if (edge) {
          onApply("Disconnect nodes", withoutEdge(graph, edge));
        }
      }
    },
    [busy, graph, onApply],
  );

  /// The canvas hands back the edge list a connection produced; the row that is not already in the
  /// document is the one the artist drew.
  const setEdges = useCallback(
    (mutate: (rows: Edge[]) => Edge[]) => {
      if (busy) {
        return;
      }
      const known = new Set(edges.map((row) => row.id));
      const added = mutate(edges).find(
        (row) => !known.has(row.id) && row.sourceHandle && row.targetHandle,
      );
      if (!added) {
        return;
      }
      const edge: BotanicalEdgeDto = {
        fromNode: added.source,
        fromPin: added.sourceHandle!,
        toNode: added.target,
        toPin: added.targetHandle!,
      };
      if (!pinsAgree(graph, edge.fromNode, edge.fromPin, edge.toNode, edge.toPin)) {
        notifyError(`${edge.fromPin} does not feed ${edge.toPin}`);
        return;
      }
      onApply("Connect nodes", withEdge(graph, edge));
    },
    [busy, edges, graph, onApply],
  );

  const addNode = useCallback(
    (type: string) => {
      if (busy) {
        return;
      }
      const added = withAddedNode(graph, type as BotanicalOperatorKind);
      onSelect(added.guid);
      onApply(
        `Add ${BOTANICAL_OPERATORS[type as BotanicalOperatorKind].label.toLowerCase()}`,
        added.graph,
      );
    },
    [busy, graph, onApply, onSelect],
  );

  return (
    <GraphCanvas
      nodes={nodes}
      edges={edges}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      setEdges={setEdges}
      updateProps={() => {}}
      onAddNode={addNode}
      schema={CANVAS_SCHEMA}
    />
  );
}
