/// Write-coalescer for high-frequency mutations (gizmo echo, scrub fields, sliders).
/// Buffers the latest pushed value, keeps at most ONE send in flight (completion
/// flushes whatever was buffered meanwhile), spaces send starts >= throttleMs apart,
/// and tracks sent/completed/in-flight counters around the async send.

export interface CoalescerStats {
  sent: number;
  completed: number;
  inFlight: number;
}

export interface Coalescer<T> {
  /// Record the latest value; sends it once the in-flight send (if any) completes
  /// and the throttle window has elapsed. Overwrites any prior pending value.
  push(value: T): void;
  /// Drop any buffered value and cancel the pending throttle timer, so nothing further
  /// is sent (an already in-flight send still completes). Use when a stream ends and a
  /// late send would be stale — e.g. a drag drop/leave that supersedes the preview.
  reset(): void;
  /// Counters for diagnostics (sent/completed since process start, current in-flight).
  stats(): CoalescerStats;
}

export interface CoalescerOptions<T> {
  /// Minimum milliseconds between two send starts (default 16, the pointer sample rate).
  throttleMs?: number;
  /// The async sink for the latest buffered value.
  send: (latest: T) => Promise<unknown>;
}

/// Minimum gap between two logged coalesced-write rejections, so a sustained
/// failure during a scrub stream produces at most one console.error per window
/// instead of one per dropped frame.
const ERROR_LOG_THROTTLE_MS = 2000;

export function makeCoalescer<T>(options: CoalescerOptions<T>): Coalescer<T> {
  const throttleMs = options.throttleMs ?? 16;
  const { send } = options;

  let pending: { value: T } | null = null;
  let lastSentAt = 0;
  let sent = 0;
  let completed = 0;
  let inFlight = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let lastErrorLoggedAt = 0;

  // A coalesced write is the tail of a scrub/drag stream, so a per-rejection toast
  // would spam. Drop the value (the next push supersedes it) but log at most once
  // per ERROR_LOG_THROTTLE_MS so a persistent failure is not fully silent.
  function logRejection(err: unknown): void {
    const now = performance.now();
    if (now - lastErrorLoggedAt < ERROR_LOG_THROTTLE_MS) {
      return;
    }
    lastErrorLoggedAt = now;
    // eslint-disable-next-line no-console
    console.error("coalesced write rejected:", err);
  }

  // Single-in-flight pump: at most one send is ever outstanding, the throttle is a
  // floor between send starts, and completion re-drives so the latest pushed value
  // is never dropped. The gate keeps per-key sends ordered: the `control`
  // command is async (concurrent invokes have no ordering guarantee).
  function maybeSend(): void {
    if (pending === null || inFlight) {
      return;
    }
    const now = performance.now();
    const elapsed = now - lastSentAt;
    if (elapsed < throttleMs) {
      if (timer === null) {
        timer = setTimeout(() => {
          timer = null;
          maybeSend();
        }, throttleMs - elapsed);
      }
      return;
    }
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    const buffered = pending;
    pending = null;
    lastSentAt = now;
    sent += 1;
    inFlight = true;
    void Promise.resolve(send(buffered.value))
      .catch(logRejection)
      .finally(() => {
        completed += 1;
        inFlight = false;
        maybeSend();
      });
  }

  return {
    push(value: T): void {
      pending = { value };
      maybeSend();
    },
    reset(): void {
      pending = null;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    },
    stats(): CoalescerStats {
      return { sent, completed, inFlight: inFlight ? 1 : 0 };
    },
  };
}
