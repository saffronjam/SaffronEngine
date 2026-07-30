/// Severity-routed, fatigue-aware alarm toasts: info stays in the log, a warning raises a throttled
/// toast, a critical a persistent one, and a RESOLVED event dismisses the toast its fingerprint
/// raised. The warning throttle is keyed per fingerprint and SURVIVES a resolve, so an alarm that
/// flaps cannot bypass it by cycling.
import { toast } from "sonner";
import type { AlarmEventDto } from "../protocol";

const WARNING_THROTTLE_MS = 10_000;

/// Active toast ids, for dismissal on RESOLVED (cleared when the alarm resolves).
const activeToasts = new Map<string, string | number>();
/// Last time a warning toast was raised per fingerprint — NOT cleared on resolve, so the
/// throttle holds across fire/resolve cycling. The dominant anti-spam guard.
const lastWarnedMs = new Map<string, number>();

/// A human-readable one-liner per alarm. `value`/`threshold` are ms for the time metrics
/// and percent for vram/burn-rate (the metric name says which).
function describe(event: AlarmEventDto): string {
  switch (event.metric) {
    case "frame-budget":
      return `Frame budget exceeded — ${event.value.toFixed(1)} ms (budget ${event.threshold.toFixed(1)} ms)`;
    case "frame-hitch":
      return `Frame hitch — ${event.value.toFixed(1)} ms spike`;
    case "burn-rate":
      return `Sustained over budget — ${event.value.toFixed(0)}% of recent frames`;
    case "vram":
      return `VRAM ${event.value.toFixed(0)}% of budget`;
    case "pso-compile":
      return `Pipeline compiled mid-frame (×${event.value.toFixed(0)})`;
    default:
      return `${event.metric}: ${event.value.toFixed(1)}`;
  }
}

/// Route a batch of drained events to toasts. `now` is a monotonic clock (performance.now).
export function routeAlarmToasts(events: AlarmEventDto[], now: number): void {
  for (const event of events) {
    if (event.state === "resolved") {
      const id = activeToasts.get(event.fingerprint);
      if (id !== undefined) {
        toast.dismiss(id);
        activeToasts.delete(event.fingerprint);
      }
      continue;
    }
    if (event.severity === "info") {
      continue; // log only — no interruption
    }
    const message = describe(event);
    if (event.severity === "critical") {
      const existing = activeToasts.get(event.fingerprint);
      if (existing !== undefined) {
        toast.dismiss(existing);
      }
      activeToasts.set(event.fingerprint, toast.error(message, { duration: Infinity }));
      continue;
    }
    // warning: one toast per fingerprint per window, regardless of resolve cycling
    const last = lastWarnedMs.get(event.fingerprint);
    if (last !== undefined && now - last < WARNING_THROTTLE_MS) {
      continue;
    }
    lastWarnedMs.set(event.fingerprint, now);
    activeToasts.set(event.fingerprint, toast.warning(message, { duration: 6000 }));
  }
}

/// Drop all tracked toasts (e.g. on engine restart) so stale ids never collide.
export function resetAlarmToasts(): void {
  for (const id of activeToasts.values()) {
    toast.dismiss(id);
  }
  activeToasts.clear();
  lastWarnedMs.clear();
}
