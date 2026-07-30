/// Dev-mode render-frequency logger: components call `logRender("Name")` at the top of their render
/// and per-component counts flush to the console once a second while dev mode is on. Reads the store
/// through `getState()` so the logger never subscribes and adds no re-renders of its own. Counts
/// include renders React later discards, so the numbers are relative, not exact.
import { useEditorStore } from "../state/store";

const counts = new Map<string, number>();
let flushTimer: number | null = null;

export function logRender(name: string): void {
  if (!useEditorStore.getState().devMode) {
    return;
  }
  counts.set(name, (counts.get(name) ?? 0) + 1);
  if (flushTimer !== null) {
    return;
  }
  flushTimer = window.setTimeout(() => {
    flushTimer = null;
    const rows = [...counts.entries()].sort((a, b) => b[1] - a[1]);
    counts.clear();
    console.info(`[renders/s] ${rows.map(([name_, count]) => `${name_}×${count}`).join("  ")}`);
  }, 1000);
}
