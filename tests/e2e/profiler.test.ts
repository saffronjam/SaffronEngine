// Profiler capture-contract test: arm a capture over the control plane, run frames, drain it,
// and assert the returned ProfileCaptureDto is a well-formed merged CPU+GPU timeline with a
// nested span tree and a parseable Chrome-Trace. Under a software rasterizer (llvmpipe/lavapipe)
// the GPU numbers are CPU rasterization time, so magnitude assertions are relaxed when
// `softwareGpu` is set — the shape of the data is still checked, and the validation log stays clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type {
  CaptureStartResult,
  CaptureStatusResult,
  CaptureStopResult,
  ProfileSpanDto,
} from "@saffron/protocol";

let engine: Engine;
let result: CaptureStopResult;

/// Arm a single-frame capture, poll the non-destructive status until ready, then drain.
async function captureSingle(): Promise<CaptureStopResult> {
  await engine.call<CaptureStartResult>("profiler.capture-start", { mode: "single" });
  for (let i = 0; i < 60; i++) {
    const status = await engine.call<CaptureStatusResult>("profiler.capture-status");
    if (status.state === "ready") {
      break;
    }
    await engine.settle(60);
  }
  return engine.call<CaptureStopResult>("profiler.capture-stop");
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.settle(300);
  result = await captureSingle();
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a single capture is ready and self-documenting", () => {
  expect(result.ready).toBe(true);
  expect(result.mode).toBe("single");
  expect(result.frameCount).toBeGreaterThanOrEqual(1);
  const meta = result.capture.metadata;
  // The honesty flags are always present so a downloaded capture is interpretable on its own.
  expect(typeof meta.softwareGpu).toBe("boolean");
  expect(typeof meta.correlated).toBe("boolean");
  expect(meta.deviceName.length).toBeGreaterThan(0);
  expect(meta.timestampPeriod).toBeGreaterThan(0);
});

test("the capture carries both a CPU lane and a GPU lane", () => {
  const spans = result.capture.spans;
  expect(spans.length).toBeGreaterThan(0);
  expect(spans.some((s) => s.lane === "cpu")).toBe(true);
  expect(spans.some((s) => s.lane === "gpu")).toBe(true);
  // The CPU lifecycle phases and the GPU passes the renderer always records.
  expect(spans.some((s) => s.lane === "cpu" && s.name === "execute-render-graph")).toBe(true);
  expect(spans.some((s) => s.lane === "gpu" && s.name === "scene")).toBe(true);
});

test("the span tree has valid depths and parents", () => {
  const spans = result.capture.spans;
  const softwareGpu = result.capture.metadata.softwareGpu;
  spans.forEach((span: ProfileSpanDto, index) => {
    expect(span.depth).toBeGreaterThanOrEqual(0);
    expect(span.endNs).toBeGreaterThanOrEqual(span.startNs);
    if (span.parentIndex >= 0) {
      // A parent must exist, share the lane, sit one level up, and contain the child in time.
      expect(span.parentIndex).toBeLessThan(spans.length);
      expect(span.parentIndex).not.toBe(index);
      const parent = spans[span.parentIndex];
      expect(parent.lane).toBe(span.lane);
      expect(span.depth).toBe(parent.depth + 1);
      if (!softwareGpu || span.lane === "cpu") {
        expect(span.startNs).toBeGreaterThanOrEqual(parent.startNs);
        expect(span.endNs).toBeLessThanOrEqual(parent.endNs);
      }
    } else {
      expect(span.depth).toBe(0);
    }
  });
  // At least one nested span exists (CPU passes under execute-render-graph).
  expect(spans.some((s) => s.parentIndex >= 0)).toBe(true);
});

test("the inline Chrome-Trace parses into well-formed X/M events", () => {
  expect(result.chromeTrace.length).toBeGreaterThan(0);
  const trace = JSON.parse(result.chromeTrace) as {
    traceEvents: { ph: string; tid?: number; name?: string; ts?: number; dur?: number }[];
    displayTimeUnit: string;
    otherData: Record<string, unknown>;
  };
  expect(trace.displayTimeUnit).toBe("ns");
  const meta = trace.traceEvents.filter((e) => e.ph === "M");
  const complete = trace.traceEvents.filter((e) => e.ph === "X");
  expect(meta.length).toBeGreaterThanOrEqual(3); // process + 2 thread names
  expect(complete.length).toBe(result.capture.spans.length);
  for (const e of complete) {
    expect(typeof e.ts).toBe("number");
    expect(typeof e.dur).toBe("number");
    expect(e.dur).toBeGreaterThanOrEqual(0);
    expect(typeof e.name).toBe("string");
  }
  // The honesty flags ride into the trace's otherData too.
  expect(trace.otherData).toHaveProperty("softwareGpu");
  expect(trace.otherData).toHaveProperty("correlated");
});

test("a frames:N capture writes a file and still returns inline spans", async () => {
  await engine.call<CaptureStartResult>("profiler.capture-start", { mode: "frames", frames: 4 });
  for (let i = 0; i < 80; i++) {
    const status = await engine.call<CaptureStatusResult>("profiler.capture-status");
    if (status.state === "ready") {
      break;
    }
    await engine.settle(60);
  }
  const frames = await engine.call<CaptureStopResult>("profiler.capture-stop");
  expect(frames.ready).toBe(true);
  expect(frames.mode).toBe("frames");
  expect(frames.frameCount).toBeGreaterThan(1);
  expect(frames.path.length).toBeGreaterThan(0); // written to a file for the viewer / sa
  expect(frames.capture.spans.length).toBeGreaterThan(result.capture.spans.length);
});

test("capturing leaves the validation log clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
