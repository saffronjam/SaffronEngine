/// Multi-axis vector editor: each axis label is a pointer-capture scrub handle and each value a
/// `NumericInput` that swallows its own pointer. Unit conversion happens in `fieldRenderer`, so this
/// widget stays unit-agnostic. Renders drag-local state and emits one atomic patch per edit; the
/// panel owns coalescing and drag-gating.
import { useRef } from "react";
import { NumericInput, formatNumber } from "./NumericInput";
import { cn } from "@/lib/utils";
import { useScrubValue } from "@/lib/useScrubValue";

/// Axis label colors matching the viewport gizmo (X red, Y green, Z blue): a dark
/// tint behind a bright label.
const AXIS_COLORS: Record<string, string> = {
  x: "bg-red-950 text-red-400",
  y: "bg-green-950 text-green-400",
  z: "bg-blue-950 text-blue-400",
};

export interface VectorEditorProps {
  /// Ordered axis keys into the value record, e.g. ["x","y","z"] or ["x","y","z","w"].
  axes: readonly string[];
  value: Record<string, number>;
  step?: number;
  /// Display text per axis (defaults to the axis key uppercased). Lets a color reuse the gizmo axis
  /// colors as R/G/B/A labels (axes ["x","y","z","w"], labels ["R","G","B","A"]).
  labels?: readonly string[];
  /// Tint the labels with the gizmo axis colors (X red, Y green, Z blue); off → neutral labels.
  coloredLabels?: boolean;
  /// Readout formatter (default `formatNumber`). A caller with very narrow fields (color channels in a
  /// node) can cap the width; the wire keeps full precision, this is display only.
  format?: (n: number) => string;
  /// One atomic patch per edit (the changed axes), never per-axis calls racing
  /// onto a stale base.
  onChange(patch: Record<string, number>): void;
  onDragStart?(): void;
  onDragEnd?(): void;
}

export function VectorEditor({
  axes,
  value,
  step = 0.05,
  labels,
  coloredLabels = true,
  format = formatNumber,
  onChange,
  onDragStart,
  onDragEnd,
}: VectorEditorProps) {
  const scrub = useScrubValue(value, onChange);
  const dragRef = useRef<{ axis: string; startX: number; startValue: number } | null>(null);

  function beginDrag(axis: string, event: React.PointerEvent<HTMLDivElement>): void {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const current = scrub.value[axis];
    dragRef.current = {
      axis,
      startX: event.clientX,
      startValue: Number.isFinite(current) ? current : 0,
    };
    scrub.begin();
    onDragStart?.();
  }

  function updateDrag(event: React.PointerEvent<HTMLDivElement>): void {
    const drag = dragRef.current;
    if (!drag) {
      return;
    }
    const delta = event.clientX - drag.startX;
    const next = drag.startValue + delta * step;
    scrub.set({ ...scrub.value, [drag.axis]: Number(next.toFixed(3)) });
  }

  function endDrag(): void {
    if (dragRef.current) {
      dragRef.current = null;
      scrub.end();
      onDragEnd?.();
    }
  }

  return (
    <div className="flex gap-1">
      {axes.map((axis, i) => (
        <div
          key={axis}
          className="flex min-w-0 flex-1 cursor-ew-resize items-center overflow-hidden rounded-sm border border-border bg-background"
          onPointerDown={(event) => beginDrag(axis, event)}
          onPointerMove={updateDrag}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          <span
            className={cn(
              "flex items-center self-stretch px-1 text-[10px] font-semibold select-none",
              coloredLabels
                ? (AXIS_COLORS[axis] ?? "bg-muted text-muted-foreground")
                : "bg-muted text-muted-foreground",
            )}
          >
            {labels?.[i] ?? axis.toUpperCase()}
          </span>
          <NumericInput
            value={scrub.value[axis] ?? 0}
            format={format}
            className="h-7 rounded-none border-0 bg-transparent px-1 py-0.5 font-mono text-[11px] shadow-none focus-visible:ring-0"
            onPointerDown={(event) => event.stopPropagation()}
            onCommit={(v) => scrub.set({ ...scrub.value, [axis]: v })}
          />
        </div>
      ))}
    </div>
  );
}
