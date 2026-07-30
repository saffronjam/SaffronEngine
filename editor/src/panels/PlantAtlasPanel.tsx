/// The packed coverage atlas the family actually samples, read out of the PUBLISHED artifact rather
/// than re-packed: a re-pack of the same images produces a different arrangement, which would look
/// authoritative while showing an image the plant never samples. The mip level is scrubbable because
/// the chain is coverage-preserving, and a level that lost alpha area is exactly where distant
/// foliage goes thin.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText } from "../lib/flash";
import type { CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Slider } from "@/components/ui/slider";
import { useEditorStore } from "../state/store";

type Atlas = CommandResultMap["plant-atlas"];

export function PlantAtlasPanel() {
  const selectedAssets = useEditorStore((state) => state.selectedAssetIds);
  // Exactly one selected asset is a subject; a multi-selection has no single atlas to show, which
  // is the same rule the structure panel beside this one uses.
  const subject = selectedAssets.size === 1 ? [...selectedAssets][0]! : null;
  const [atlas, setAtlas] = useState<Atlas | null>(null);
  const [level, setLevel] = useState(0);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const load = useCallback(
    async (which: number) => {
      if (!subject) {
        setAtlas(null);
        return;
      }
      try {
        setAtlas(await client.plantAtlas({ plant: subject, level: which }));
        setUnavailable(null);
      } catch (err) {
        // A family whose slots resolve to catalog materials cooks no packed atlas. That is a fact
        // about the family rather than a failed operation, so it is stated in place instead of
        // raised as a toast the user has to dismiss on every plant they open.
        setAtlas(null);
        setUnavailable(errorText(err));
      }
    },
    [subject],
  );

  useEffect(() => {
    setLevel(0);
    void load(0);
  }, [load]);

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        {unavailable ? <p className="text-[11px] text-muted-foreground">{unavailable}</p> : null}

        {atlas ? (
          <>
            <div className="relative w-full overflow-hidden rounded border border-border bg-card">
              {/* A checker under the image so the gutter and the cut-out alpha read as
                  transparent rather than as black, which is the whole point of looking at it. */}
              <div
                className="absolute inset-0 opacity-40"
                style={{
                  backgroundImage:
                    "linear-gradient(45deg, #808080 25%, transparent 25%), linear-gradient(-45deg, #808080 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #808080 75%), linear-gradient(-45deg, transparent 75%, #808080 75%)",
                  backgroundPosition: "0 0, 0 4px, 4px -4px, -4px 0",
                  backgroundSize: "8px 8px",
                }}
              />
              <img
                alt={`Coverage atlas level ${atlas.level}`}
                className="relative w-full"
                src={`data:image/png;base64,${atlas.base64}`}
                style={{ imageRendering: "pixelated" }}
              />
            </div>

            <div className="flex flex-col gap-1">
              <Label className="text-[11px] text-muted-foreground">
                Level{" "}
                <span className="font-mono tabular-nums">
                  {atlas.level} / {atlas.levelCount - 1} — {atlas.width}×{atlas.height}
                </span>
              </Label>
              <Slider
                max={Math.max(atlas.levelCount - 1, 0)}
                min={0}
                onValueChange={([value]) => {
                  const next = value ?? 0;
                  setLevel(next);
                  void load(next);
                }}
                step={1}
                value={[level]}
              />
            </div>

            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">
                Slots <span className="text-muted-foreground">(gutter {atlas.gutter}px)</span>
              </h3>
              {atlas.placements.map((placement) => (
                <div className="flex items-baseline justify-between gap-2" key={placement.slot}>
                  <span className="text-[11px] text-muted-foreground">Slot {placement.slot}</span>
                  <span className="font-mono text-[11px] tabular-nums text-foreground">
                    {placement.x},{placement.y} · {placement.width}×{placement.height}
                  </span>
                </div>
              ))}
            </section>
          </>
        ) : null}

        <Button
          className="h-7 self-start text-[11px]"
          onClick={() => void load(level)}
          size="sm"
          variant="outline"
        >
          Refresh
        </Button>
      </div>
    </ScrollArea>
  );
}
