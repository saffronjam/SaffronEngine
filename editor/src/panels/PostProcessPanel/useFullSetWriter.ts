import { useRef } from "react";

/// One command over a flat parameter set: each gesture reads the live state, writes the full set,
/// and folds the echoed result back. A scrub captures the prior at drag start and records once at
/// drag end; a discrete edit records inline. Shared by the bloom and color-grade write paths.
export function useFullSetWriter<T>({
  label,
  from,
  equal,
  apply,
  send,
  record,
  setDragActive,
}: {
  label: string;
  from: (patch: Partial<T>) => T;
  equal: (a: T, b: T) => boolean;
  apply: (value: T) => void;
  send: (value: T) => Promise<unknown>;
  record: (label: string, undo: () => Promise<unknown>, redo: () => Promise<unknown>) => void;
  setDragActive: (active: boolean) => void;
}): {
  write(patch: Partial<T>): void;
  onDragStart(): void;
  onDragEnd(): void;
  /// Drop the in-flight gesture without recording it, for a caller that records the whole
  /// gesture itself.
  cancelGesture(): void;
} {
  const prior = useRef<T | null>(null);
  const recordDelta = (before: T, after: T): void => {
    if (!equal(before, after)) {
      record(
        label,
        () => send(before),
        () => send(after),
      );
    }
  };
  return {
    write(patch) {
      const next = from(patch);
      if (prior.current === null) {
        recordDelta(from({}), next);
      }
      apply(next);
    },
    onDragStart() {
      prior.current = from({});
      setDragActive(true);
    },
    onDragEnd() {
      setDragActive(false);
      const before = prior.current;
      prior.current = null;
      if (before) {
        recordDelta(before, from({}));
      }
    },
    cancelGesture() {
      prior.current = null;
    },
  };
}
