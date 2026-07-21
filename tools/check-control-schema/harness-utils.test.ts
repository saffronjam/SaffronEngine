import { expect, test } from "bun:test";
import { BoundedTextLog, drainHostStream, withHostLog } from "./harness-utils.ts";

test("bounded log retains only the newest output", () => {
  const log = new BoundedTextLog(8);
  log.append("abc");
  log.append("defghijk");

  expect(log.tail(8)).toBe("[... earlier host output omitted ...]\ndefghijk");
  expect(log.tail(4)).toBe("[... earlier host output omitted ...]\nhijk");
});

test("stream drain consumes every chunk into the same bounded log", async () => {
  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encoder.encode("first\n"));
      controller.enqueue(encoder.encode("second\n"));
      controller.close();
    },
  });
  const log = new BoundedTextLog(64);

  await drainHostStream(stream, log);

  expect(log.tail(64)).toBe("first\nsecond\n");
});

test("failure formatting attaches one useful host tail", () => {
  const log = new BoundedTextLog(64);
  log.append("renderer stalled\n");
  const formatted = withHostLog("timeout calling ping", log, 64);

  expect(formatted).toContain("timeout calling ping\n\nhost log tail:\nrenderer stalled");
  expect(withHostLog(formatted, log, 64)).toBe(formatted);
});
