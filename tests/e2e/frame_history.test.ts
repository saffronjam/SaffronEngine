// Frame-history & quality-metrics smoke test (Phase 2). The engine records every frame
// into a ring (independent of the profiler mode) and computes percentile/stutter metrics
// on demand; a single shared budget/threshold config drives green/amber/red everywhere.
// This asserts the percentile summary is well-formed and that the budget tracks targetFps.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { FrameHistoryDto, PerfConfigDto, SetUpscaleResult } from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Let the ring fill with enough frames for percentiles to be meaningful.
  await engine.settle(600);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("frame-history reports ordered percentiles over recorded frames", async () => {
  const h = await engine.call<FrameHistoryDto>("frame-history");
  expect(h.sampleCount).toBeGreaterThan(0);
  // Percentiles come from a sorted distribution, so they are monotone non-decreasing.
  expect(h.p50Ms).toBeLessThanOrEqual(h.p95Ms);
  expect(h.p95Ms).toBeLessThanOrEqual(h.p99Ms);
  expect(h.p99Ms).toBeLessThanOrEqual(h.p999Ms);
  expect(h.p999Ms).toBeLessThanOrEqual(h.maxMs);
  expect(h.p50Ms).toBeGreaterThan(0);
  expect(h.stddevMs).toBeGreaterThanOrEqual(0);
  expect(Number.isInteger(h.stutterCount)).toBe(true);
  expect(h.stutterCount).toBeGreaterThanOrEqual(0);
});

test("frame-history returns the requested recent raw samples", async () => {
  const h = await engine.call<FrameHistoryDto>("frame-history", { samples: 32 });
  expect(Array.isArray(h.samples)).toBe(true);
  expect(h.samples.length).toBeGreaterThan(0);
  expect(h.samples.length).toBeLessThanOrEqual(32);
  let prevIndex = -1;
  for (const s of h.samples) {
    expect(typeof s.cpuMs).toBe("number");
    expect(s.cpuMs).toBeGreaterThanOrEqual(0);
    expect(s.cpuWaitMs).toBeGreaterThanOrEqual(0);
    // The absolute frame index is monotonic — the editor dedups overlapping windows on it.
    expect(s.frameIndex).toBeGreaterThan(prevIndex);
    prevIndex = s.frameIndex;
  }
  // No samples requested ⇒ summary only, empty array.
  const summary = await engine.call<FrameHistoryDto>("frame-history");
  expect(summary.samples.length).toBe(0);
});

test("upscale: targetMs drives the derived budget and round-trips", async () => {
  const at60 = await engine.call<PerfConfigDto>("get-perf-config");
  expect(at60.targetFps).toBe(60);
  expect(at60.budgetMs).toBeCloseTo(1000 / 60, 2);

  // The frame budget is set through set-upscale's targetMs (the render-scale surface); the
  // get-perf-config read stays as budget telemetry.
  const at30 = await engine.call<SetUpscaleResult>("set-upscale", { targetMs: 1000 / 30 });
  expect(at30.upscale.targetMs).toBeCloseTo(1000 / 30, 2);

  // The change is observable on the frame-history budget too (one shared source of truth).
  const h = await engine.call<FrameHistoryDto>("frame-history");
  expect(h.budgetMs).toBeCloseTo(1000 / 30, 2);

  // And get-perf-config's read-only telemetry reflects it.
  const cfg = await engine.call<PerfConfigDto>("get-perf-config");
  expect(cfg.targetFps).toBeCloseTo(30, 1);

  await engine.call("set-upscale", { targetMs: 1000 / 60 });
  expect(engine.validationErrors()).toEqual([]);
});
