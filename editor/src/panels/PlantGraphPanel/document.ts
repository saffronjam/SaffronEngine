/// Canonical edits over a botanical graph document. Every write path the panel offers goes through
/// one of these, and each returns a whole replacement document — `plant-graph-set` is the single
/// write, and its inverse is the previous document replayed through it.
///
/// The engine refuses a document whose nodes are not in strictly ascending GUID order, whose edges
/// are not in canonical order, or whose edits are not ordered by (target, action tag), so ordering
/// happens here rather than being left to whatever order the UI produced.
import type {
  BotanicalEdgeDto,
  BotanicalEditActionDto,
  BotanicalGraphDto,
  BotanicalNodeDto,
  BotanicalOperatorDto,
  PlantModuleReferenceDto,
} from "../../protocol";
import {
  BOTANICAL_OPERATORS,
  defaultOperator,
  type BotanicalOperatorKind,
} from "./botanicalSchema";

function compareBig(a: string, b: string): number {
  const left = BigInt(a);
  const right = BigInt(b);
  return left < right ? -1 : left > right ? 1 : 0;
}

function compareEdges(a: BotanicalEdgeDto, b: BotanicalEdgeDto): number {
  return (
    compareBig(a.fromNode, b.fromNode) ||
    (a.fromPin < b.fromPin ? -1 : a.fromPin > b.fromPin ? 1 : 0) ||
    compareBig(a.toNode, b.toNode) ||
    (a.toPin < b.toPin ? -1 : a.toPin > b.toPin ? 1 : 0)
  );
}

const EDIT_TAGS: Record<BotanicalEditActionDto["kind"], number> = {
  transform: 0,
  trim: 1,
  remove: 2,
  graft: 3,
};

/// The document with every ordered collection back in canonical order.
function canonical(graph: BotanicalGraphDto): BotanicalGraphDto {
  return {
    ...graph,
    nodes: [...graph.nodes].sort((a, b) => compareBig(a.guid, b.guid)),
    edges: [...graph.edges].sort(compareEdges),
    edits: [...graph.edits].sort(
      (a, b) =>
        compareBig(a.target, b.target) || EDIT_TAGS[a.action.kind] - EDIT_TAGS[b.action.kind],
    ),
  };
}

/// Replaces one node's operator payload.
export function withOperator(
  graph: BotanicalGraphDto,
  guid: string,
  operator: BotanicalOperatorDto,
): BotanicalGraphDto {
  return canonical({
    ...graph,
    nodes: graph.nodes.map((node) => (node.guid === guid ? { ...node, operator } : node)),
  });
}

/// Adds a node of `kind` with a fresh GUID past every existing one. The schema version comes from
/// the document's own nodes, which the engine wrote — the editor never names a version constant.
export function withAddedNode(
  graph: BotanicalGraphDto,
  kind: BotanicalOperatorKind,
): { graph: BotanicalGraphDto; guid: string } {
  const guid = (
    graph.nodes.reduce((highest, node) => {
      const value = BigInt(node.guid);
      return value > highest ? value : highest;
    }, 0n) + 1n
  ).toString();
  const template = graph.nodes[0];
  const node: BotanicalNodeDto = {
    guid,
    version: template?.version ?? 1,
    semanticRevision: 1,
    operator: defaultOperator(kind),
  };
  return { graph: canonical({ ...graph, nodes: [...graph.nodes, node] }), guid };
}

/// Removes a node and every edge touching it.
export function withoutNode(graph: BotanicalGraphDto, guid: string): BotanicalGraphDto {
  return canonical({
    ...graph,
    nodes: graph.nodes.filter((node) => node.guid !== guid),
    edges: graph.edges.filter((edge) => edge.fromNode !== guid && edge.toNode !== guid),
  });
}

/// Connects two pins, replacing whatever already occupied the destination input.
export function withEdge(graph: BotanicalGraphDto, edge: BotanicalEdgeDto): BotanicalGraphDto {
  const kept = graph.edges.filter(
    (row) => !(row.toNode === edge.toNode && row.toPin === edge.toPin),
  );
  return canonical({ ...graph, edges: [...kept, edge] });
}

/// Disconnects one edge.
export function withoutEdge(graph: BotanicalGraphDto, edge: BotanicalEdgeDto): BotanicalGraphDto {
  return canonical({
    ...graph,
    edges: graph.edges.filter((row) => compareEdges(row, edge) !== 0),
  });
}

/// Lays one manual edit over the graph, replacing any edit the same action already made on that
/// element. A `remove` drops every other edit on the target: saying "delete this" and "move this"
/// at once is what the engine refuses, and the artist's last word is the one that stands.
export function withEdit(
  graph: BotanicalGraphDto,
  target: string,
  action: BotanicalEditActionDto,
): BotanicalGraphDto {
  const others = graph.edits.filter(
    (edit) =>
      edit.target !== target ||
      (action.kind !== "remove" &&
        edit.action.kind !== "remove" &&
        edit.action.kind !== action.kind),
  );
  return canonical({ ...graph, edits: [...others, { target, action }] });
}

/// Drops every manual edit on one element.
export function withoutEdits(graph: BotanicalGraphDto, target: string): BotanicalGraphDto {
  return canonical({ ...graph, edits: graph.edits.filter((edit) => edit.target !== target) });
}

/// Whether `from`'s output pin and `to`'s input pin carry the same domain, which is what the engine
/// checks before it grows anything.
export function pinsAgree(
  graph: BotanicalGraphDto,
  fromNode: string,
  fromPin: string,
  toNode: string,
  toPin: string,
): boolean {
  const source = graph.nodes.find((node) => node.guid === fromNode);
  const sink = graph.nodes.find((node) => node.guid === toNode);
  if (!source || !sink) {
    return false;
  }
  return (
    BOTANICAL_OPERATORS[source.operator.kind].outputs.includes(fromPin) &&
    BOTANICAL_OPERATORS[sink.operator.kind].inputs.includes(toPin) &&
    DOMAIN_OF[fromPin] === DOMAIN_OF[toPin]
  );
}

/// The domain each pin name carries. Pin names are unique per domain across the whole vocabulary,
/// which is what lets a connection be typed by name alone.
const DOMAIN_OF: Record<string, string> = {
  axes: "spines",
  frames: "frames",
  shells: "shells",
  elements: "elements",
};

/// The unit one `plant-graph-set` writes: the graph plus the module table its `module-call` nodes
/// bind to. Validation checks a call and its binding against each other, so they never move apart.
export interface PlantDocument {
  graph: BotanicalGraphDto;
  modules: PlantModuleReferenceDto[];
}

/// The engine's canonical module order, by call-site GUID.
function canonicalModules(modules: PlantModuleReferenceDto[]): PlantModuleReferenceDto[] {
  return [...modules].sort((a, b) =>
    a.callGuid < b.callGuid ? -1 : a.callGuid > b.callGuid ? 1 : 0,
  );
}

/// A call-site GUID no existing binding uses, as the 32 lowercase hex digits the wire requires.
function freshCallGuid(modules: PlantModuleReferenceDto[]): string {
  const highest = modules.reduce(
    (found, module) =>
      BigInt(`0x${module.callGuid}`) > found ? BigInt(`0x${module.callGuid}`) : found,
    0n,
  );
  return (highest + 1n).toString(16).padStart(32, "0");
}

/// Adds a module call: a fresh call site, the node that grows it, and the binding naming which
/// `.splant` to grow there. One operation, because the engine refuses a call with no binding and a
/// binding no call names.
export function withModuleCall(document: PlantDocument, plant: string): PlantDocument {
  const callGuid = freshCallGuid(document.modules);
  const added = withAddedNode(document.graph, "module-call");
  return {
    graph: withOperator(added.graph, added.guid, { kind: "module-call", callGuid }),
    modules: canonicalModules([
      ...document.modules,
      { callGuid, plant, variation: 0, scaleBits: 65_536 },
    ]),
  };
}

/// Drops a module call: the binding and the node that named it, together.
export function withoutModuleCall(document: PlantDocument, callGuid: string): PlantDocument {
  const node = document.graph.nodes.find(
    (row) => row.operator.kind === "module-call" && row.operator.callGuid === callGuid,
  );
  return {
    graph: node ? withoutNode(document.graph, node.guid) : document.graph,
    modules: document.modules.filter((module) => module.callGuid !== callGuid),
  };
}

/// Rebinds one call site, keeping the node that names it.
export function withModuleBinding(
  document: PlantDocument,
  callGuid: string,
  patch: Partial<Omit<PlantModuleReferenceDto, "callGuid">>,
): PlantDocument {
  return {
    ...document,
    modules: document.modules.map((module) =>
      module.callGuid === callGuid ? { ...module, ...patch } : module,
    ),
  };
}
