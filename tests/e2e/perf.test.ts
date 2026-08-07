// Perf-telemetry smoke test: enable the GPU profiler over the control plane, run a few
// frames, and assert the instrumentation stays honest — real GPU timings, a non-empty
// per-pass breakdown, and non-zero throughput counters. Under a software rasterizer
// (llvmpipe/lavapipe) the GPU numbers are CPU rasterization time, so magnitude assertions
// are relaxed when `softwareGpu` is set — the shape of the data is still checked.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type {
  CaptureStopResult,
  ProfileSpanDto,
  ProfilerModeResult,
  RenderStats,
} from "@saffron/protocol";

let engine: Engine;
let caps: ProfilerModeResult;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Need a drawn scene for the per-pass breakdown + throughput counters to be non-trivial.
  await engine.call("add-entity", { preset: "cube" });
  caps = await engine.call("profiler.set-mode", { args: ["timestamps"] });
  // Let the frame telemetry warm up: the project-load reset withholds the smoothed
  // CPU headline for its warm-up frames, so poll (each query is itself a redraw
  // signal) until the headline is live instead of guessing a wall-clock delay.
  for (let i = 0; i < 40; i++) {
    const stats = await engine.call("render-stats");
    if (stats.cpuFrameMs > 0) break;
    await engine.settle(250);
  }
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
  const stats = await engine.call("render-stats");
  expect(stats.drawCalls).toBeGreaterThan(0);
  expect(stats.triangles).toBeGreaterThan(0);
  expect(stats.sceneGatherMs).toBeGreaterThanOrEqual(0);
  expect(stats.retainedMeshCpuBytes).toBeGreaterThan(0);
  expect(stats.shadowDrawCalls).toBeGreaterThanOrEqual(0);
  expect(stats.rtInstances).toBeGreaterThanOrEqual(0);
  expect(stats.descriptorBinds).toBeGreaterThan(0);
  expect(stats.commandBuffers).toBeGreaterThan(0);
  expect(stats.queueSubmits).toBeGreaterThan(0);
  expect(stats.cpuFrameMs).toBeGreaterThan(0);
  if (caps.timestampsSupported) {
    expect(stats.profilerMode).not.toBe("off");
    expect(stats.gpuFrameMs).toBeGreaterThan(0);
  }
});

// Device-local memory is the one telemetry axis with no device gate: with VK_EXT_memory_budget the
// driver reports it, and without it VMA reports its own block totals against a fraction of the heap
// sizes. Either way a running renderer occupies memory and has headroom, so a zero here means the
// figures are not being sampled at all — which is what made every recorded baseline read `0`.
test("render-stats reports device-local memory against the driver's budget", async () => {
  const stats = await engine.call("render-stats");
  expect(stats.vramBudgetBytes).toBeGreaterThan(0);
  expect(stats.vramUsageBytes).toBeGreaterThan(0);
  expect(stats.vramUsageBytes).toBeLessThan(stats.vramBudgetBytes);
});

// Render preparation scales with what the frame CHANGED, not with what the scene holds. The claim
// is only worth asserting comparatively: a bound on one scene's counter passes for an
// implementation that walks every instance, whereas a small scene and a much larger one reporting
// the same steady-frame cost cannot.
test("steady-frame preparation cost does not grow with the scene", async () => {
  // Polls on convergence ALONE, so the preparation-cost claim below is the assertion's and not
  // the poll's: a helper that waited for `sceneGatherEntities === 0` would spin until the
  // property it is meant to test happened to hold. A mutation re-cuts for a frame or two, so a
  // sample taken mid-settle measures the change rather than the steady state.
  async function settled(): Promise<RenderStats> {
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const stats = await engine.call("render-stats");
      if (stats.converged) {
        return stats;
      }
      await engine.settle(100);
    }
    return engine.call("render-stats");
  }

  const small = await settled();
  expect(small.sceneGatherEntities).toBe(0);
  const smallUpload = small.instanceUploadBytes;

  const added: string[] = [];
  for (let i = 0; i < 64; i += 1) {
    const entity = await engine.call("add-entity", { preset: "cube" });
    added.push(entity.id);
    await engine.call("set-transform", {
      entity: entity.id,
      translation: { x: i * 2 - 64, y: 0, z: -6 },
    });
  }
  const large = await settled();
  expect(large.instances).toBeGreaterThan(small.instances);
  // The whole property: 65 instances cost the same steady frame as one.
  expect(large.sceneGatherEntities).toBe(0);
  expect(large.instanceUploadBytes).toBe(smallUpload);

  for (const entity of added) {
    await engine.call("destroy-entity", { entity });
  }
  await settled();
  expect(engine.validationErrors()).toEqual([]);
});

test("pass-timings returns a non-empty per-pass breakdown", async () => {
  const timings = await engine.call("pass-timings");
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
  const stats = await engine.call("render-stats");
  expect(stats.profilerMode).toBe("off");
  expect(engine.validationErrors()).toEqual([]);
});

// Profiler capture contract: arm a capture over the control plane, run frames, drain it, and assert
// the returned ProfileCaptureDto is a well-formed merged CPU+GPU timeline with a nested span tree
// and a parseable Chrome-Trace. Under a software rasterizer (llvmpipe/lavapipe) the GPU numbers are
// CPU rasterization time, so magnitude assertions are relaxed when `softwareGpu` is set — the shape
// of the data is still checked, and the validation log stays clean.
describe("profiler capture", () => {
  let result: CaptureStopResult;

  // Arm a single-frame capture, poll the non-destructive status until ready, then drain.
  async function captureSingle(): Promise<CaptureStopResult> {
    await engine.call("profiler.capture-start", { mode: "single" });
    for (let i = 0; i < 60; i++) {
      const status = await engine.call("profiler.capture-status");
      if (status.state === "ready") {
        break;
      }
      await engine.settle(60);
    }
    return engine.call("profiler.capture-stop");
  }

  beforeAll(async () => {
    result = await captureSingle();
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
    await engine.call("profiler.capture-start", { mode: "frames", frames: 4 });
    for (let i = 0; i < 80; i++) {
      const status = await engine.call("profiler.capture-status");
      if (status.state === "ready") {
        break;
      }
      await engine.settle(60);
    }
    const frames = await engine.call("profiler.capture-stop");
    expect(frames.ready).toBe(true);
    expect(frames.mode).toBe("frames");
    expect(frames.frameCount).toBeGreaterThan(1);
    expect(frames.path.length).toBeGreaterThan(0); // written to a file for the viewer / sa
    expect(frames.capture.spans.length).toBeGreaterThan(result.capture.spans.length);
  });

  test("capturing leaves the validation log clean", () => {
    expect(engine.validationErrors()).toEqual([]);
  });
});
