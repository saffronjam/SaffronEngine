/// The biome graph rendered read-only on the shared GraphCanvas: nodes labelled by operator with
/// authority badges, pins typed from the engine's closed node schema (observed edges fill in for an
/// unknown operator).
import { useEffect, useMemo, useState } from "react";
import type { Edge } from "@xyflow/react";
import { client } from "../control/client";
import type { VegetationNodeEvaluationDiagnosticDto, VegetationNodeSchemaDto } from "../protocol";
import { Button } from "@/components/ui/button";
import {
  GraphCanvas,
  type GraphCanvasSchema,
  type GraphFlowNode,
  type GraphNodeSpecBase,
} from "../components/graph/GraphCanvas";
import { errorText, notifyError } from "../lib/flash";
import { humanizeFieldName } from "../lib/humanize";
import { useAssetPreview } from "./assetEditorPanels";

interface BiomeGraphNodeJson {
  guid: number | string;
  operator: string;
  authority?: string;
}

interface BiomeGraphEdgeJson {
  fromNode: number | string;
  fromPin: string;
  toNode: number | string;
  toPin: string;
}

interface BiomeGraphJson {
  nodes?: BiomeGraphNodeJson[];
  edges?: BiomeGraphEdgeJson[];
}

/// One derived flow model of an authored biome graph document. Pins come from the
/// operator's closed schema (a required input marks itself with `*`); an operator
/// absent from the schema falls back to the pins its edges observe.
function biomeGraphToFlow(
  graph: BiomeGraphJson,
  nodeSchemas: ReadonlyMap<string, VegetationNodeSchemaDto>,
  diagnostics: ReadonlyMap<string, VegetationNodeEvaluationDiagnosticDto>,
): {
  nodes: GraphFlowNode[];
  edges: Edge[];
  schema: GraphCanvasSchema;
} {
  const nodes = graph.nodes ?? [];
  const edges = graph.edges ?? [];
  const inputsByNode = new Map<string, Set<string>>();
  const outputsByNode = new Map<string, Set<string>>();
  for (const edge of edges) {
    const to = String(edge.toNode);
    const from = String(edge.fromNode);
    (inputsByNode.get(to) ?? inputsByNode.set(to, new Set()).get(to)!).add(edge.toPin);
    (outputsByNode.get(from) ?? outputsByNode.set(from, new Set()).get(from)!).add(edge.fromPin);
  }

  const specs: Record<string, GraphNodeSpecBase> = {};
  const flowNodes: GraphFlowNode[] = nodes.map((node, index) => {
    const id = String(node.guid);
    // Pin names double as edge handle ids, so the schema names pass through
    // verbatim — required/domain detail stays in the schema row.
    const operatorSchema = nodeSchemas.get(node.operator) ?? null;
    const inputs =
      operatorSchema !== null
        ? operatorSchema.inputs.map((pin) => pin.name)
        : [...(inputsByNode.get(id) ?? [])].sort();
    const outputs =
      operatorSchema !== null
        ? operatorSchema.outputs.map((pin) => pin.name)
        : [...(outputsByNode.get(id) ?? [])].sort();
    const gpu = node.authority !== "authoritative" || (operatorSchema?.slangCompute ?? false);
    // A profiled node carries its measured evaluation row: elapsed time, output
    // cardinality, and the domain the plan actually placed it on.
    const diagnostic = diagnostics.get(id) ?? null;
    const profile =
      diagnostic !== null
        ? ` · ${(Number(diagnostic.elapsedMicros) / 1000).toFixed(1)} ms · ${
            diagnostic.outputCandidates
          } out · ${diagnostic.executionDomain}`
        : "";
    const spec: GraphNodeSpecBase = {
      type: `biome:${id}`,
      label: `${humanizeFieldName(node.operator.replaceAll("-", " "))}${gpu ? " (gpu)" : ""}${profile}`,
      category: "biome",
      inputs,
      outputs,
    };
    specs[spec.type] = spec;
    return {
      id,
      type: "saffron",
      position: { x: (index % 4) * 220, y: Math.floor(index / 4) * 140 },
      data: { spec, props: {} },
    };
  });
  const flowEdges: Edge[] = edges.map((edge, index) => ({
    id: `e${index}`,
    source: String(edge.fromNode),
    sourceHandle: edge.fromPin,
    target: String(edge.toNode),
    targetHandle: edge.toPin,
  }));
  return { nodes: flowNodes, edges: flowEdges, schema: { specs, categories: ["biome"] } };
}

export function BiomeGraphPanel() {
  const { assetId, vegetationType } = useAssetPreview();
  const [graph, setGraph] = useState<BiomeGraphJson | null>(null);
  const [nodeSchemas, setNodeSchemas] = useState<ReadonlyMap<string, VegetationNodeSchemaDto>>(
    new Map(),
  );
  const [diagnostics, setDiagnostics] = useState<
    ReadonlyMap<string, VegetationNodeEvaluationDiagnosticDto>
  >(new Map());
  const [profiling, setProfiling] = useState(false);
  // The standalone compile summary: predicted cardinality, influence halo, and the
  // evaluator caps — graph-local planning facts independent of any map region.
  const [compileSummary, setCompileSummary] = useState<{
    haloBits: number;
    candidates: string;
    accepted: string;
    microSamples: string;
  } | null>(null);

  /// Profiles one evaluation of this biome through the bound map's instance and
  /// annotates every node with its measured row (time, cardinality, domain).
  const profileEvaluation = (): void => {
    setProfiling(true);
    void (async () => {
      try {
        const status = await client.vegetationRuntimeStatus();
        const map = status.state === "available" ? status.map : null;
        if (map === null) {
          notifyError("Bind the vegetation map to a scene field first");
          return;
        }
        const summary = await client.vegetationAssetSummary(map);
        if (summary.summary.kind !== "vegetation-map") {
          return;
        }
        const asset = summary.summary.asset;
        const instance = asset.biomeInstances.find((row) => String(row.biome) === assetId);
        if (!instance) {
          notifyError("The bound map has no instance of this biome");
          return;
        }
        const prepared = await client.vegetationPreflightRegion({
          map,
          biomeInstance: instance.instance,
          bounds: asset.bounds,
          level: asset.chunkLevel,
        });
        await client.vegetationStartEvaluation(prepared.job);
        for (;;) {
          const state = await client.vegetationEvaluationStatus(prepared.job);
          if (state.state === "completed") {
            setDiagnostics(
              new Map((state.summary?.nodes ?? []).map((row) => [String(row.node), row])),
            );
            return;
          }
          if (state.state === "failed" || state.state === "cancelled") {
            notifyError(state.error?.message ?? "vegetation evaluation did not complete");
            return;
          }
          await new Promise((resolve) => setTimeout(resolve, 500));
        }
      } catch (err) {
        notifyError(errorText(err));
      } finally {
        setProfiling(false);
      }
    })();
  };

  useEffect(() => {
    if (vegetationType !== "biome") {
      return;
    }
    let live = true;
    void client
      .vegetationAssetSummary(assetId)
      .then((summary) => {
        if (live && summary.summary.kind === "biome") {
          setGraph((summary.summary.asset as { graph: BiomeGraphJson }).graph);
        }
      })
      .catch((err: unknown) => notifyError(errorText(err)));
    void client
      .vegetationNodeSchema()
      .then((schema) => {
        if (live) {
          setNodeSchemas(new Map(schema.nodes.map((node) => [node.operator, node])));
        }
      })
      .catch((err: unknown) => notifyError(errorText(err)));
    void client
      .vegetationCompileBiome({ target: { scope: "asset", biome: assetId } })
      .then((compiled) => {
        if (live) {
          setCompileSummary({
            haloBits: compiled.requiredHaloBits,
            candidates: compiled.estimate.candidates,
            accepted: compiled.estimate.accepted,
            microSamples: compiled.estimate.microSamples,
          });
        }
      })
      .catch((err: unknown) => notifyError(errorText(err)));
    return () => {
      live = false;
    };
  }, [assetId, vegetationType]);

  const flow = useMemo(
    () => (graph ? biomeGraphToFlow(graph, nodeSchemas, diagnostics) : null),
    [graph, nodeSchemas, diagnostics],
  );
  if (vegetationType !== "biome") {
    return null;
  }
  return (
    <div className="relative h-full w-full bg-background">
      <div className="absolute right-2 top-2 z-10 flex items-center gap-2">
        {compileSummary !== null && (
          <span className="rounded bg-card/90 px-2 py-1 font-mono text-[10px] tabular-nums text-muted-foreground">
            halo {compileSummary.haloBits} · ≤{compileSummary.candidates} candidates · ≤
            {compileSummary.accepted} accepted · ≤{compileSummary.microSamples} micro
          </span>
        )}
        <Button
          size="sm"
          variant="outline"
          className="h-6 text-[11px]"
          disabled={profiling}
          onClick={profileEvaluation}
        >
          {profiling ? "Profiling…" : "Profile evaluation"}
        </Button>
      </div>
      {flow === null ? (
        <div className="flex h-full items-center justify-center text-[11px] text-muted-foreground">
          Loading graph…
        </div>
      ) : (
        <GraphCanvas
          nodes={flow.nodes}
          edges={flow.edges}
          onNodesChange={() => {}}
          onEdgesChange={() => {}}
          setEdges={() => {}}
          updateProps={() => {}}
          schema={flow.schema}
          readOnly
        />
      )}
    </div>
  );
}
