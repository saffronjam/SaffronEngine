const OMITTED_OUTPUT = "[... earlier host output omitted ...]\n";
const HOST_LOG_HEADING = "\n\nhost log tail:\n";

/// A bounded, newest-data-first sink for child-process output.
export class BoundedTextLog {
  private value = "";
  private omitted = false;

  constructor(private readonly capacity: number) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new Error("bounded log capacity must be a positive integer");
    }
  }

  append(chunk: string): void {
    if (chunk.length >= this.capacity) {
      this.value = chunk.slice(-this.capacity);
      this.omitted = true;
      return;
    }
    const overflow = this.value.length + chunk.length - this.capacity;
    if (overflow > 0) {
      this.value = this.value.slice(overflow);
      this.omitted = true;
    }
    this.value += chunk;
  }

  tail(limit: number): string {
    if (!Number.isInteger(limit) || limit <= 0) {
      throw new Error("bounded log tail limit must be a positive integer");
    }
    const clipped = this.value.length > limit;
    const value = clipped ? this.value.slice(-limit) : this.value;
    return this.omitted || clipped ? `${OMITTED_OUTPUT}${value}` : value;
  }
}

/// Host-log lines that make a run untrustworthy however its commands answered: a lost device, or
/// the GPU hang watchdog naming a submission still in flight.
const HOST_FAULT_PATTERNS: readonly RegExp[] = [/ERROR_DEVICE_LOST/, /has been in flight/];

/// The most fault lines kept verbatim; a hang reports once a second, so the rest are counted only.
const MAX_KEPT_FAULTS = 16;

/// Collects the host's GPU-fault lines as they stream, independent of the bounded tail — a device
/// loss early in a long run must still be reported at the end of it.
export class HostFaultWatch {
  private partial = "";
  private readonly kept: string[] = [];
  private total = 0;

  append(chunk: string): void {
    const lines = (this.partial + chunk).split("\n");
    this.partial = lines.pop() ?? "";
    for (const line of lines) {
      this.scan(line);
    }
  }

  /// Scans whatever the last chunk left without a newline. Call once the stream ends.
  finish(): void {
    if (this.partial) {
      this.scan(this.partial);
      this.partial = "";
    }
  }

  /// One entry per retained fault line, plus a tail entry when more were seen than retained.
  report(): string[] {
    const report = [...this.kept];
    if (this.total > this.kept.length) {
      report.push(`${this.total - this.kept.length} further GPU fault line(s) omitted`);
    }
    return report;
  }

  private scan(line: string): void {
    if (!HOST_FAULT_PATTERNS.some((pattern) => pattern.test(line))) {
      return;
    }
    this.total += 1;
    if (this.kept.length < MAX_KEPT_FAULTS) {
      this.kept.push(line.trim());
    }
  }
}

/// Continuously drains one child-process stream into a bounded log, scanning it for GPU faults.
export async function drainHostStream(
  stream: ReadableStream<Uint8Array>,
  log: BoundedTextLog,
  faults?: HostFaultWatch,
): Promise<void> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      const text = decoder.decode(value, { stream: true });
      log.append(text);
      faults?.append(text);
    }
    const tail = decoder.decode();
    log.append(tail);
    faults?.append(tail);
    faults?.finish();
  } catch (error) {
    log.append(`\n[host log drain failed: ${String(error)}]\n`);
  } finally {
    reader.releaseLock();
  }
}

/// Adds the retained host-output tail to a failure without duplicating an existing attachment.
export function withHostLog(message: string, log: BoundedTextLog, tailLimit: number): string {
  if (message.includes(HOST_LOG_HEADING)) {
    return message;
  }
  const tail = log.tail(tailLimit);
  return `${message}${HOST_LOG_HEADING}${tail || "<no host output captured>"}`;
}
