import { expect, test } from "bun:test";
import { BoundedTextLog, HostFaultWatch, drainHostStream, withHostLog } from "./harness-utils.ts";

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

test("the fault watch keeps device-loss and hang lines whole across chunk splits", async () => {
  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(
        encoder.encode("frame 12 ok\nERROR vulkan: wait_for_fences (begin) -> ERR"),
      );
      controller.enqueue(encoder.encode("OR_DEVICE_LOST\nGPU frame ring slot 0 has been in "));
      controller.enqueue(
        encoder.encode(
          "flight 3s — frame 167 is wedged in render-graph batch 6/14 'gbuffer…ssgi', whose " +
            "timeline point 41 has not signalled (counter 40)\ntrailing without newline",
        ),
      );
      controller.close();
    },
  });
  const faults = new HostFaultWatch();

  await drainHostStream(stream, new BoundedTextLog(1024), faults);

  expect(faults.report()).toEqual([
    "ERROR vulkan: wait_for_fences (begin) -> ERROR_DEVICE_LOST",
    "GPU frame ring slot 0 has been in flight 3s — frame 167 is wedged in render-graph batch " +
      "6/14 'gbuffer…ssgi', whose timeline point 41 has not signalled (counter 40)",
  ]);
});

test("the fault watch stays silent on a clean host log", async () => {
  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encoder.encode("vulkan ready — gpu 'llvmpipe' (cpu)\nframe 1 ok\n"));
      controller.close();
    },
  });
  const faults = new HostFaultWatch();

  await drainHostStream(stream, new BoundedTextLog(1024), faults);

  expect(faults.report()).toEqual([]);
});
