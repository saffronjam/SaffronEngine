/// A Resolve-style color trackball: a hue/saturation pad with a draggable chroma puck plus a
/// vertical luma-trim bar. It encodes an RGB triplet as a uniform luma level plus a zero-sum chroma
/// offset, so centred + neutral is the identity correction. The chroma lives in the plane orthogonal
/// to grey spanned by two orthonormal RGB basis vectors, making the disc↔triplet map exact and
/// invertible, so an external value round-trips to the same puck position.
///
/// Owns drag-local rendering and brackets a gesture with `onDragStart`/`onDragEnd`; the panel owns
/// the coalesced emit.
import { useMemo, useRef } from "react";
import { useScrubValue } from "@/lib/useScrubValue";

type Triplet = [number, number, number];

export interface GradingWheelProps {
  label: string;
  /// The current triplet (CDL offset, slope, or power) the disc + bar encode.
  value: Triplet;
  /// The uniform luma level the bar rests at with no correction (0 for offset, 1 for slope/power).
  neutral: number;
  /// Half-extent of the per-channel chroma push at the disc edge, around the luma level.
  chromaRange: number;
  /// The luma-trim bar bounds (its neutral is `neutral`).
  masterMin: number;
  masterMax: number;
  /// Emits the recomputed triplet; `master` is the decoded uniform luma level (bar position).
  onChange(rgb: Triplet, master: number): void;
  onDragStart?(): void;
  onDragEnd?(): void;
}

/// Orthonormal basis of the zero-sum (grey-orthogonal) chroma plane. `B1` is the red↔cyan axis
/// (screen x), `B2` the green↔magenta axis (screen y).
const B1: Triplet = [2 / Math.sqrt(6), -1 / Math.sqrt(6), -1 / Math.sqrt(6)];
const B2: Triplet = [0, 1 / Math.sqrt(2), -1 / Math.sqrt(2)];

interface WheelState {
  rgb: Triplet;
  master: number;
}

/// Decode a triplet into disc coordinates (`px`/`py` in the unit disc); the bar's luma level is the
/// triplet's mean.
function decode(value: Triplet, chromaRange: number): { px: number; py: number } {
  const master = (value[0] + value[1] + value[2]) / 3;
  const ch: Triplet = [value[0] - master, value[1] - master, value[2] - master];
  const dot = (b: Triplet): number => ch[0] * b[0] + ch[1] * b[1] + ch[2] * b[2];
  const px = chromaRange > 0 ? dot(B1) / chromaRange : 0;
  const py = chromaRange > 0 ? dot(B2) / chromaRange : 0;
  return { px, py };
}

/// Compose disc coordinates + a luma level back into a triplet.
function compose(px: number, py: number, master: number, chromaRange: number): Triplet {
  const chroma = (i: 0 | 1 | 2): number => (px * B1[i] + py * B2[i]) * chromaRange;
  return [master + chroma(0), master + chroma(1), master + chroma(2)];
}

function toHex(c: number): string {
  const v = Math.round(Math.min(1, Math.max(0, c)) * 255);
  return v.toString(16).padStart(2, "0");
}

export function GradingWheel({
  label,
  value,
  neutral,
  chromaRange,
  masterMin,
  masterMax,
  onChange,
  onDragStart,
  onDragEnd,
}: GradingWheelProps) {
  const initial = useMemo<WheelState>(() => ({ rgb: value, master: masterOf(value) }), [value]);
  const scrub = useScrubValue<WheelState>(initial, (s) => onChange(s.rgb, s.master));
  const discRef = useRef<HTMLDivElement>(null);

  const { px, py } = decode(scrub.value.rgb, chromaRange);
  const master = scrub.value.master;
  const masterT = masterMax > masterMin ? (master - masterMin) / (masterMax - masterMin) : 0;

  // The colour under the puck (grey + the current chroma push), for the puck chip + a live preview.
  const chip = compose(px, py, 0.5, chromaRange * 0.5);
  const chipHex = `#${toHex(chip[0])}${toHex(chip[1])}${toHex(chip[2])}`;

  const setDisc = (clientX: number, clientY: number): void => {
    const el = discRef.current;
    if (!el) {
      return;
    }
    const rect = el.getBoundingClientRect();
    const half = rect.width / 2;
    let nx = (clientX - rect.left - half) / half;
    let ny = (clientY - rect.top - half) / half;
    const len = Math.hypot(nx, ny);
    if (len > 1) {
      nx /= len;
      ny /= len;
    }
    scrub.set({
      rgb: compose(nx, ny, scrub.value.master, chromaRange),
      master: scrub.value.master,
    });
  };

  const beginDisc = (e: React.PointerEvent<HTMLDivElement>): void => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    scrub.begin();
    onDragStart?.();
    setDisc(e.clientX, e.clientY);
  };
  const moveDisc = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      setDisc(e.clientX, e.clientY);
    }
  };
  const endDisc = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
      scrub.end();
      onDragEnd?.();
    }
  };

  const setBar = (clientY: number, el: HTMLDivElement): void => {
    const rect = el.getBoundingClientRect();
    const t = 1 - Math.min(1, Math.max(0, (clientY - rect.top) / rect.height));
    const next = masterMin + t * (masterMax - masterMin);
    const cur = decode(scrub.value.rgb, chromaRange);
    scrub.set({ rgb: compose(cur.px, cur.py, next, chromaRange), master: next });
  };
  const beginBar = (e: React.PointerEvent<HTMLDivElement>): void => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    scrub.begin();
    onDragStart?.();
    setBar(e.clientY, e.currentTarget);
  };
  const moveBar = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      setBar(e.clientY, e.currentTarget);
    }
  };
  const endBar = (e: React.PointerEvent<HTMLDivElement>): void => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
      scrub.end();
      onDragEnd?.();
    }
  };

  const reset = (): void => {
    onChange([neutral, neutral, neutral], neutral);
  };

  return (
    <div className="flex flex-col items-center gap-1">
      <div className="flex items-end gap-1.5">
        <div
          ref={discRef}
          className="relative size-[92px] cursor-crosshair touch-none rounded-full border border-border"
          style={{
            background:
              "radial-gradient(circle, hsl(0 0% 100% / 0.95) 0%, hsl(0 0% 100% / 0) 62%), conic-gradient(from 90deg, #ff4d4d, #ffe14d, #4dff77, #4de1ff, #4d77ff, #e14dff, #ff4d4d)",
          }}
          onPointerDown={beginDisc}
          onPointerMove={moveDisc}
          onPointerUp={endDisc}
          onPointerCancel={endDisc}
          onDoubleClick={reset}
        >
          <div
            className="pointer-events-none absolute size-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full border border-white shadow ring-1 ring-black/50"
            style={{
              left: `${50 + px * 50}%`,
              top: `${50 + py * 50}%`,
              backgroundColor: chipHex,
            }}
          />
        </div>
        <div
          className="relative h-[92px] w-2.5 cursor-ns-resize touch-none overflow-hidden rounded-full border border-border bg-gradient-to-t from-black to-white"
          onPointerDown={beginBar}
          onPointerMove={moveBar}
          onPointerUp={endBar}
          onPointerCancel={endBar}
          onDoubleClick={reset}
        >
          <div
            className="pointer-events-none absolute left-1/2 h-1.5 w-3 -translate-x-1/2 -translate-y-1/2 rounded-sm border border-black/50 bg-white shadow"
            style={{ top: `${(1 - masterT) * 100}%` }}
          />
        </div>
      </div>
      <span className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

/// The decoded uniform luma level of a triplet (the bar position).
function masterOf(value: Triplet): number {
  return (value[0] + value[1] + value[2]) / 3;
}
