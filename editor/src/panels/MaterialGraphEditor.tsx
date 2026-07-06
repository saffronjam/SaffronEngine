/// The node-graph material editor: a React Flow canvas + a LIVE 3D preview sphere, hosted as a main
/// tab (see App.tsx / openMaterialGraphTab). Loads a material's stored graph (material-get), lets you
/// add (right-click the canvas) / connect / edit nodes, and auto-applies changes (debounced) via
/// material-set-graph. The preview pane is the modal `assetPreview` subsurface (the same view the
/// asset editor drives) showing the edited `.smat` on a built-in sphere under real IBL — orbit it
/// (pan-only, no dolly); it re-renders on its own each frame as the material cache is invalidated by
/// the apply, so there is no readback round-trip. "Compile" forces codegen (material-compile-graph)
/// for procedural graphs that don't fold to params. Node types mirror the engine emitter
/// (materials/graph.ts).
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
  Handle,
  type NodeProps,
  type NodeTypes,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Hammer } from "lucide-react";
import { client } from "../control/client";
import { ColorField } from "../components/ColorField";
import { useSubsurfaceBounds } from "../lib/useSubsurfaceBounds";
import { useOrbitCamera } from "../lib/useOrbitCamera";
import { errorText, notifyError } from "../lib/flash";
import { humanizeFieldName } from "../lib/humanize";
import {
  type FlowNode,
  flowToGraph,
  freshNodeId,
  graphToFlow,
  type MaterialGraph,
  type NodeCategory,
  NODE_SPECS,
  type SaffronNodeData,
  TEXTURE_SLOTS,
} from "../materials/graph";
import { useTabSnapshotHistory } from "../lib/useTabSnapshotHistory";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/// Stable, order-insensitive equality of two graphs (nodes/edges sorted by id/endpoints),
/// so a settle that only reorders the arrays or rounds a position back records no entry.
export function graphsEqual(a: MaterialGraph, b: MaterialGraph): boolean {
  const key = (g: MaterialGraph): string => {
    const nodes = [...g.nodes].sort((x, y) => x.id.localeCompare(y.id));
    const edges = [...g.edges].sort((x, y) =>
      `${x.from[0]}.${x.from[1]}>${x.to[0]}.${x.to[1]}`.localeCompare(
        `${y.from[0]}.${y.from[1]}>${y.to[0]}.${y.to[1]}`,
      ),
    );
    return JSON.stringify({ nodes, edges });
  };
  return key(a) === key(b);
}

interface NodeCallbacks {
  updateProps: (id: string, props: Record<string, unknown>) => void;
}
const NodeCallbacksContext = createContext<NodeCallbacks>({ updateProps: () => {} });

/// Pin labels are Sentence case (humanizeFieldName), but single-letter math pins (a/b/t) stay lowercase.
function pinLabel(pin: string): string {
  return pin.length === 1 ? pin : humanizeFieldName(pin);
}

/// The single output handle + label, vertically centered in its row (used by the editor-node layout so
/// the anchor sits beside the editor, not in a separate pin row above it).
function OutputAnchor({ pin }: { pin: string }) {
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

/// One graph node card. Editor nodes (constant → color picker, texture → slot select) put the editor in a
/// single body row with the output anchor centered on its right. Other nodes are pins only: a row per pin,
/// inputs on the left + outputs on the right, the handle centered on its label.
function SaffronNode({ id, data }: NodeProps<FlowNode>) {
  const { spec, props } = data as SaffronNodeData;
  const { updateProps } = useContext(NodeCallbacksContext);
  const hasEditor = spec.type === "constant" || spec.type === "textureSlot";
  const editorOutput = spec.outputs[0];

  return (
    <div className="min-w-[150px] rounded border border-border bg-card text-[11px] text-foreground shadow">
      <div className="rounded-t border-b border-border bg-muted px-2 py-1 font-medium">
        {spec.label}
      </div>
      {hasEditor ? (
        <div className="relative flex items-center gap-2 py-2 pl-2">
          {spec.type === "constant" ? (
            (() => {
              const value = (props.value as number[] | undefined) ?? [1, 1, 1, 1];
              // Fixed width so the inline rgba channels shrink to fit instead of expanding to the
              // native number-input width (which blows the node out to ~600px).
              return (
                <div className="nodrag w-64">
                  <ColorField
                    kind="color4"
                    value={{
                      x: value[0] ?? 0,
                      y: value[1] ?? 0,
                      z: value[2] ?? 0,
                      w: value[3] ?? 1,
                    }}
                    onChange={(patch) => {
                      const next = [...value];
                      if (patch.x !== undefined) next[0] = patch.x;
                      if (patch.y !== undefined) next[1] = patch.y;
                      if (patch.z !== undefined) next[2] = patch.z;
                      if (patch.w !== undefined) next[3] = patch.w;
                      updateProps(id, { ...props, value: next });
                    }}
                  />
                </div>
              );
            })()
          ) : (
            <Select
              value={(props.slot as string | undefined) ?? "albedo"}
              onValueChange={(slot) => updateProps(id, { ...props, slot })}
            >
              <SelectTrigger size="sm" className="nodrag h-7 w-36 text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TEXTURE_SLOTS.map((slot) => (
                  <SelectItem key={slot} value={slot} className="text-[11px]">
                    {humanizeFieldName(slot)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
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

const NODE_TYPES: NodeTypes = { saffron: SaffronNode };
const PALETTE_CATEGORIES: NodeCategory[] = ["input", "math", "output"];

function GraphCanvas({ materialId }: { materialId: string }) {
  const [nodes, setNodes, onNodesChange] = useNodesState<FlowNode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const [status, setStatus] = useState<string>("");
  const loadedRef = useRef(false);

  // The live preview sphere: its own transparent hole down to the modal `assetPreview` subsurface,
  // orbit-only (pan around the framed sphere, no dolly). This tab renders only while active (App.tsx),
  // so mount == activate: enter the material preview on mount, exit on unmount.
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [previewReady, setPreviewReady] = useState(false);
  const orbit = useOrbitCamera({ enableZoom: false });
  useSubsurfaceBounds(hostRef, "assetPreview", { enabled: previewReady });
  // Preview exposure sweep (EV, exp2) — judge the material across a stop range. Stashed on preview
  // enter and restored on exit engine-side, so it never dirties the authored viewport's exposure.
  const [exposureEv, setExposureEv] = useState(0);
  const onExposure = useCallback((ev: number) => {
    setExposureEv(ev);
    void client.setExposure(ev).catch((err: unknown) => notifyError(errorText(err)));
  }, []);
  useEffect(() => {
    let cancelled = false;
    setPreviewReady(false);
    void (async () => {
      try {
        const entered = await client.enterAssetPreview(materialId);
        if (cancelled) {
          return;
        }
        const cam = await client.getCamera();
        if (cancelled) {
          return;
        }
        orbit.setFramed({
          target: { ...entered.target },
          distance: entered.distance,
          yaw: cam.yaw,
          pitch: cam.pitch,
        });
        setPreviewReady(true);
      } catch (err) {
        notifyError(errorText(err));
      }
    })();
    return () => {
      cancelled = true;
      void client.exitAssetPreview().catch(() => {});
    };
  }, [materialId, orbit]);
  // The node-create menu is a controlled, positioned element (not Radix) so every right-click reopens
  // it at the new cursor — Radix ContextMenu re-anchors unreliably while open. menuPosRef feeds addNode
  // (screen → flow coords); `menu` drives the rendered position.
  const menuPosRef = useRef<{ x: number; y: number } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const reactFlow = useReactFlow();

  // Per-tab snapshot history: the local React Flow state is the authority between
  // debounced saves, so undo records a { before, after } graph and replays the same
  // materialSetGraph apply the engine already sees.
  const history = useTabSnapshotHistory<MaterialGraph>(`materialGraph:${materialId}`, {
    read: () => flowToGraph(nodes, edges),
    write: async (g) => {
      const { nodes: n, edges: e } = graphToFlow(g);
      setNodes(n);
      setEdges(e);
      const set = await client.materialSetGraph(materialId, g);
      setStatus(set.foldable ? "applied (folded to params)" : "applied (codegen)");
    },
    equals: graphsEqual,
    label: "Edit material graph",
    selectionId: materialId,
  });

  // Load the material's stored graph into the canvas. The live preview sphere reflects the material
  // on its own (it references the `.smat` by id), so there is nothing to render here.
  useEffect(() => {
    loadedRef.current = false;
    void (async () => {
      try {
        const material = await client.materialGet(materialId);
        const { nodes: n, edges: e } = graphToFlow(
          material.graph as Parameters<typeof graphToFlow>[0],
        );
        setNodes(n);
        setEdges(e);
        // Baseline for the first real edit: the normalized loaded graph (same space the
        // apply effect produces), so a no-op first settle records nothing.
        history.seed(flowToGraph(n, e));
      } catch (err) {
        notifyError(errorText(err));
      }
    })();
  }, [materialId, setNodes, setEdges, history]);

  const onConnect = useCallback(
    (connection: Connection) => {
      if (connection.source === connection.target) {
        return; // no self-loops
      }
      // The emitter takes one source per input pin, so a new wire into an occupied input replaces it.
      setEdges((eds) => {
        const freed = eds.filter(
          (e) => !(e.target === connection.target && e.targetHandle === connection.targetHandle),
        );
        return addEdge(connection, freed);
      });
    },
    [setEdges],
  );

  const updateProps = useCallback(
    (id: string, props: Record<string, unknown>) => {
      setNodes((ns) => ns.map((n) => (n.id === id ? { ...n, data: { ...n.data, props } } : n)));
    },
    [setNodes],
  );
  const nodeCallbacks = useMemo(() => ({ updateProps }), [updateProps]);
  // Reject a self-loop during the drag (visual feedback); onConnect enforces the rest.
  const isValidConnection = useCallback((c: Connection | Edge) => c.source !== c.target, []);

  // Create a node from the context menu at the last right-click position (flow coordinates).
  const addNode = useCallback(
    (type: string) => {
      const spec = NODE_SPECS[type];
      if (!spec) {
        return;
      }
      const screen = menuPosRef.current;
      const position = screen ? reactFlow.screenToFlowPosition(screen) : { x: 200, y: 120 };
      const node: FlowNode = {
        id: freshNodeId(type),
        type: "saffron",
        position,
        data: { spec, props: { ...(spec.defaultProps ?? {}) } },
      };
      setNodes((ns) => [...ns, node]);
    },
    [reactFlow, setNodes],
  );

  // Debounced auto-apply: push the graph to the engine as it changes; the live sphere reflects it on
  // its own next frame. Skip the very first settle after load (that graph is already saved).
  useEffect(() => {
    if (!loadedRef.current) {
      loadedRef.current = true;
      return;
    }
    const timer = setTimeout(() => {
      // A replay already pushed the authoritative graph to the engine; skip this settle's
      // re-send and recording (the replay's setNodes triggered us, but it is not an edit).
      if (history.consumeReplay()) {
        return;
      }
      void (async () => {
        try {
          const graph = flowToGraph(nodes, edges);
          const set = await client.materialSetGraph(materialId, graph);
          setStatus(set.foldable ? "applied (folded to params)" : "applied (codegen)");
          // The live sphere re-renders on its own — material-set-graph invalidates the material
          // cache, so the next frame re-resolves the edited `.smat`. No readback round-trip.
          history.record(graph);
        } catch (err) {
          notifyError(errorText(err));
          setStatus("apply failed");
        }
      })();
    }, 500);
    return () => clearTimeout(timer);
  }, [nodes, edges, materialId, history]);

  const compile = useCallback(async () => {
    try {
      const result = await client.materialCompileGraph(materialId);
      setStatus(result.ok ? "compiled OK" : "compile failed");
    } catch (err) {
      notifyError(errorText(err));
      setStatus("compile failed");
    }
  }, [materialId]);

  const palette = useMemo(() => {
    const cats: Record<NodeCategory, string[]> = { input: [], math: [], output: [] };
    for (const spec of Object.values(NODE_SPECS)) {
      cats[spec.category].push(spec.type);
    }
    return cats;
  }, []);

  // Close the create menu on a left-click outside it or Escape (a right-click is handled by
  // onPaneContextMenu, which repositions; its button-2 mousedown is ignored here).
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

  // No bg on the root or the preview pane's hole: the preview is a transparent region down to the
  // engine's `assetPreview` subsurface (composited below the webview). Every other region paints its
  // own opaque bg-background (toolbar, the ReactFlow panel, the Preview header, the loading overlay),
  // so only the hole shows through.
  return (
    <div className="flex h-full w-full flex-col text-[12px] text-foreground">
      <div className="flex items-center gap-2 border-b border-border bg-background px-3 py-2">
        <span className="font-medium">Material graph</span>
        <span className="text-muted-foreground">{status}</span>
        <div className="ml-auto flex gap-2">
          <Button size="sm" variant="secondary" onClick={() => void compile()}>
            <Hammer className="size-3.5" />
            Compile
          </Button>
        </div>
      </div>
      <ResizablePanelGroup orientation="horizontal" className="min-h-0 flex-1">
        <ResizablePanel defaultSize={76} minSize={40} className="min-w-0 bg-background">
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
              onPaneContextMenu={(event) => {
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
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={24} minSize={12} className="min-w-0">
          <div className="flex h-full flex-col">
            <div className="flex items-center gap-2 border-b border-border bg-background px-3 py-2">
              <span className="text-[10px] uppercase text-muted-foreground">Preview</span>
              <div className="ml-auto flex items-center gap-1.5">
                <span className="text-[10px] uppercase text-muted-foreground">EV</span>
                <Slider
                  className="w-16"
                  value={[exposureEv]}
                  min={-6}
                  max={6}
                  step={0.1}
                  onValueChange={([v]) => onExposure(v)}
                  aria-label="Preview exposure (EV)"
                />
                <span className="w-7 text-right text-[10px] tabular-nums text-muted-foreground">
                  {exposureEv > 0 ? `+${exposureEv.toFixed(1)}` : exposureEv.toFixed(1)}
                </span>
              </div>
            </div>
            {/* The transparent hole: the live sphere composites through here. Orbit-only (no dolly). */}
            <div
              ref={hostRef}
              className="relative min-h-0 flex-1 cursor-grab active:cursor-grabbing"
              onPointerDown={orbit.onPointerDown}
              onPointerMove={orbit.onPointerMove}
              onPointerUp={orbit.onPointerUp}
              onWheel={orbit.onWheel}
            >
              {!previewReady && (
                <div className="absolute inset-0 flex items-center justify-center bg-background text-muted-foreground">
                  Rendering…
                </div>
              )}
            </div>
          </div>
        </ResizablePanel>
      </ResizablePanelGroup>
      {menu
        ? createPortal(
            <div
              ref={menuRef}
              className="fixed z-50 max-h-[80vh] w-44 overflow-y-auto rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md"
              style={{ left: menu.x, top: menu.y }}
            >
              {PALETTE_CATEGORIES.map((cat) => (
                <div key={cat}>
                  <div className="px-2 py-1 text-[10px] uppercase text-muted-foreground">{cat}</div>
                  {palette[cat].map((type) => (
                    <button
                      key={type}
                      type="button"
                      onClick={() => {
                        addNode(type);
                        setMenu(null);
                      }}
                      className="flex w-full items-center rounded-sm px-2 py-1.5 text-left text-[13px] hover:bg-accent hover:text-accent-foreground"
                    >
                      {NODE_SPECS[type].label}
                    </button>
                  ))}
                </div>
              ))}
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}

/// The material-graph main-tab body (mounted by App.tsx when a `materialGraph` ViewTab is active).
export function MaterialGraphEditor({ materialId }: { materialId: string }) {
  return (
    <ReactFlowProvider>
      <GraphCanvas materialId={materialId} />
    </ReactFlowProvider>
  );
}
