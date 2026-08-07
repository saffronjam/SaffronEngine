/// The authoring surface for a native plant family: the botanical graph on a node canvas, the typed
/// parameters of whichever operator is selected, the structure that graph grows, and the manual-edit
/// layer over any element in it.
///
/// A native family's structure is a *result* — parts, dimensions, spines, and proxies all come out of
/// growing the graph — so this panel edits the graph document and the variation list and reads
/// everything else back. One recorded edit is one semantic operation, and its inverse is the previous
/// document replayed through the same single write path.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { client } from "../../control/client";
import { errorText, notify, notifyError } from "../../lib/flash";
import { useEditorStore } from "../../state/store";
import type { BotanicalEditActionDto, BotanicalGraphDto, CommandResultMap } from "../../protocol";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { MAX_VARIATIONS, Q16 } from "./botanicalSchema";
import { withEdit, withOperator, withoutEdits, type PlantDocument } from "./document";
import { GraphEditor } from "./GraphEditor";
import { ModuleBindings } from "./ModuleBindings";
import { OperatorInspector } from "./OperatorInspector";
import { StructureTree } from "./StructureTree";

type GraphResult = CommandResultMap["plant-graph"];
type ElementsResult = CommandResultMap["plant-elements"];
type ValidationResult = CommandResultMap["plant-validate"];

/// Quiet time that ends a parameter burst. A regrow is expensive and the socket carries one request
/// at a time, so a scrub writes once when the artist stops rather than once per keystroke.
const SETTLE_MS = 350;

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

export function PlantGraphPanel() {
  const selectedAssets = useEditorStore((s) => s.selectedAssetIds);
  const pushEdit = useEditorStore((s) => s.pushEdit);
  const [subject, setSubject] = useState<string | null>(null);
  const [graph, setGraph] = useState<GraphResult | null>(null);
  const [elements, setElements] = useState<ElementsResult | null>(null);
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [busy, setBusy] = useState(false);
  /// The optimistic document a parameter burst is editing, cleared once the engine answers.
  const [draft, setDraft] = useState<PlantDocument | null>(null);
  const [node, setNode] = useState<string | null>(null);
  const [element, setElement] = useState<string | null>(null);

  // Exactly one selected asset is a subject; a multi-selection has no single graph to show.
  const selected = selectedAssets.size === 1 ? [...selectedAssets][0]! : null;

  const load = useCallback(async (asset: string) => {
    try {
      const next = await client.plantGraph(asset);
      setGraph(next);
      setElements(await client.plantElements(asset));
      setValidation(await client.plantValidate(asset));
      setSubject(asset);
    } catch {
      // An imported family has no botanical graph. That is a fact about the asset, not an operation
      // that failed, so the panel says so in place rather than raising a toast.
      setGraph(null);
      setElements(null);
      setValidation(null);
      setSubject(asset);
    }
  }, []);

  useEffect(() => {
    setNode(null);
    setElement(null);
    setDraft(null);
    if (selected === null) {
      setSubject(null);
      setGraph(null);
      return;
    }
    void load(selected);
  }, [load, selected]);

  // The grafts a native family declares are its own and this panel never rewrites them, so every
  // write — including an undo replayed long afterwards — sends back the set the family carries.
  const grafts = useRef<GraphResult["grafts"]>([]);
  grafts.current = graph?.grafts ?? grafts.current;

  /// The single write: `plant-graph-set` plus the readbacks it invalidates. Every other derived
  /// field is left to the engine to recompute rather than sent back as a second truth.
  const write = useCallback(async (asset: string, document: PlantDocument): Promise<void> => {
    const result = await client.plantGraphSet({
      plant: asset,
      graph: document.graph,
      grafts: grafts.current,
      modules: document.modules,
    });
    setGraph(result);
    setDraft(null);
    setElements(await client.plantElements(asset));
    setValidation(await client.plantValidate(asset));
    if (result.growth.orphans.length > 0) {
      notify(`${result.growth.orphans.length} manual edit(s) lost their target`);
    }
  }, []);

  // Writes queue behind one another: a structural gesture that lands while a parameter burst is
  // still settling must not race it into the engine, or the later reply would publish the older
  // document.
  const chain = useRef<Promise<unknown>>(Promise.resolve());
  const commit = useCallback(
    (label: string, before: PlantDocument, next: PlantDocument) => {
      const asset = subject;
      if (asset === null) {
        return;
      }
      setBusy(true);
      chain.current = chain.current.then(async () => {
        try {
          await write(asset, next);
          pushEdit({
            label,
            undo: () => write(asset, before),
            redo: () => write(asset, next),
            selectionId: asset,
          });
        } catch (err) {
          setDraft(null);
          notifyError(errorText(err));
        } finally {
          setBusy(false);
        }
      });
    },
    [pushEdit, subject, write],
  );

  const document = useMemo<PlantDocument | null>(
    () => draft ?? (graph ? { graph: graph.graph, modules: graph.modules } : null),
    [draft, graph],
  );

  // A parameter scrub edits the document locally and writes once the artist stops, so the burst is
  // one recorded edit against the document as it stood before the first keystroke.
  const burst = useRef<{
    timer: ReturnType<typeof setTimeout>;
    before: PlantDocument;
  } | null>(null);
  useEffect(
    () => () => {
      if (burst.current) {
        clearTimeout(burst.current.timer);
      }
    },
    [],
  );

  /// One structural gesture: written now, recorded now. A parameter burst still settling is folded
  /// in rather than recorded separately — its changes are already in the document being written.
  const apply = useCallback(
    (label: string, next: PlantDocument) => {
      if (!document) {
        return;
      }
      const before = burst.current?.before ?? document;
      if (burst.current) {
        clearTimeout(burst.current.timer);
        burst.current = null;
      }
      commit(label, before, next);
    },
    [commit, document],
  );

  /// One graph-only gesture, for the surfaces that never touch the module table.
  const applyGraph = useCallback(
    (label: string, next: BotanicalGraphDto) => {
      if (document) {
        apply(label, { ...document, graph: next });
      }
    },
    [apply, document],
  );

  const scrub = useCallback(
    (label: string, next: PlantDocument) => {
      const before =
        burst.current?.before ?? (graph ? { graph: graph.graph, modules: graph.modules } : null);
      if (!before) {
        return;
      }
      if (burst.current) {
        clearTimeout(burst.current.timer);
      }
      setDraft(next);
      burst.current = {
        before,
        timer: setTimeout(() => {
          burst.current = null;
          commit(label, before, next);
        }, SETTLE_MS),
      };
    },
    [commit, graph],
  );

  /// One graph-only scrub, for the surfaces that never touch the module table.
  const scrubGraph = useCallback(
    (label: string, next: BotanicalGraphDto) => {
      if (document) {
        scrub(label, { ...document, graph: next });
      }
    },
    [document, scrub],
  );

  const operator = useMemo(
    () => document?.graph.nodes.find((row) => row.guid === node)?.operator ?? null,
    [document, node],
  );

  const addVariation = useCallback(() => {
    if (!document) {
      return;
    }
    const base = document.graph.variations[0];
    if (base === undefined) {
      return;
    }
    applyGraph("Add variation", {
      ...document.graph,
      variations: [
        ...document.graph.variations,
        {
          // A new individual, not the same one twice: the seed advances, which validation requires.
          seed: (BigInt(base.seed) + BigInt(document.graph.variations.length)).toString(),
          age: base.age,
          name: `Variation ${document.graph.variations.length + 1}`,
        },
      ],
    });
  }, [applyGraph, document]);

  if (selected === null) {
    return (
      <div className="p-3 text-[11px] text-muted-foreground">
        Select one plant asset to author the structure it grows.
      </div>
    );
  }
  if (document === null || graph === null) {
    return (
      <div className="p-3 text-[11px] text-muted-foreground">
        {subject === null
          ? "Loading…"
          : "This plant family has an imported source rather than a botanical graph."}
      </div>
    );
  }

  const growth = graph.growth;
  const errors = validation?.diagnostics.filter((entry) => entry.severity === "error") ?? [];
  const warnings = validation?.diagnostics.filter((entry) => entry.severity === "warning") ?? [];

  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <div className="min-h-0 flex-1">
        <GraphEditor
          graph={document.graph}
          selected={node}
          onSelect={setNode}
          onApply={applyGraph}
          busy={busy}
        />
      </div>
      <Tabs
        defaultValue="node"
        className="flex min-h-0 shrink-0 basis-[46%] flex-col gap-0 border-t border-border"
      >
        <TabsList className="mx-2 mt-1 self-start">
          <TabsTrigger value="node">Node</TabsTrigger>
          <TabsTrigger value="structure">Structure</TabsTrigger>
          <TabsTrigger value="family">Family</TabsTrigger>
        </TabsList>

        <TabsContent value="node" className="min-h-0 flex-1">
          <ScrollArea className="h-full">
            <div className="p-2">
              {operator === null ? (
                <p className="text-[11px] text-muted-foreground">
                  Select a node on the canvas to edit what it grows. Right-click the canvas to add
                  one.
                </p>
              ) : (
                <OperatorInspector
                  operator={operator}
                  onCommit={(label, next) =>
                    scrubGraph(label, withOperator(document.graph, node!, next))
                  }
                />
              )}
            </div>
          </ScrollArea>
        </TabsContent>

        <TabsContent value="structure" className="min-h-0 flex-1">
          <StructureTree
            axes={elements?.axes ?? []}
            placements={elements?.elements ?? []}
            edits={document.graph.edits}
            grafts={graph.grafts}
            selected={element}
            onSelect={setElement}
            onEdit={(label, target, action: BotanicalEditActionDto) =>
              scrubGraph(label, withEdit(document.graph, target, action))
            }
            onClearEdits={(target) =>
              applyGraph("Clear element edits", withoutEdits(document.graph, target))
            }
            busy={busy}
          />
        </TabsContent>

        <TabsContent value="family" className="min-h-0 flex-1">
          <ScrollArea className="h-full">
            <div className="flex flex-col gap-2 p-2">
              <div className="flex items-center justify-between">
                <Label className="text-[11px] text-muted-foreground">Variations</Label>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={addVariation}
                  disabled={busy || document.graph.variations.length >= MAX_VARIATIONS}
                >
                  Add
                </Button>
              </div>
              <div className="flex flex-col gap-1">
                {document.graph.variations.map((variation, index) => (
                  <div key={variation.seed + variation.name} className="flex items-center gap-2">
                    <span className="flex-1 truncate text-[11px] text-foreground">
                      {variation.name}
                    </span>
                    <Input
                      className="h-6 w-20 font-mono text-[11px]"
                      type="number"
                      min={1}
                      max={65535}
                      value={variation.age}
                      onChange={(event) => {
                        const age = Math.round(Number(event.target.value));
                        if (Number.isFinite(age)) {
                          scrubGraph("Set variation age", {
                            ...document.graph,
                            variations: document.graph.variations.map((row, at) =>
                              at === index ? { ...row, age } : row,
                            ),
                          });
                        }
                      }}
                    />
                  </div>
                ))}
              </div>

              <Separator />
              <ModuleBindings document={document} onApply={apply} onScrub={scrub} busy={busy} />

              <Separator />
              <div className="flex flex-col gap-0.5">
                <Stat label="Axes" value={growth.axes} />
                <Stat label="Elements" value={growth.elements} />
                <Stat label="Shells" value={growth.shells} />
                <Stat label="Grafts" value={growth.grafts} />
                <Stat label="Vertices" value={growth.vertices} />
                <Stat label="Triangles" value={growth.triangles} />
                <Stat label="Parts" value={growth.parts} />
                <Stat label="Height" value={`${(growth.heightBits / Q16).toFixed(2)} m`} />
                <Stat label="Applied edits" value={growth.appliedEdits} />
                <Stat
                  label="Graph"
                  value={
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <span>{growth.graph.slice(0, 12)}</span>
                      </TooltipTrigger>
                      <TooltipContent className="font-mono text-[11px]">
                        {growth.graph}
                      </TooltipContent>
                    </Tooltip>
                  }
                />
              </div>

              {growth.orphans.length > 0 && (
                <>
                  <Separator />
                  <Label className="text-[11px] text-muted-foreground">Orphaned edits</Label>
                  <div className="flex flex-col gap-0.5 font-mono text-[11px]">
                    {growth.orphans.map((orphan) => (
                      <div key={orphan.target + orphan.reason} className="text-amber-500">
                        {orphan.target.slice(0, 12)} · {orphan.reason}
                      </div>
                    ))}
                  </div>
                </>
              )}

              <Separator />
              <div className="flex flex-col gap-0.5">
                <Stat label="Errors" value={errors.length} />
                <Stat label="Warnings" value={warnings.length} />
              </div>
              {[...errors, ...warnings].slice(0, 8).map((entry) => (
                <div
                  key={entry.code + entry.path + entry.message}
                  className={
                    entry.severity === "error"
                      ? "text-[11px] text-destructive"
                      : "text-[11px] text-amber-500"
                  }
                >
                  {entry.code}: {entry.message}
                </div>
              ))}
            </div>
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  );
}
