/// The shared typed node-graph canvas: React Flow chrome (card nodes with pin rows,
/// sky target / emerald source handles), replace-occupied-input connection semantics,
/// and the right-click add-node palette — parameterized by a schema so the material,
/// biome, and future botanical editors stay separate type systems on one surface.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import {
  addEdge,
  Background,
  type Connection,
  Controls,
  type Edge,
  type EdgeChange,
  Handle,
  type Node as FlowNodeBase,
  type NodeChange,
  type NodeProps,
  type NodeTypes,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { humanizeFieldName } from "../../lib/humanize";

/// The minimum shape every schema's node spec provides.
export interface GraphNodeSpecBase {
  type: string;
  label: string;
  category: string;
  inputs: string[];
  outputs: string[];
  defaultProps?: Record<string, unknown>;
}

/// The node payload every graph node carries.
export interface GraphNodeData<Spec extends GraphNodeSpecBase = GraphNodeSpecBase> {
  spec: Spec;
  props: Record<string, unknown>;
  [key: string]: unknown;
}

export type GraphFlowNode<Spec extends GraphNodeSpecBase = GraphNodeSpecBase> = FlowNodeBase<
  GraphNodeData<Spec>
>;

/// One graph vocabulary: its node specs, palette order, and (optionally) a custom
/// node-body editor for specs that edit inline instead of showing pin rows.
export interface GraphCanvasSchema {
  specs: Record<string, GraphNodeSpecBase>;
  categories: string[];
  /// Renders the inline editor body for a node, or null for the default pin rows.
  renderEditor?: (args: {
    id: string;
    spec: GraphNodeSpecBase;
    props: Record<string, unknown>;
    updateProps: (id: string, props: Record<string, unknown>) => void;
  }) => React.ReactNode | null;
}

export interface GraphCanvasProps {
  nodes: GraphFlowNode[];
  edges: Edge[];
  onNodesChange: (changes: NodeChange<GraphFlowNode>[]) => void;
  onEdgesChange: (changes: EdgeChange<Edge>[]) => void;
  /// Applies a functional edge mutation (connections replace occupied inputs).
  setEdges: (mutate: (edges: Edge[]) => Edge[]) => void;
  /// Updates one node's props payload (inline editors call it).
  updateProps: (id: string, props: Record<string, unknown>) => void;
  /// Adds a node of `type` at the flow-space position (the palette converts the
  /// screen point); omit to hide the add palette (a read-only vocabulary).
  onAddNode?: (type: string, position: { x: number; y: number }) => void;
  schema: GraphCanvasSchema;
  /// Disables connecting/adding (inspection surfaces).
  readOnly?: boolean;
}

interface NodeCallbacks {
  updateProps: (id: string, props: Record<string, unknown>) => void;
  schema: GraphCanvasSchema;
}
const NodeCallbacksContext = createContext<NodeCallbacks>({
  updateProps: () => {},
  schema: { specs: {}, categories: [] },
});

/// Pin labels are Sentence case (humanizeFieldName), but single-letter math pins stay lowercase.
function pinLabel(pin: string): string {
  return pin.length === 1 ? pin : humanizeFieldName(pin);
}

/// The single output handle + label, vertically centered in its row (inline-editor
/// nodes anchor their one output beside the editor, not in a separate pin row).
export function OutputAnchor({ pin }: { pin: string }) {
  return (
    <>
      <span className="ml-auto pr-3 text-muted-foreground">{pinLabel(pin)}</span>
      <Handle
        type="source"
        position={Position.Right}
        id={pin}
        className="!h-3 !w-3 !border-0 !bg-emerald-400"
      />
    </>
  );
}

/// One graph node card: the schema's inline editor when it provides one, else a row
/// per pin — inputs left (sky), outputs right (emerald), handles centered on labels.
function SchemaNode({ id, data }: NodeProps<GraphFlowNode>) {
  const { spec, props } = data;
  const { updateProps, schema } = useContext(NodeCallbacksContext);
  const editor = schema.renderEditor?.({ id, spec, props, updateProps }) ?? null;
  const editorOutput = spec.outputs[0];

  return (
    <div className="min-w-[150px] rounded border border-border bg-card text-[11px] text-foreground shadow">
      <div className="rounded-t border-b border-border bg-muted px-2 py-1 font-medium">
        {spec.label}
      </div>
      {editor !== null ? (
        <div className="relative flex items-center gap-2 py-2 pl-2">
          {editor}
          {editorOutput ? <OutputAnchor pin={editorOutput} /> : null}
        </div>
      ) : (
        <div className="py-1">
          {Array.from({ length: Math.max(spec.inputs.length, spec.outputs.length) }).map((_, k) => {
            const inPin = spec.inputs[k];
            const outPin = spec.outputs[k];
            return (
              <div
                key={`${inPin ?? ""}|${outPin ?? ""}`}
                className="relative flex h-6 items-center justify-between"
              >
                {inPin ? (
                  <Handle
                    type="target"
                    position={Position.Left}
                    id={inPin}
                    className="!h-3 !w-3 !border-0 !bg-sky-400"
                  />
                ) : null}
                <span className="pl-3 text-muted-foreground">{inPin ? pinLabel(inPin) : ""}</span>
                <span className="pr-3 text-muted-foreground">{outPin ? pinLabel(outPin) : ""}</span>
                {outPin ? (
                  <Handle
                    type="source"
                    position={Position.Right}
                    id={outPin}
                    className="!h-3 !w-3 !border-0 !bg-emerald-400"
                  />
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

const NODE_TYPES: NodeTypes = { saffron: SchemaNode };

function CanvasBody({
  nodes,
  edges,
  onNodesChange,
  onEdgesChange,
  setEdges,
  updateProps,
  onAddNode,
  schema,
  readOnly,
}: GraphCanvasProps) {
  // The node-create menu is a controlled, positioned element (not Radix) so every
  // right-click reopens it at the new cursor — Radix ContextMenu re-anchors
  // unreliably while open. menuPosRef feeds the add (screen → flow coords).
  const menuPosRef = useRef<{ x: number; y: number } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const reactFlow = useReactFlow();

  const onConnect = useCallback(
    (connection: Connection) => {
      if (readOnly || connection.source === connection.target) {
        return; // no self-loops
      }
      // One source per input pin: a new wire into an occupied input replaces it.
      setEdges((eds) => {
        const freed = eds.filter(
          (e) => !(e.target === connection.target && e.targetHandle === connection.targetHandle),
        );
        return addEdge(connection, freed);
      });
    },
    [readOnly, setEdges],
  );
  const isValidConnection = useCallback(
    (c: Connection | Edge) => !readOnly && c.source !== c.target,
    [readOnly],
  );
  const nodeCallbacks = useMemo(() => ({ updateProps, schema }), [updateProps, schema]);

  const palette = useMemo(() => {
    const cats: Record<string, string[]> = {};
    for (const category of schema.categories) {
      cats[category] = [];
    }
    for (const spec of Object.values(schema.specs)) {
      (cats[spec.category] ??= []).push(spec.type);
    }
    return cats;
  }, [schema]);

  // Close the create menu on a left-click outside it or Escape (a right-click is
  // handled by onPaneContextMenu, which repositions).
  useEffect(() => {
    if (!menu) {
      return;
    }
    const onDown = (e: MouseEvent): void => {
      if (e.button === 0 && menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenu(null);
      }
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        setMenu(null);
      }
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  return (
    <>
      <NodeCallbacksContext.Provider value={nodeCallbacks}>
        <ReactFlow
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onConnect={onConnect}
          isValidConnection={isValidConnection}
          nodeTypes={NODE_TYPES}
          colorMode="dark"
          fitView
          fitViewOptions={{ maxZoom: 1, padding: 0.3 }}
          proOptions={{ hideAttribution: true }}
          nodesConnectable={!readOnly}
          elementsSelectable
          onPaneContextMenu={(event) => {
            if (!onAddNode || readOnly) {
              return;
            }
            event.preventDefault();
            const point = { x: event.clientX, y: event.clientY };
            menuPosRef.current = point;
            setMenu(point);
          }}
          onMoveStart={() => setMenu(null)}
        >
          <Background />
          <Controls />
        </ReactFlow>
      </NodeCallbacksContext.Provider>
      {menu && onAddNode
        ? createPortal(
            <div
              ref={menuRef}
              className="fixed z-50 max-h-[80vh] w-44 overflow-y-auto rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md"
              style={{ left: menu.x, top: menu.y }}
            >
              {schema.categories.map((cat) => (
                <div key={cat}>
                  <div className="px-2 py-1 text-[10px] uppercase text-muted-foreground">{cat}</div>
                  {(palette[cat] ?? []).map((type) => (
                    <button
                      key={type}
                      type="button"
                      onClick={() => {
                        const screen = menuPosRef.current;
                        const position = screen
                          ? reactFlow.screenToFlowPosition(screen)
                          : { x: 200, y: 120 };
                        onAddNode(type, position);
                        setMenu(null);
                      }}
                      className="flex w-full items-center rounded-sm px-2 py-1.5 text-left text-[13px] hover:bg-accent hover:text-accent-foreground"
                    >
                      {schema.specs[type]?.label ?? type}
                    </button>
                  ))}
                </div>
              ))}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}

/// The exported canvas: owns its ReactFlowProvider so consumers stay hook-free.
export function GraphCanvas(props: GraphCanvasProps) {
  return (
    <ReactFlowProvider>
      <CanvasBody {...props} />
    </ReactFlowProvider>
  );
}
