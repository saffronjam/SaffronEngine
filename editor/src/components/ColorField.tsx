/// Color field for Vec3/Vec4 channels, which are LINEAR floats in 0..1 on the wire. The swatch opens
/// a saturation/hue (and alpha) canvas; per-channel numeric inputs keep HDR range and alpha editable
/// beyond what the canvas exposes. Renders drag-local state so the canvas tracks the pointer
/// exactly; the panel owns coalescing and drag-gating.
import { RgbaColorPicker, RgbColorPicker } from "react-colorful";
import { VectorEditor } from "./VectorEditor";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { useScrubValue } from "@/lib/useScrubValue";

export interface ColorFieldProps {
  /// "color3" for Vec3 (rgb) or "color4" for Vec4 (rgba); alpha shown only for color4.
  kind: "color3" | "color4";
  value: Record<string, number>;
  /// One atomic patch per edit: the canvas emits all changed channels together so a
  /// drag is a single write, never per-axis calls racing onto a stale base.
  onChange(patch: Record<string, number>): void;
  onDragStart?(): void;
  onDragEnd?(): void;
}

function channelToByte(c: number): number {
  return Math.round(Math.min(1, Math.max(0, Number.isFinite(c) ? c : 0)) * 255);
}

function channelToHex(c: number): string {
  return channelToByte(c).toString(16).padStart(2, "0");
}

/// Cap a channel readout to 4 characters so it fits the compact node fields (the wire keeps full
/// precision; this is display only): 0.678 → "0.68", 0.314 → "0.31", 1 → "1", 12.34 → "12.3".
function formatChannel(n: number): string {
  if (!Number.isFinite(n)) {
    return "0";
  }
  for (let decimals = 2; decimals >= 0; decimals -= 1) {
    const s = Number(n.toFixed(decimals)).toString();
    if (s.length <= 4) {
      return s;
    }
  }
  return Number(n.toFixed(0)).toString();
}

export function ColorField({ kind, value, onChange, onDragStart, onDragEnd }: ColorFieldProps) {
  const hasAlpha = kind === "color4";
  const channels = hasAlpha ? (["x", "y", "z", "w"] as const) : (["x", "y", "z"] as const);
  const channelLabels = hasAlpha ? ["R", "G", "B", "A"] : ["R", "G", "B"];
  const scrub = useScrubValue(value, onChange);
  const color = scrub.value;

  const hex = `#${channelToHex(color.x ?? 0)}${channelToHex(color.y ?? 0)}${channelToHex(color.z ?? 0)}`;

  // Gate the reconcile poll across a canvas drag: a pointerdown on the picker opens
  // the drag, a single window pointerup closes it (flush before ungating so the
  // panel's release re-push reads the final value).
  const beginCanvasDrag = (): void => {
    scrub.begin();
    onDragStart?.();
    const end = (): void => {
      scrub.end();
      onDragEnd?.();
      window.removeEventListener("pointerup", end);
    };
    window.addEventListener("pointerup", end);
  };

  const rgbPatch = (rgb: { r: number; g: number; b: number }): Record<string, number> => ({
    x: Number((rgb.r / 255).toFixed(3)),
    y: Number((rgb.g / 255).toFixed(3)),
    z: Number((rgb.b / 255).toFixed(3)),
  });

  return (
    <div className="flex items-center gap-1.5">
      <Popover>
        <PopoverTrigger asChild>
          <button
            type="button"
            className="h-[22px] w-[26px] flex-none cursor-pointer rounded-sm border border-border"
            style={{ backgroundColor: hex }}
            aria-label="Pick color"
          />
        </PopoverTrigger>
        <PopoverContent className="w-auto p-2" align="start" onPointerDownCapture={beginCanvasDrag}>
          <div className={cn("color-picker-canvas", hasAlpha && "color-picker-canvas--alpha")}>
            {hasAlpha ? (
              <RgbaColorPicker
                color={{
                  r: channelToByte(color.x ?? 0),
                  g: channelToByte(color.y ?? 0),
                  b: channelToByte(color.z ?? 0),
                  a: Math.min(1, Math.max(0, color.w ?? 1)),
                }}
                onChange={(c) => scrub.set({ ...color, ...rgbPatch(c), w: Number(c.a.toFixed(3)) })}
              />
            ) : (
              <RgbColorPicker
                color={{
                  r: channelToByte(color.x ?? 0),
                  g: channelToByte(color.y ?? 0),
                  b: channelToByte(color.z ?? 0),
                }}
                onChange={(c) => scrub.set({ ...color, ...rgbPatch(c) })}
              />
            )}
          </div>
        </PopoverContent>
      </Popover>
      <div className="min-w-0 flex-1">
        <VectorEditor
          axes={channels}
          labels={channelLabels}
          value={color}
          step={0.01}
          format={formatChannel}
          onChange={onChange}
          onDragStart={onDragStart}
          onDragEnd={onDragEnd}
        />
      </div>
    </div>
  );
}
