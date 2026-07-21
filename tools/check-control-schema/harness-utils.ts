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

/// Continuously drains one child-process stream into a bounded log.
export async function drainHostStream(
  stream: ReadableStream<Uint8Array>,
  log: BoundedTextLog,
): Promise<void> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      log.append(decoder.decode(value, { stream: true }));
    }
    log.append(decoder.decode());
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
