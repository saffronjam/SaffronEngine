/// Dev-mode overlay open→paint timer. A shared overlay primitive (Select, DropdownMenu, …)
/// given a `perfLabel` calls `measureOverlayOpen(label)` on its opening edge; this stamps the
/// time and logs the elapsed milliseconds on the second animation frame (a first-paint proxy):
///
///   [overlay-perf] resolution open→paint 312.0ms
///
/// Reads the store via getState() so it never subscribes (must add no re-render of its own), and
/// is a no-op while dev mode is off (the titlebar chip / VITE_SAFFRON_DEV_MODE). Two rAFs: the
/// first fires after the open commit is scheduled, the second after the browser has laid out and
/// is about to paint — so the delta captures style-recalc + layout, the cost this measures.
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
