/// Render-frequency logger: components call `logRender("Name")` at the top of their render and
/// per-component counts flush to the console once a second while explicitly enabled. Counts include
/// renders React later discards, so the numbers are relative, not exact.

const counts = new Map<string, number>();
let flushTimer: number | null = null;

function renderLogEnabled(): boolean {
  return (
    import.meta.env.VITE_SAFFRON_RENDER_LOG === "1" ||
    localStorage.getItem("saffron.renderLog") === "1"
  );
}

export function logRender(name: string): void {
  if (!renderLogEnabled()) {
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
