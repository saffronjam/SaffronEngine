/// The structure a native family grows, its variations, its addressable elements, and its validation
/// state. A native family's structure is a *result* — parts, dimensions, spines, and proxies are all
/// derived from growing the graph — so this panel edits the graph document and the variation list
/// and reads everything else back.
///
/// One recorded edit is one semantic operation, and its inverse is the previous graph document
/// replayed through the same single write path; nothing reconstructs the old parts by hand.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notify, notifyError } from "../lib/flash";
import { useEditorStore } from "../state/store";
import type { BotanicalGraphDto, CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

type GraphResult = CommandResultMap["plant-graph"];
type ElementsResult = CommandResultMap["plant-elements"];
type ValidationResult = CommandResultMap["plant-validate"];

/// One Q15.16 metre, the unit every botanical length crosses the wire in.
const Q16 = 65536;

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

export function PlantGraphPanel() {
  const plant = useEditorStore((s) => s.selectedAssetIds);
  const pushEdit = useEditorStore((s) => s.pushEdit);
  const [subject, setSubject] = useState<string | null>(null);
  const [graph, setGraph] = useState<GraphResult | null>(null);
  const [elements, setElements] = useState<ElementsResult | null>(null);
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [busy, setBusy] = useState(false);

  // Exactly one selected asset is a subject; a multi-selection has no single graph to show.
  const selected = plant.size === 1 ? [...plant][0]! : null;

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
    if (selected === null) {
      setSubject(null);
      setGraph(null);
      return;
    }
    void load(selected);
  }, [load, selected]);

  /// Applies `next` as one undoable semantic operation, with the previous document as its inverse.
  const apply = useCallback(
    async (label: string, next: BotanicalGraphDto) => {
      if (subject === null || graph === null) {
        return;
      }
      const previous = graph.graph;
      const write = async (document: BotanicalGraphDto) => {
        // The grafts a native family declares are its own; a graph edit never rewrites them.
        const result = await client.plantGraphSet({
          plant: subject,
          graph: document,
          grafts: graph.grafts,
        });
        setGraph(result);
        setElements(await client.plantElements(subject));
        setValidation(await client.plantValidate(subject));
        return result;
      };
      setBusy(true);
      try {
        const result = await write(next);
        pushEdit({
          label,
          undo: () => write(previous),
          redo: () => write(next),
          selectionId: subject,
        });
        if (result.growth.orphans.length > 0) {
          notify(`${result.growth.orphans.length} manual edit(s) lost their target`);
        }
      } catch (err) {
        notifyError(errorText(err));
      } finally {
        setBusy(false);
      }
    },
    [graph, pushEdit, subject],
  );

  const addVariation = useCallback(() => {
    if (graph === null) {
      return;
    }
    const base = graph.graph.variations[0];
    if (base === undefined) {
      return;
    }
    void apply("Add variation", {
      ...graph.graph,
      variations: [
        ...graph.graph.variations,
        {
          // A new individual, not the same one twice: the seed advances, which validation requires.
          seed: (BigInt(base.seed) + BigInt(graph.graph.variations.length)).toString(),
          age: base.age,
          name: `Variation ${graph.graph.variations.length + 1}`,
        },
      ],
    });
  }, [apply, graph]);

  const setAge = useCallback(
    (index: number, age: number) => {
      if (graph === null) {
        return;
      }
      void apply("Set variation age", {
        ...graph.graph,
        variations: graph.graph.variations.map((variation, at) =>
          at === index ? { ...variation, age } : variation,
        ),
      });
    },
    [apply, graph],
  );

  if (selected === null) {
    return (
      <div className="p-3 text-[11px] text-muted-foreground">
        Select one plant asset to inspect the structure it grows.
      </div>
    );
  }
  if (graph === null) {
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
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-2 p-2">
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
                <TooltipContent className="font-mono text-[11px]">{growth.graph}</TooltipContent>
              </Tooltip>
            }
          />
        </div>

        <Separator />
        <div className="flex items-center justify-between">
          <Label className="text-[11px] text-muted-foreground">Variations</Label>
          <Button size="sm" variant="secondary" onClick={addVariation} disabled={busy}>
            Add
          </Button>
        </div>
        <div className="flex flex-col gap-1">
          {graph.graph.variations.map((variation, index) => (
            <div key={variation.seed + variation.name} className="flex items-center gap-2">
              <span className="flex-1 truncate text-[11px] text-foreground">{variation.name}</span>
              <Input
                className="h-6 w-20 font-mono text-[11px]"
                type="number"
                min={1}
                max={65535}
                value={variation.age}
                disabled={busy}
                onChange={(event) => setAge(index, Number(event.target.value))}
              />
            </div>
          ))}
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
        <Label className="text-[11px] text-muted-foreground">
          Elements ({elements?.axes.length ?? 0} axes, {elements?.elements.length ?? 0} placed)
        </Label>
        <div className="flex flex-col gap-0.5 font-mono text-[11px]">
          {(elements?.axes ?? []).slice(0, 24).map((axis) => (
            <div key={axis.id} className="flex justify-between gap-2">
              <span className="text-muted-foreground">{axis.element}</span>
              <span className="tabular-nums">
                r={(axis.baseRadiusBits / Q16).toFixed(3)} · {axis.points} pts
              </span>
            </div>
          ))}
        </div>

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
  );
}
