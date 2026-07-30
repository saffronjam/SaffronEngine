/// A single-axis drag-scrub number field: pointer capture on the wrapper scrubs by `clientX` delta ×
/// step, while the `NumericInput` swallows its own pointer so typing never starts a scrub. Renders
/// drag-local state so the readout never waits on the wire; the panel owns coalescing and
/// drag-gating.
import { useRef } from "react";
import { cn } from "@/lib/utils";
import { useScrubValue } from "@/lib/useScrubValue";
import { NumericInput, clampNumber } from "./NumericInput";

export interface NumberDragProps {
  value: number;
  step?: number;
  min?: number;
  max?: number;
  /// When true, draw a slider fill behind the value (used for `slider` fields).
  track?: boolean;
  onChange(value: number): void;
  /// Bracket a scrub gesture so the panel can gate the reconcile poll off.
  onDragStart?(): void;
  onDragEnd?(): void;
}

export function NumberDrag({
  value,
  step = 0.05,
  min,
  max,
  track = false,
  onChange,
  onDragStart,
  onDragEnd,
}: NumberDragProps) {
  const scrub = useScrubValue(value, onChange);
  const dragRef = useRef<{ startX: number; startValue: number } | null>(null);

  function beginDrag(event: React.PointerEvent<HTMLDivElement>): void {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      startX: event.clientX,
      startValue: Number.isFinite(scrub.value) ? scrub.value : 0,
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
    const next = clampNumber(drag.startValue + delta * step, min, max);
    scrub.set(Number(next.toFixed(3)));
  }

  function endDrag(): void {
    if (dragRef.current) {
      dragRef.current = null;
      scrub.end();
      onDragEnd?.();
    }
  }

  const fill =
    track && min !== undefined && max !== undefined && max > min
      ? ((clampNumber(scrub.value, min, max) - min) / (max - min)) * 100
      : null;

  return (
    <div
      className="relative cursor-ew-resize overflow-hidden rounded-sm"
      onPointerDown={beginDrag}
      onPointerMove={updateDrag}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    >
      {fill !== null ? (
        <div
          className="pointer-events-none absolute inset-y-0 left-0 bg-primary/25"
          style={{ width: `${fill}%` }}
        />
      ) : null}
      <NumericInput
        value={scrub.value}
        min={min}
        max={max}
        className={cn(
          "relative h-7 rounded-sm bg-background px-1.5 py-0.5 font-mono text-[11px]",
          track && "bg-transparent",
        )}
        onPointerDown={(event) => event.stopPropagation()}
        onCommit={(v) => scrub.set(v)}
      />
    </div>
  );
}
