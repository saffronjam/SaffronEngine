/// The Plant workspace's wind and interaction preview. The controls write the real wind and
/// interaction fields — the same ones a scene uses — so the preview is the plant, not a model of it.
/// Wind speed offers named conditions as well as a slider, because a stiff sapling only separates
/// from a supple reed once the field is strong enough.
import { useCallback, useState } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Slider } from "@/components/ui/slider";

/// Wind speeds the presets step through, in metres per second: still, a breeze, and a gale. Named
/// rather than free-scrubbed because the interesting comparison is between conditions, not between
/// neighbouring tenths.
const PRESETS: { label: string; speed: number; gust: number }[] = [
  { label: "Still", speed: 0, gust: 0 },
  { label: "Breeze", speed: 4, gust: 0.2 },
  { label: "Wind", speed: 12, gust: 0.6 },
  { label: "Gale", speed: 22, gust: 0.9 },
];

/// A push strong enough to reach the texel oscillator's one-metre displacement clamp, and wide
/// enough to cover a previewed plant wherever the preview scene placed it.
const IMPULSE = { radiusM: 32, strength: 40, depress: 2 };

export function PlantWindPanel() {
  const [speed, setSpeed] = useState(0);
  const [gust, setGust] = useState(0);
  const [orientation, setOrientation] = useState(0);

  const applyWind = useCallback(
    async (next: { speed?: number; gust?: number; orientation?: number }) => {
      const settings = {
        speed: next.speed ?? speed,
        gust: next.gust ?? gust,
        orientation: next.orientation ?? orientation,
      };
      setSpeed(settings.speed);
      setGust(settings.gust);
      setOrientation(settings.orientation);
      try {
        await client.setWind(settings);
      } catch (err) {
        notifyError(errorText(err));
      }
    },
    [gust, orientation, speed],
  );

  const push = useCallback(async (direction: [number, number]) => {
    try {
      // The preview subject stands at the origin of its own scene, so the impulse is centred there.
      // A DIRECTION is explicit rather than left radial: a directionless impulse pushes outward from
      // its own centre, which cancels at the centre and looks exactly like nothing happening.
      await client.emitInteractionImpulse({
        positionM: [0, 0],
        radiusM: IMPULSE.radiusM,
        strength: IMPULSE.strength,
        direction,
        depress: IMPULSE.depress,
      });
    } catch (err) {
      notifyError(errorText(err));
    }
  }, []);

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-3">
        <section className="flex flex-col gap-2">
          <h3 className="text-[11px] font-medium text-foreground">Wind</h3>
          <div className="flex flex-wrap gap-1">
            {PRESETS.map((preset) => (
              <Button
                className="h-7 text-[11px]"
                key={preset.label}
                onClick={() => void applyWind({ gust: preset.gust, speed: preset.speed })}
                size="sm"
                variant={speed === preset.speed ? "default" : "outline"}
              >
                {preset.label}
              </Button>
            ))}
          </div>
          <div className="flex flex-col gap-1">
            <Label className="text-[11px] text-muted-foreground">
              Speed <span className="font-mono tabular-nums">{speed.toFixed(1)} m/s</span>
            </Label>
            <Slider
              max={30}
              min={0}
              onValueChange={([value]) => void applyWind({ speed: value ?? 0 })}
              step={0.5}
              value={[speed]}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label className="text-[11px] text-muted-foreground">
              Gust <span className="font-mono tabular-nums">{gust.toFixed(2)}</span>
            </Label>
            <Slider
              max={1}
              min={0}
              onValueChange={([value]) => void applyWind({ gust: value ?? 0 })}
              step={0.05}
              value={[gust]}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label className="text-[11px] text-muted-foreground">
              Direction <span className="font-mono tabular-nums">{orientation.toFixed(0)}°</span>
            </Label>
            <Slider
              max={360}
              min={0}
              onValueChange={([value]) => void applyWind({ orientation: value ?? 0 })}
              step={5}
              value={[orientation]}
            />
          </div>
        </section>

        <Separator />

        <section className="flex flex-col gap-2">
          <h3 className="text-[11px] font-medium text-foreground">Interaction</h3>
          <p className="text-[10px] text-muted-foreground">
            Pushes the world interaction field at the subject. The field is a damped oscillator, so
            the plant leans and springs back over about a second.
          </p>
          <div className="flex flex-wrap gap-1">
            <Button
              className="h-7 text-[11px]"
              onClick={() => void push([1, 0])}
              size="sm"
              variant="outline"
            >
              Push +X
            </Button>
            <Button
              className="h-7 text-[11px]"
              onClick={() => void push([-1, 0])}
              size="sm"
              variant="outline"
            >
              Push −X
            </Button>
            <Button
              className="h-7 text-[11px]"
              onClick={() => void push([0, 1])}
              size="sm"
              variant="outline"
            >
              Push +Z
            </Button>
            <Button
              className="h-7 text-[11px]"
              onClick={() => void push([0, -1])}
              size="sm"
              variant="outline"
            >
              Push −Z
            </Button>
          </div>
        </section>
      </div>
    </ScrollArea>
  );
}
