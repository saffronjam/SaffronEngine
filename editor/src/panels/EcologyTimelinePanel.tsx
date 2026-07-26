/// The Ecology Timeline panel: pause, step, and run over the world's biological clock, with the
/// dependency regions and their catch-up state beside it.
///
/// Biological time only moves forward, and it moves by executing ticks — there is no analytical
/// fast-forward to scrub against. So the controls here are step and run, never a seek bar: a slider
/// that could drag the clock backwards would promise something the simulation cannot do.
///
/// A region only advances while every cell it spans is resident, so the table separates "caught up"
/// from "waiting on residency". Those are different problems: the first is work still owed, the second
/// is ground that has not loaded.
import { useCallback, useEffect, useRef, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { CommandResultMap } from "../protocol";

type EcologyStatus = CommandResultMap["vegetation-ecology-status"];
type EcologyReport = CommandResultMap["vegetation-advance-ecology"];
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/// Ticks one "run" advances before yielding, so a long catch-up stays interruptible.
const RUN_CHUNK = 8;
/// Ticks one call may execute, which bounds how long the engine holds the frame.
const MAX_TICKS_PER_CALL = 64;

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

/// A world cell as the engine addresses it.
function cellLabel(cell: { coordinates: string[]; level: number }): string {
  return `${cell.coordinates.join(", ")} L${cell.level}`;
}

/// A region's stable identity: its cells, which the engine reports in canonical order.
function regionKey(region: { cells: { coordinates: string[]; level: number }[] }): string {
  return region.cells.map(cellLabel).join("|");
}

export function EcologyTimelinePanel() {
  const [status, setStatus] = useState<EcologyStatus | null>(null);
  const [report, setReport] = useState<EcologyReport | null>(null);
  const [running, setRunning] = useState(false);
  const [water, setWater] = useState(32768);
  const [warmth, setWarmth] = useState(32768);
  // A ref rather than state: the run loop reads it between calls, and a stale closure would keep
  // stepping after the user pressed pause.
  const runningRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await client.vegetationEcologyStatus());
    } catch (err) {
      notifyError(errorText(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const advance = useCallback(
    async (ticks: number) => {
      const current = status?.worldTick ?? "0";
      try {
        const next = (BigInt(current) + BigInt(ticks)).toString();
        setReport(
          await client.vegetationAdvanceEcology({
            targetTick: next,
            maxTicks: Math.min(ticks, MAX_TICKS_PER_CALL),
            water,
            warmth,
          }),
        );
      } catch (err) {
        notifyError(errorText(err));
        return false;
      }
      await refresh();
      return true;
    },
    [refresh, status?.worldTick, warmth, water],
  );

  const run = useCallback(async () => {
    if (runningRef.current) {
      runningRef.current = false;
      setRunning(false);
      return;
    }
    runningRef.current = true;
    setRunning(true);
    // Chunked rather than one long call: a catch-up that owes thousands of ticks stays interruptible,
    // and the panel keeps repainting while it works.
    while (runningRef.current) {
      if (!(await advance(RUN_CHUNK))) {
        break;
      }
    }
    runningRef.current = false;
    setRunning(false);
  }, [advance]);

  useEffect(
    () => () => {
      runningRef.current = false;
    },
    [],
  );

  const regions = status?.regions ?? [];
  const waiting = regions.filter((region) => !region.resident).length;
  const owed = regions.filter((region) => region.resident && !region.caughtUp).length;

  return (
    <div className="flex h-full flex-col gap-2 p-2">
      <div className="flex items-center gap-1">
        <Button size="sm" variant="secondary" onClick={() => void advance(1)} disabled={running}>
          Step
        </Button>
        <Button size="sm" variant={running ? "destructive" : "default"} onClick={() => void run()}>
          {running ? "Pause" : "Run"}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => void refresh()} disabled={running}>
          Refresh
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-2">
        <div className="flex flex-col gap-1">
          <Label className="text-[11px] text-muted-foreground">Water</Label>
          <Input
            className="h-7 font-mono text-[11px]"
            type="number"
            min={0}
            max={65535}
            value={water}
            onChange={(event) => setWater(Number(event.target.value))}
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label className="text-[11px] text-muted-foreground">Warmth</Label>
          <Input
            className="h-7 font-mono text-[11px]"
            type="number"
            min={0}
            max={65535}
            value={warmth}
            onChange={(event) => setWarmth(Number(event.target.value))}
          />
        </div>
      </div>
      <Separator />
      <div className="flex flex-col gap-0.5">
        <Stat label="World tick" value={status?.worldTick ?? "—"} />
        <Stat label="Rule set" value={status?.simulationVersion ?? "—"} />
        <Stat label="Region radius" value={`${status?.regionRadiusCells ?? "—"} cells`} />
        <Stat label="Regions" value={regions.length} />
        <Stat label="Owed work" value={owed} />
        <Stat label="Waiting on residency" value={waiting} />
        <Stat
          label="Checkpoint"
          value={
            <Tooltip>
              <TooltipTrigger asChild>
                <span>{(status?.checkpoint ?? "—").slice(0, 12)}</span>
              </TooltipTrigger>
              <TooltipContent className="font-mono text-[11px]">
                {status?.checkpoint ?? "no bound generation"}
              </TooltipContent>
            </Tooltip>
          }
        />
      </div>
      {report !== null && (
        <>
          <Separator />
          <div className="flex flex-col gap-0.5">
            <Stat label="Ticks run" value={report.ticksRun} />
            <Stat label="Ticks owed" value={report.ticksOwed} />
            <Stat label="Caught up" value={`${report.regionsCaughtUp} / ${report.regions}`} />
          </div>
        </>
      )}
      <Separator />
      <ScrollArea className="min-h-0 flex-1">
        <table className="w-full text-[11px]">
          <thead className="text-muted-foreground">
            <tr>
              <th className="text-left font-normal">Region</th>
              <th className="text-right font-normal">Tick</th>
              <th className="text-right font-normal">State</th>
            </tr>
          </thead>
          <tbody className="font-mono tabular-nums">
            {regions.map((region) => (
              <tr key={regionKey(region)} className="border-t border-border/40">
                <td className="py-0.5 pr-2">
                  {region.cells.length === 1
                    ? cellLabel(region.cells[0]!)
                    : `${region.cells.length} cells`}
                </td>
                <td className="py-0.5 text-right">{region.tick}</td>
                <td
                  className={cn(
                    "py-0.5 text-right",
                    !region.resident && "text-amber-500",
                    region.resident && !region.caughtUp && "text-blue-400",
                  )}
                >
                  {!region.resident ? "waiting" : region.caughtUp ? "current" : "behind"}
                </td>
              </tr>
            ))}
            {regions.length === 0 && (
              <tr>
                <td className="py-1 text-muted-foreground" colSpan={3}>
                  No dependency region has a cooked cell resident.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </ScrollArea>
    </div>
  );
}
