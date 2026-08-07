/// The cooked cut and its declared errors: which nodes draw triangles, which draw an aggregate voxel
/// brick, and the transition error each one declares, with a cut control that makes the preview
/// beside it draw either representation.
///
/// The cut is pinnable rather than distance-driven because a representation comparison needs the cut
/// to move while the camera holds still — flying out to reach the aggregate shrinks the subject at
/// the same time, conflating the two changes. The error column is the number the cut selector reads:
/// a node whose declared error saturates is never selected while anything finer is resident.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { useEditorStore } from "../state/store";

type Hierarchy = CommandResultMap["plant-hierarchy"];
type Cut = CommandResultMap["set-hierarchy-cut"]["cut"];

const CUTS: { value: Cut; label: string; hint: string }[] = [
  { value: "auto", label: "Auto", hint: "Projected error chooses, as a shipped frame does" },
  { value: "coarse", label: "Coarse", hint: "Never refine: aggregate voxels" },
  { value: "fine", label: "Fine", hint: "Always refine: triangle clusters" },
];

/// Q15.16 out of the cooked error, shown in local units so it reads as a distance rather than as a
/// raw fixed-point word.
function errorUnits(value: number): string {
  return value >= 0xffff_ffff ? "saturated" : (value / 65536).toFixed(3);
}

export function PlantHierarchyPanel() {
  const selectedAssets = useEditorStore((state) => state.selectedAssetIds);
  const subject = selectedAssets.size === 1 ? [...selectedAssets][0]! : null;
  const [hierarchy, setHierarchy] = useState<Hierarchy | null>(null);
  const [cut, setCut] = useState<Cut>("auto");
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!subject) {
      setHierarchy(null);
      return;
    }
    try {
      setHierarchy(await client.plantHierarchy({ plant: subject }));
      setUnavailable(null);
    } catch (err) {
      setHierarchy(null);
      setUnavailable(errorText(err));
    }
  }, [subject]);

  useEffect(() => {
    void load();
    void client
      .setHierarchyCut()
      .then((reply) => setCut(reply.cut))
      .catch(() => {});
  }, [load]);

  const applyCut = useCallback(async (next: Cut) => {
    try {
      const reply = await client.setHierarchyCut({ cut: next });
      setCut(reply.cut);
    } catch (err) {
      notifyError(errorText(err));
    }
  }, []);

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        <section className="flex flex-col gap-2">
          <Label className="text-[11px] text-muted-foreground">Cut</Label>
          <div className="flex flex-wrap gap-1">
            {CUTS.map((option) => (
              <Button
                className="h-7 text-[11px]"
                key={option.value}
                onClick={() => void applyCut(option.value)}
                size="sm"
                variant={cut === option.value ? "default" : "outline"}
              >
                {option.label}
              </Button>
            ))}
          </div>
          <p className="text-[10px] text-muted-foreground">
            {CUTS.find((option) => option.value === cut)?.hint}
          </p>
        </section>

        <Separator />

        {unavailable ? <p className="text-[11px] text-muted-foreground">{unavailable}</p> : null}

        {hierarchy ? (
          <>
            <div className="flex items-baseline justify-between gap-2">
              <span className="text-[11px] text-muted-foreground">Nodes</span>
              <span className="font-mono text-[11px] tabular-nums text-foreground">
                {hierarchy.triangleNodes} triangle · {hierarchy.voxelNodes} voxel
              </span>
            </div>

            <div className="flex flex-col">
              {hierarchy.nodes.map((node) => (
                <div
                  className="flex items-baseline justify-between gap-2 border-b border-border/40 py-0.5"
                  key={node.id}
                  style={{ paddingLeft: `${Math.min(node.depth, 8) * 8}px` }}
                >
                  <span className="text-[11px] text-foreground">
                    {node.representation === "voxel" ? "▧" : "△"} {node.id}
                    <span className="ml-1 text-muted-foreground">
                      {node.primitives} · p{node.page}
                    </span>
                  </span>
                  <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                    {errorUnits(node.appearanceError.total)}
                  </span>
                </div>
              ))}
            </div>
          </>
        ) : null}

        <Button
          className="h-7 self-start text-[11px]"
          onClick={() => void load()}
          size="sm"
          variant="outline"
        >
          Refresh
        </Button>
      </div>
    </ScrollArea>
  );
}
