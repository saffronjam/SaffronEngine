/// The wind debug read-out: what the field is doing at a probe position, and what the world
/// interaction field is holding. Everything here is captured from the engine — the spectrum and the
/// source influences come from the same `sample_composed` the deformation prepass evaluates, and the
/// field grid is a readback of the live cascade — so a value that looks wrong here is wrong in the
/// frame, not in a second model of it.
import { useCallback, useEffect, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import type { SampleWindResult, WindInteractionFieldResult } from "../protocol";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";

/// Grid side of the interaction-field read-out. Coarse on purpose: a cascade is 256 texels a side
/// and the question a person asks of it is where the disturbance is, not which texel holds it.
const FIELD_RESOLUTION = 16;

function magnitude(vector: number[]): number {
  return Math.hypot(...vector);
}

function vectorText(vector: number[]): string {
  return vector.map((value) => value.toFixed(2)).join(", ");
}

export function WindDebugPanel() {
  const [position, setPosition] = useState<[number, number, number]>([0, 2, 0]);
  const [sample, setSample] = useState<SampleWindResult | null>(null);
  const [field, setField] = useState<WindInteractionFieldResult | null>(null);
  const [cascade, setCascade] = useState(0);
  const [live, setLive] = useState(false);

  const probe = useCallback(async () => {
    try {
      setSample(await client.sampleWind({ positionM: position }));
    } catch (err) {
      notifyError(errorText(err));
    }
  }, [position]);

  const readField = useCallback(async () => {
    try {
      setField(await client.windInteractionField({ cascade, resolution: FIELD_RESOLUTION }));
    } catch (err) {
      notifyError(errorText(err));
    }
  }, [cascade]);

  useEffect(() => {
    if (!live) {
      return;
    }
    // The field capture idles the GPU queue, so live polling stays well away from frame rate.
    const id = window.setInterval(() => {
      void probe();
      void readField();
    }, 500);
    return () => window.clearInterval(id);
  }, [live, probe, readField]);

  const peak = sample
    ? Math.max(...sample.octaves.map((row) => magnitude(row.velocityMps)), 1e-6)
    : 1;
  /// The grid cells carry their own coordinate, which is what identifies a cell across reads —
  /// the array position is an implementation detail of the row-major layout.
  const grid = (field?.cells ?? []).map((cell, index) => ({
    key: `${index % (field?.resolution ?? 1)},${Math.floor(index / (field?.resolution ?? 1))}`,
    push: Math.hypot(cell[0], cell[1]),
    depress: cell[2],
  }));

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        <section className="flex flex-col gap-2">
          <div className="flex items-center justify-between">
            <h3 className="text-[11px] font-medium text-foreground">Probe</h3>
            <div className="flex gap-1">
              <Button
                className="h-7 text-[11px]"
                onClick={() => void probe()}
                size="sm"
                variant="outline"
              >
                Sample
              </Button>
              <Button
                className="h-7 text-[11px]"
                onClick={() => setLive((value) => !value)}
                size="sm"
                variant={live ? "default" : "outline"}
              >
                Live
              </Button>
            </div>
          </div>
          <div className="flex gap-1">
            {(["X", "Y", "Z"] as const).map((axis, index) => (
              <div className="flex flex-1 flex-col gap-1" key={axis}>
                <Label className="text-[10px] text-muted-foreground">{axis}</Label>
                <Input
                  className="h-7 text-[11px]"
                  onChange={(event) => {
                    const value = Number(event.target.value);
                    setPosition((current) => {
                      const next: [number, number, number] = [...current];
                      next[index] = Number.isFinite(value) ? value : 0;
                      return next;
                    });
                  }}
                  type="number"
                  value={position[index]}
                />
              </div>
            ))}
          </div>
          {sample ? (
            <dl className="grid grid-cols-2 gap-x-2 gap-y-0.5 text-[11px]">
              <dt className="text-muted-foreground">Velocity</dt>
              <dd className="font-mono tabular-nums">{vectorText(sample.velocityMps)}</dd>
              <dt className="text-muted-foreground">Speed</dt>
              <dd className="font-mono tabular-nums">
                {magnitude(sample.velocityMps).toFixed(2)} m/s
              </dd>
              <dt className="text-muted-foreground">Mean</dt>
              <dd className="font-mono tabular-nums">{vectorText(sample.meanMps)}</dd>
              <dt className="text-muted-foreground">Turbulence</dt>
              <dd className="font-mono tabular-nums">{vectorText(sample.turbulenceMps)}</dd>
              <dt className="text-muted-foreground">Gust front</dt>
              <dd className="font-mono tabular-nums">{sample.gustFront.toFixed(3)}</dd>
              <dt className="text-muted-foreground">Clock</dt>
              <dd className="font-mono tabular-nums">{sample.timeS.toFixed(2)} s</dd>
            </dl>
          ) : (
            <p className="text-[10px] text-muted-foreground">Sample to read the field here.</p>
          )}
        </section>

        <Separator />

        <section className="flex flex-col gap-2">
          <h3 className="text-[11px] font-medium text-foreground">Turbulence spectrum</h3>
          {sample && sample.octaves.length > 0 ? (
            <div className="flex flex-col gap-1">
              {sample.octaves.map((row) => {
                const speed = magnitude(row.velocityMps);
                return (
                  <div className="flex items-center gap-2" key={row.octave}>
                    <span className="w-16 shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
                      {row.wavelengthM.toFixed(1)} m
                    </span>
                    <div className="h-2 flex-1 rounded-sm bg-muted">
                      <div
                        className="h-full rounded-sm bg-primary"
                        style={{ width: `${Math.min(100, (speed / peak) * 100)}%` }}
                      />
                    </div>
                    <span className="w-14 shrink-0 text-right font-mono text-[10px] tabular-nums">
                      {speed.toFixed(2)}
                    </span>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="text-[10px] text-muted-foreground">
              No turbulence octaves — the profile is laminar.
            </p>
          )}
        </section>

        <Separator />

        <section className="flex flex-col gap-2">
          <h3 className="text-[11px] font-medium text-foreground">Local sources</h3>
          {sample && sample.sources.length > 0 ? (
            <div className="flex flex-col gap-1">
              {sample.sources.map((source) => (
                <div
                  className="flex flex-col gap-0.5 rounded-sm border border-border px-2 py-1"
                  key={source.entity}
                >
                  <div className="flex items-center justify-between text-[11px]">
                    <span>{source.kind}</span>
                    <span className="font-mono tabular-nums text-muted-foreground">
                      {source.distanceM.toFixed(1)} m · w {source.weight.toFixed(2)}
                    </span>
                  </div>
                  <span className="font-mono text-[10px] tabular-nums text-muted-foreground">
                    {source.kind === "volume"
                      ? `scales ×${source.globalScale.toFixed(2)}`
                      : `adds ${vectorText(source.addedMps)}`}
                  </span>
                </div>
              ))}
            </div>
          ) : (
            <p className="text-[10px] text-muted-foreground">
              No placed wind sources reach this position.
            </p>
          )}
        </section>

        <Separator />

        <section className="flex flex-col gap-2">
          <div className="flex items-center justify-between">
            <h3 className="text-[11px] font-medium text-foreground">Interaction field</h3>
            <div className="flex gap-1">
              {[0, 1].map((index) => (
                <Button
                  className="h-7 text-[11px]"
                  key={index}
                  onClick={() => setCascade(index)}
                  size="sm"
                  variant={cascade === index ? "default" : "outline"}
                >
                  Cascade {index}
                </Button>
              ))}
              <Button
                className="h-7 text-[11px]"
                onClick={() => void readField()}
                size="sm"
                variant="outline"
              >
                Read
              </Button>
            </div>
          </div>
          {field ? (
            <>
              <dl className="grid grid-cols-2 gap-x-2 gap-y-0.5 text-[11px]">
                <dt className="text-muted-foreground">Texel size</dt>
                <dd className="font-mono tabular-nums">{field.texelMeters.toFixed(2)} m</dd>
                <dt className="text-muted-foreground">Live texels</dt>
                <dd className="font-mono tabular-nums">{field.liveTexels}</dd>
                <dt className="text-muted-foreground">Peak bend</dt>
                <dd className="font-mono tabular-nums">{field.peakDisplacementM.toFixed(3)} m</dd>
                <dt className="text-muted-foreground">Peak recovery</dt>
                <dd className="font-mono tabular-nums">{field.peakVelocityMps.toFixed(3)} m/s</dd>
              </dl>
              <div
                className="grid gap-px"
                style={{ gridTemplateColumns: `repeat(${field.resolution}, minmax(0, 1fr))` }}
              >
                {grid.map((cell) => {
                  const scale =
                    field.peakDisplacementM > 0 ? cell.push / field.peakDisplacementM : 0;
                  return (
                    <div
                      aria-label={`push ${cell.push.toFixed(3)} m, depress ${cell.depress.toFixed(3)} m`}
                      className="aspect-square rounded-[1px] bg-primary"
                      key={cell.key}
                      style={{ opacity: 0.08 + scale * 0.92 }}
                    />
                  );
                })}
              </div>
            </>
          ) : (
            <p className="text-[10px] text-muted-foreground">
              Read the field to see where it is disturbed.
            </p>
          )}
        </section>
      </div>
    </ScrollArea>
  );
}
