/// Drag-local widget value with rAF-coalesced emit: the widget renders its own state the instant the
/// pointer moves, never waiting on the store or wire round trip, while changes flow outward at most
/// once per animation frame. Outside a gesture the local value mirrors the prop, so external edits
/// still flow in. Every scrub widget shares this one ownership model.
import { useEffect, useRef, useState } from "react";

export interface ScrubValue<T> {
  /// What the widget renders: local-first during a gesture, prop-following idle.
  value: T;
  /// Update from the widget: local state now, one emit on the next frame.
  set(next: T): void;
  /// Open a gesture — the prop stops overwriting the local value.
  begin(): void;
  /// Close it, flushing any pending emit synchronously. Call BEFORE the panel's
  /// onDragEnd so the release re-push reads the final value, not a frame-old one.
  end(): void;
}

export function useScrubValue<T>(prop: T, emit: (value: T) => void): ScrubValue<T> {
  const [local, setLocal] = useState(prop);
  const dragging = useRef(false);
  const pending = useRef<{ value: T } | null>(null);
  const raf = useRef<number | null>(null);
  const emitRef = useRef(emit);
  emitRef.current = emit;

  useEffect(() => {
    if (!dragging.current) {
      setLocal(prop);
    }
  }, [prop]);

  const flushRef = useRef(() => {});
  flushRef.current = (): void => {
    if (raf.current !== null) {
      cancelAnimationFrame(raf.current);
      raf.current = null;
    }
    const buffered = pending.current;
    pending.current = null;
    if (buffered) {
      emitRef.current(buffered.value);
    }
  };

  // Unmount mid-gesture: deliver the last value instead of dropping it.
  useEffect(() => () => flushRef.current(), []);

  return {
    value: local,
    set(next: T): void {
      setLocal(next);
      pending.current = { value: next };
      if (raf.current === null) {
        raf.current = requestAnimationFrame(() => {
          raf.current = null;
          flushRef.current();
        });
      }
    },
    begin(): void {
      dragging.current = true;
    },
    end(): void {
      flushRef.current();
      dragging.current = false;
    },
  };
}
