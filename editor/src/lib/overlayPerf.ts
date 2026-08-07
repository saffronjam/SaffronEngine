/// Dev-mode overlay open→paint timer: a shared overlay primitive given a `perfLabel` stamps its
/// opening edge and logs the elapsed milliseconds on the second animation frame. Two rAFs, because
/// the first fires after the open commit is scheduled and the second after layout, just before
/// paint — so the delta captures the style-recalc and layout this is meant to measure. Reads the
/// store through `getState()` so it never subscribes, and is a no-op while dev mode is off.
import { useEditorStore } from "../state/store";

export function measureOverlayOpen(label: string): void {
  if (!useEditorStore.getState().devMode) {
    return;
  }
  const t0 = performance.now();
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      console.info(`[overlay-perf] ${label} open→paint ${(performance.now() - t0).toFixed(1)}ms`);
    });
  });
}

/// Compose a Radix Root's `onOpenChange` with the timer: when `label` is set, the opening edge is
/// timed before delegating to the caller's handler. Returns the original handler untouched when no
/// label is given, so shipped call sites (which pass none) are unaffected.
export function withOverlayPerf(
  label: string | undefined,
  onOpenChange: ((open: boolean) => void) | undefined,
): ((open: boolean) => void) | undefined {
  if (!label) {
    return onOpenChange;
  }
  return (open: boolean) => {
    if (open) {
      measureOverlayOpen(label);
    }
    onOpenChange?.(open);
  };
}
