import {
  IDENTITY_CURVE,
  ToneCurve,
  type CurvePoint,
  type ToneCurveChannels,
} from "../../components/ToneCurve";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import type { Environment } from "../../protocol";
import type { EnvironmentEditor } from "./useEnvironmentEditor";

type TimeOfDay = Environment["timeOfDay"];

const identityCurve = (): CurvePoint[] => IDENTITY_CURVE.map((point) => ({ ...point }));

const scalarChannels = (curve: CurvePoint[]): ToneCurveChannels => ({
  master: curve,
  r: [],
  g: [],
  b: [],
});

const tintChannels = (tint: TimeOfDay["tintCurve"]): ToneCurveChannels => ({
  master: tint.master,
  r: tint.red,
  g: tint.green,
  b: tint.blue,
});

/// One curve card: the enable switch swaps between an empty curve list (automation off) and the
/// identity curve.
function ScalarCurveCard({
  title,
  masterLabel,
  editor,
  curve,
  onChange,
}: {
  title: string;
  masterLabel: string;
  editor: EnvironmentEditor;
  curve: CurvePoint[];
  onChange(curve: CurvePoint[]): void;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-md border border-border p-3">
      <div className="flex items-center justify-between">
        <Label>{title}</Label>
        <Switch
          checked={curve.length > 0}
          onCheckedChange={(checked) => onChange(checked ? identityCurve() : [])}
        />
      </div>
      {curve.length > 0 ? (
        <ToneCurve
          channels={scalarChannels(curve)}
          visibleChannels={["master"]}
          masterLabel={masterLabel}
          onChange={(channels) => onChange(channels.master)}
          onDragStart={editor.onDragStart}
          onDragEnd={editor.onDragEnd}
        />
      ) : null}
    </div>
  );
}

/// Time-of-day appearance curves, evaluated from normalized sun elevation.
export function CurvesDialog({
  open,
  onOpenChange,
  tod,
  editor,
}: {
  open: boolean;
  onOpenChange(open: boolean): void;
  tod: TimeOfDay;
  editor: EnvironmentEditor;
}) {
  const patchTod = <K extends keyof TimeOfDay>(field: K, value: TimeOfDay[K]): void =>
    editor.patchBlock("timeOfDay", field, value);
  const tintEnabled = Object.values(tod.tintCurve).some((curve) => curve.length > 0);

  return (
    <Dialog open={open} onOpenChange={onOpenChange} perfLabel="environment-curves">
      <DialogContent className="flex h-[min(760px,calc(100%-2rem))] max-w-[min(960px,calc(100%-2rem))] grid-rows-none flex-col sm:max-w-[min(960px,calc(100%-2rem))]">
        <DialogHeader>
          <DialogTitle>Time-of-day appearance curves</DialogTitle>
          <DialogDescription>
            Curves are evaluated from normalized sun elevation and automate the rendered world.
          </DialogDescription>
        </DialogHeader>
        <ScrollArea className="min-h-0 flex-1">
          <div className="grid gap-3 pr-3 lg:grid-cols-2">
            <ScalarCurveCard
              title="Exposure"
              masterLabel="EV"
              editor={editor}
              curve={tod.exposureCurve}
              onChange={(curve) => patchTod("exposureCurve", curve)}
            />

            <div className="flex flex-col gap-2 rounded-md border border-border p-3">
              <div className="flex items-center justify-between">
                <Label>Sky tint</Label>
                <Switch
                  checked={tintEnabled}
                  onCheckedChange={(checked) =>
                    patchTod(
                      "tintCurve",
                      checked
                        ? {
                            master: identityCurve(),
                            red: identityCurve(),
                            green: identityCurve(),
                            blue: identityCurve(),
                          }
                        : { master: [], red: [], green: [], blue: [] },
                    )
                  }
                />
              </div>
              {tintEnabled ? (
                <ToneCurve
                  channels={tintChannels(tod.tintCurve)}
                  onChange={(channels) =>
                    patchTod("tintCurve", {
                      master: channels.master,
                      red: channels.r,
                      green: channels.g,
                      blue: channels.b,
                    })
                  }
                  onDragStart={editor.onDragStart}
                  onDragEnd={editor.onDragEnd}
                />
              ) : null}
            </div>

            <ScalarCurveCard
              title="Cloud coverage"
              masterLabel="Cloud"
              editor={editor}
              curve={tod.coverageCurve}
              onChange={(curve) => patchTod("coverageCurve", curve)}
            />

            <ScalarCurveCard
              title="Cloud type"
              masterLabel="Type"
              editor={editor}
              curve={tod.cloudTypeCurve}
              onChange={(curve) => patchTod("cloudTypeCurve", curve)}
            />
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button type="button" onClick={() => onOpenChange(false)}>
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
