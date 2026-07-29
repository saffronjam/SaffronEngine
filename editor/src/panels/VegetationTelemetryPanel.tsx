/// The Vegetation Telemetry panel: where the runtime's time and memory go, and which content is
/// over budget.
///
/// The engine already published all of this — `vegetation-telemetry` has been on the wire and
/// reachable from `sa` for a while — but nothing in the editor called it, so the numbers only
/// existed for someone who already suspected a problem and knew the command. A panel is what turns
/// them into something noticed.
///
/// Two halves, deliberately: the stage times say WHERE the frame's vegetation work went, and the
/// budgets say WHOSE content is responsible when it is too much. A stage time alone tells you
/// residency is slow; the owner column tells you which cell to open.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { CommandResultMap } from "../protocol";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";

type Telemetry = CommandResultMap["vegetation-telemetry"];
type Budgets = CommandResultMap["vegetation-budgets"];
type Alarms = CommandResultMap["list-active-alarms"];

/// How often the panel re-reads. Telemetry is compact counters and averaged stage times, so this
/// is a handful of numbers over the socket — but it is still a poll, so it stays slow enough to be
/// free and fast enough to watch a cook.
const REFRESH_MS = 1000;

function microseconds(value: number): string {
  return value >= 1000 ? `${(value / 1000).toFixed(2)} ms` : `${value} µs`;
}

function bytes(value: string): string {
  const count = Number(value);
  if (!Number.isFinite(count)) {
    return value;
  }
  if (count >= 1024 * 1024) {
    return `${(count / (1024 * 1024)).toFixed(1)} MiB`;
  }
  if (count >= 1024) {
    return `${(count / 1024).toFixed(1)} KiB`;
  }
  return `${count} B`;
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="font-mono text-[11px] tabular-nums text-foreground">{value}</span>
    </div>
  );
}

export function VegetationTelemetryPanel() {
  const [telemetry, setTelemetry] = useState<Telemetry | null>(null);
  const [budgets, setBudgets] = useState<Budgets | null>(null);
  const [alarms, setAlarms] = useState<Alarms["alarms"]>([]);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [next, budget, active] = await Promise.all([
        client.vegetationTelemetry(),
        client.vegetationBudgets(),
        client.listActiveAlarms(),
      ]);
      setTelemetry(next);
      setBudgets(budget);
      setAlarms(
        active.alarms.filter((alarm: Alarms["alarms"][number]) =>
          alarm.metric.startsWith("vegetation-"),
        ),
      );
      setUnavailable(null);
    } catch (err) {
      // A world with no bound vegetation runtime answers with an error rather than zeros, which is
      // the honest reply — so it is shown as the panel's state instead of raising a toast every
      // second the user has no vegetation loaded.
      setTelemetry(null);
      setUnavailable(errorText(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const commitBudget = useCallback(
    async (patch: Parameters<typeof client.vegetationBudgets>[0]) => {
      try {
        setBudgets(await client.vegetationBudgets(patch));
      } catch (err) {
        notifyError(errorText(err));
      }
    },
    [],
  );

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        {unavailable ? <p className="text-[11px] text-muted-foreground">{unavailable}</p> : null}

        {telemetry ? (
          <>
            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">Stage times (average)</h3>
              <Row label="Residency" value={microseconds(telemetry.average.residencyUs)} />
              <Row label="Promotion" value={microseconds(telemetry.average.promotionUs)} />
              <Row label="Collision" value={microseconds(telemetry.average.collisionUs)} />
              <Row label="Navigation" value={microseconds(telemetry.average.navigationUs)} />
              <Row label="Ecology" value={microseconds(telemetry.average.ecologyUs)} />
              <Separator className="my-1" />
              <Row label="Total" value={microseconds(telemetry.average.totalUs)} />
              <Row label="Last sync" value={microseconds(telemetry.last.totalUs)} />
            </section>

            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">Resident bytes</h3>
              <Row label="Render" value={bytes(telemetry.residentBytes.render)} />
              <Row label="Physics" value={bytes(telemetry.residentBytes.physics)} />
              <Row label="Simulation" value={bytes(telemetry.residentBytes.simulation)} />
              <Row label="Editing" value={bytes(telemetry.residentBytes.editing)} />
              <Row label="Navigation" value={bytes(telemetry.residentBytes.navigation)} />
              <Row label="Network" value={bytes(telemetry.residentBytes.network)} />
            </section>

            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">Work</h3>
              <Row label="Synchronizations" value={telemetry.work.synchronizations} />
              <Row label="Queries" value={telemetry.work.queries} />
              <Row label="Query hits" value={telemetry.work.queryHits} />
              <Row label="Mutations" value={telemetry.work.mutations} />
              <Row label="Ecology ticks" value={telemetry.work.ecologyTicks} />
              <Row label="Collision bodies" value={telemetry.collisionBodies} />
              <Row label="Navigation contributions" value={telemetry.navigationContributions} />
              <Row label="Promoted plants" value={telemetry.promoted} />
            </section>

            <section className="flex flex-col gap-1">
              <h3 className="text-[11px] font-medium text-foreground">Cook queue</h3>
              <Row label="Live" value={telemetry.cookQueue.live} />
              <Row label="Completed" value={telemetry.cookQueue.completed} />
              <Row label="Cancelled" value={telemetry.cookQueue.cancelled} />
              <Row label="Superseded" value={telemetry.cookQueue.superseded} />
              <Row label="Failed" value={telemetry.cookQueue.failed} />
            </section>
          </>
        ) : null}

        <section className="flex flex-col gap-2">
          <h3 className="text-[11px] font-medium text-foreground">Budgets</h3>
          {budgets ? (
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <Label className="w-32 text-[11px] text-muted-foreground">Plants per cell</Label>
                <Input
                  className="h-7 flex-1 font-mono text-[11px]"
                  defaultValue={budgets.cellPlants}
                  key={`cell-${budgets.cellPlants}`}
                  onBlur={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isFinite(value) && value >= 0) {
                      void commitBudget({ cellPlants: Math.trunc(value) });
                    }
                  }}
                />
              </div>
              <div className="flex items-center gap-2">
                <Label className="w-32 text-[11px] text-muted-foreground">
                  Instances per family
                </Label>
                <Input
                  className="h-7 flex-1 font-mono text-[11px]"
                  defaultValue={budgets.familyInstances}
                  key={`family-${budgets.familyInstances}`}
                  onBlur={(event) => {
                    const value = Number(event.currentTarget.value);
                    if (Number.isFinite(value) && value >= 0) {
                      void commitBudget({ familyInstances: Math.trunc(value) });
                    }
                  }}
                />
              </div>
              <p className="text-[10px] text-muted-foreground">
                Zero turns a budget off. A breach raises an alarm naming the cell or family that
                broke it.
              </p>
            </div>
          ) : null}

          {alarms.length > 0 ? (
            <div className="flex flex-col gap-1">
              {alarms.map((alarm) => (
                <div
                  className="flex flex-col rounded border border-destructive/40 bg-destructive/10 px-2 py-1"
                  key={alarm.fingerprint}
                >
                  <span className="font-mono text-[11px] text-foreground">{alarm.owner}</span>
                  <span className="text-[10px] text-muted-foreground">
                    {alarm.metric} — {alarm.value} over {alarm.threshold}
                  </span>
                </div>
              ))}
            </div>
          ) : (
            <p className="text-[10px] text-muted-foreground">Nothing over budget.</p>
          )}
        </section>

        <Button
          className="h-7 self-start text-[11px]"
          onClick={() => void refresh()}
          size="sm"
          variant="outline"
        >
          Refresh
        </Button>
      </div>
    </ScrollArea>
  );
}
