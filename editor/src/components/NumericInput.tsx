/// A free-typing numeric input. While editing it holds the raw text as a draft — a partial value
/// like "-" or "1." is allowed — and parses nothing until blur or Enter, committing only a finite,
/// clamped number; empty or non-numeric input reverts and emits nothing, so erasing a field to
/// retype it never sends 0 to the engine. No focus handler runs, so clicking in just places the
/// caret where clicked.
import { useRef, useState } from "react";
import { Input } from "@/components/ui/input";

export function formatNumber(value: number): string {
  if (!Number.isFinite(value)) {
    return "0";
  }
  return Number(value.toFixed(3)).toString();
}

export function clampNumber(value: number, min?: number, max?: number): number {
  let v = value;
  if (min !== undefined && v < min) {
    v = min;
  }
  if (max !== undefined && v > max) {
    v = max;
  }
  return v;
}

export interface NumericInputProps {
  value: number;
  min?: number;
  max?: number;
  /// Readout formatter when idle (default `formatNumber`); display only, the commit
  /// keeps full precision.
  format?: (n: number) => string;
  className?: string;
  /// Commit a finite, clamped value. Never called for empty or non-numeric input.
  onCommit(value: number): void;
  /// Swallow the pointer so a parent drag-scrub handle doesn't start a scrub on focus.
  onPointerDown?(event: React.PointerEvent<HTMLInputElement>): void;
}

export function NumericInput({
  value,
  min,
  max,
  format = formatNumber,
  className,
  onCommit,
  onPointerDown,
}: NumericInputProps) {
  const [draft, setDraft] = useState<string | null>(null);
  const cancel = useRef(false);

  const handleBlur = (event: React.FocusEvent<HTMLInputElement>): void => {
    const text = event.currentTarget.value.trim();
    setDraft(null);
    if (cancel.current) {
      cancel.current = false;
      return;
    }
    const parsed = Number(text);
    if (text === "" || !Number.isFinite(parsed)) {
      return;
    }
    const clamped = clampNumber(parsed, min, max);
    if (clamped !== value) {
      onCommit(clamped);
    }
  };

  return (
    <Input
      type="text"
      inputMode="decimal"
      value={draft ?? format(value)}
      className={className}
      onPointerDown={onPointerDown}
      onChange={(event) => setDraft(event.currentTarget.value)}
      onBlur={handleBlur}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.currentTarget.blur();
        } else if (event.key === "Escape") {
          cancel.current = true;
          event.currentTarget.blur();
        }
      }}
    />
  );
}
