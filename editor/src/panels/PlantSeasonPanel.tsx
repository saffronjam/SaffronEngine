/// The Plant workspace's lifecycle and season timeline.
///
/// Scrubbing the year asks the engine which appearance the family would render at that point, then
/// binds it to the live preview — so this is the plant through the year rather than a picker over a
/// list of phenotype ids. The resolution is the ENGINE's (`resolve_rendered_phenotype`), never a
/// second reading of the same rules in TypeScript: a preview that resolved the season differently
/// from the renderer would be showing an appearance the scene never picks.
///
/// Lifecycle wins over season, which is why it is a control here and not an afterthought. A dead
/// plant does not turn autumnal, and seeing that hold is the point of being able to set both.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { CommandParamsMap, CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Slider } from "@/components/ui/slider";
import { useEditorStore } from "../state/store";

type Resolved = CommandResultMap["plant-season-phenotype"];
type Lifecycle = NonNullable<CommandParamsMap["plant-season-phenotype"]["lifecycle"]>;

/// The lifecycles worth scrubbing between. `seed` and `removed` render nothing to compare.
const LIFECYCLES: Lifecycle[] = ["juvenile", "mature", "senescent", "dead"];

/// Quarter marks, so a scrub lands on something nameable rather than on "part 637 of the year".
const MARKS: { label: string; mille: number }[] = [
  { label: "Spring", mille: 125 },
  { label: "Summer", mille: 375 },
  { label: "Autumn", mille: 625 },
  { label: "Winter", mille: 875 },
];

export function PlantSeasonPanel() {
  const selectedAssets = useEditorStore((state) => state.selectedAssetIds);
  const subject = selectedAssets.size === 1 ? [...selectedAssets][0]! : null;
  const [season, setSeason] = useState(375);
  const [lifecycle, setLifecycle] = useState<Lifecycle>("mature");
  const [resolved, setResolved] = useState<Resolved | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const apply = useCallback(
    async (mille: number, state: Lifecycle) => {
      if (!subject) {
        setResolved(null);
        return;
      }
      let next: Resolved;
      try {
        next = await client.plantSeasonPhenotype({
          plant: subject,
          seasonMille: mille,
          lifecycle: state,
        });
      } catch (err) {
        // A subject that is not a plant family, or one that resolves nothing, is a fact about the
        // asset rather than a failed operation — stated in place instead of toasted at an author
        // who merely opened the wrong tab.
        setResolved(null);
        setUnavailable(errorText(err));
        return;
      }
      setResolved(next);
      setUnavailable(null);
      try {
        // Binding both is deliberate: a phenotype draws a specific variation, and applying one
        // without the other leaves the preview showing a combination the family never declares.
        await client.setAssetPreviewOptions({
          phenotype: next.phenotype,
          variation: next.variation,
        });
      } catch (err) {
        // This one IS a failed operation: the scrub did not take, and the author has to see why.
        notifyError(errorText(err));
      }
    },
    [subject],
  );

  useEffect(() => {
    void apply(season, lifecycle);
    // Only on a subject change: scrubbing drives `apply` directly, and re-running here on every
    // slider tick would fight the scrub.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [subject]);

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        {unavailable ? <p className="text-[11px] text-muted-foreground">{unavailable}</p> : null}

        <section className="flex flex-col gap-2">
          <Label className="text-[11px] text-muted-foreground">Lifecycle</Label>
          <div className="flex flex-wrap gap-1">
            {LIFECYCLES.map((state) => (
              <Button
                className="h-7 text-[11px] capitalize"
                key={state}
                onClick={() => {
                  setLifecycle(state);
                  void apply(season, state);
                }}
                size="sm"
                variant={lifecycle === state ? "default" : "outline"}
              >
                {state}
              </Button>
            ))}
          </div>
        </section>

        <section className="flex flex-col gap-2">
          <Label className="text-[11px] text-muted-foreground">
            Season <span className="font-mono tabular-nums">{season}‰</span>
          </Label>
          <Slider
            max={999}
            min={0}
            onValueChange={([value]) => {
              const next = value ?? 0;
              setSeason(next);
              void apply(next, lifecycle);
            }}
            step={5}
            value={[season]}
          />
          <div className="flex flex-wrap gap-1">
            {MARKS.map((mark) => (
              <Button
                className="h-7 text-[11px]"
                key={mark.label}
                onClick={() => {
                  setSeason(mark.mille);
                  void apply(mark.mille, lifecycle);
                }}
                size="sm"
                variant="outline"
              >
                {mark.label}
              </Button>
            ))}
          </div>
        </section>

        {resolved ? (
          <div className="flex items-baseline justify-between gap-2">
            <span className="text-[11px] text-muted-foreground">Renders</span>
            <span className="font-mono text-[11px] tabular-nums text-foreground">
              phenotype {resolved.phenotype} · variation {resolved.variation}
            </span>
          </div>
        ) : null}

        <p className="text-[10px] text-muted-foreground">
          Lifecycle wins over season: a dead or senescent plant takes its own appearance whatever
          the time of year. A family with one phenotype resolves to it throughout.
        </p>
      </div>
    </ScrollArea>
  );
}
