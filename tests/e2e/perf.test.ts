// Perf-telemetry smoke test: enable the GPU profiler over the control plane, run a few
// frames, and assert the instrumentation stays honest — real GPU timings, a non-empty
// per-pass breakdown, and non-zero throughput counters. Under a software rasterizer
// (llvmpipe/lavapipe) the GPU numbers are CPU rasterization time, so magnitude assertions
// are relaxed when `softwareGpu` is set — the shape of the data is still checked.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { ProfilerModeResult, RenderPassTimingsDto, RenderStats } from "@saffron/protocol";


let engine: Engine;
let caps: ProfilerModeResult;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Need a drawn scene for the per-pass breakdown + throughput counters to be non-trivial.
  await engine.call("add-entity", { preset: "cube" });
  caps = await engine.call<ProfilerModeResult>("profiler.set-mode", { args: ["timestamps"] });
  // Let several frames record + read back (read-back lags by MaxFramesInFlight frames).
  await engine.settle(500);
});
afterAll(async () => {
  await engine?.shutdown();
});

test("set-mode reports a coherent capability set", () => {
  // The mode the device settled on never claims more than it supports.
  expect(["off", "timestamps", "pipeline-stats"]).toContain(caps.mode);
  if (!caps.timestampsSupported) {
    expect(caps.mode).toBe("off");
  } else {
    expect(caps.mode).not.toBe("off");
  }
});

test("render-stats reports throughput counters and the CPU/GPU split", async () => {
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.drawCalls).toBeGreaterThan(0);
  expect(stats.triangles).toBeGreaterThan(0);
  expect(stats.descriptorBinds).toBeGreaterThan(0);
  expect(stats.commandBuffers).toBeGreaterThan(0);
  expect(stats.queueSubmits).toBeGreaterThan(0);
  expect(stats.cpuFrameMs).toBeGreaterThan(0);
  if (caps.timestampsSupported) {
    expect(stats.profilerMode).not.toBe("off");
    expect(stats.gpuFrameMs).toBeGreaterThan(0);
  }
});

test("pass-timings returns a non-empty per-pass breakdown", async () => {
  const timings = await engine.call<RenderPassTimingsDto>("pass-timings");
  if (!caps.timestampsSupported) {
    return; // device cannot time passes; nothing to assert
  }
  expect(timings.passes.length).toBeGreaterThan(0);
  for (const pass of timings.passes) {
    expect(typeof pass.name).toBe("string");
    expect(pass.name.length).toBeGreaterThan(0);
    expect(pass.gpuMs).toBeGreaterThanOrEqual(0);
  }
  // The scene pass is always recorded once a draw is present.
  expect(timings.passes.some((p) => p.name === "scene")).toBe(true);
  expect(timings.gpuTotalMs).toBeGreaterThanOrEqual(0);
});

test("disabling the profiler returns to baseline", async () => {
  await engine.call("profiler.set-mode", { args: ["off"] });
  await engine.settle(200);
  const stats = await engine.call<RenderStats>("render-stats");
  expect(stats.profilerMode).toBe("off");
  expect(engine.validationErrors()).toEqual([]);
});
