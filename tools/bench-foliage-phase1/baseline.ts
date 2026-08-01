// The Phase 1 foliage baseline measurement: one deterministic fixture, one timestamp capture, one
// record. `measure.ts` writes the record to `benchmarks/foliage-veg/`; `check.ts` re-measures and
// holds a fresh run to the record captured on the same device. Both go through this module, so a
// recorded ceiling and the number graded against it can never come from two different fixtures.

import { spawnSync } from "node:child_process";
import { arch, cpus, platform, release } from "node:os";
import { Engine } from "../../tests/e2e/harness.ts";
import type { RenderStatsDto } from "../../editor/src/protocol/sa-types.ts";

const INSTANCE_COUNT = Number(process.env.SAFFRON_BASELINE_INSTANCES ?? 512);
const CAPTURE_FRAMES = Number(process.env.SAFFRON_BASELINE_FRAMES ?? 32);
const PRESETS = ["cube", "plane", "sphere"] as const;

const BUDGET_DERIVATION =
  "Anima steady-state p95 plus 25% or an absolute noise floor, whichever is larger";

export interface Distribution {
  p50: number;
  p95: number;
  p99: number;
  max: number;
}

export interface BaselineFixture {
  name: string;
  primitiveInstances: number;
  primitiveCycle: readonly string[];
  pointLights: number;
  spotLights: number;
  starterDirectionalLights: number;
  captureFrames: number;
  viewport: readonly number[];
}

export interface BaselinePlatform {
  os: string;
  osRelease: string;
  architecture: string;
  cpu: string;
  rustc: string;
  revision: string;
  workingTreeDirty: boolean;
  gpu: string;
  softwareGpu: boolean;
  rtSupported: boolean;
  profilerMode: string;
  timestampsSupported: boolean;
}

export interface BaselineObserved {
  sceneGatherMs: Distribution;
  cpuFrameMs: Distribution;
  gpuFrameMs: Distribution;
  drawCalls: number;
  batches: number;
  instances: number;
  triangles: number;
  instanceUploadBytes: number;
  shadowDrawCalls: number;
  rtInstances: number;
  retainedMeshCpuBytes: number;
  vramUsageBytes: number;
  vramBudgetBytes: number;
}

export interface BaselineBudgets {
  derivation: string;
  sceneGatherP95Ms: number;
  cpuFrameP95Ms: number;
  gpuFrameP95Ms: number;
  drawCallsMax: number;
  instanceUploadBytesMax: number;
  retainedMeshCpuBytesMax: number;
}

export interface BaselineRecord {
  schemaVersion: number;
  recordedAt: string;
  fixture: BaselineFixture;
  platform: BaselinePlatform;
  observed: BaselineObserved;
  budgets: BaselineBudgets;
  validationErrors: string[];
}

export interface MeasureOptions {
  // Called with the booted device's stats before the fixture is built. Throwing here abandons the
  // run without paying for it, which is how a caller rejects a device it will not grade.
  precheck?: (stats: RenderStatsDto) => void;
}

function command(program: string, args: string[]): string {
  const result = spawnSync(program, args, { encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : "unavailable";
}

function percentile(values: number[], fraction: number): number {
  if (values.length === 0) return 0;
  const sorted = values.toSorted((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)];
}

function distribution(values: number[]): Distribution {
  return {
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
    max: values.length === 0 ? 0 : Math.max(...values),
  };
}

// The one derivation behind every recorded ceiling: a quarter above the steady-state p95, or an
// absolute noise floor above it, whichever is larger. Quantized to 3 decimals so the record and a
// re-derivation of it compare exactly.
function ceiling(value: number, absoluteHeadroom: number): number {
  return Number(Math.max(value * 1.25, value + absoluteHeadroom).toFixed(3));
}

function deriveBudgets(observed: BaselineObserved): BaselineBudgets {
  return {
    derivation: BUDGET_DERIVATION,
    sceneGatherP95Ms: ceiling(observed.sceneGatherMs.p95, 0.05),
    cpuFrameP95Ms: ceiling(observed.cpuFrameMs.p95, 0.25),
    gpuFrameP95Ms: ceiling(observed.gpuFrameMs.p95, 0.25),
    drawCallsMax: observed.drawCalls,
    instanceUploadBytesMax: observed.instanceUploadBytes,
    retainedMeshCpuBytesMax: observed.retainedMeshCpuBytes,
  };
}

export async function measureBaseline(options: MeasureOptions = {}): Promise<BaselineRecord> {
  if (!Number.isSafeInteger(INSTANCE_COUNT) || INSTANCE_COUNT <= 0) {
    throw new Error("SAFFRON_BASELINE_INSTANCES must be a positive safe integer");
  }
  if (!Number.isSafeInteger(CAPTURE_FRAMES) || CAPTURE_FRAMES < 16) {
    throw new Error("SAFFRON_BASELINE_FRAMES must be a safe integer of at least 16");
  }

  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  try {
    const initialStats = await engine.call("render-stats");
    options.precheck?.(initialStats);

    for (let index = 0; index < INSTANCE_COUNT; index++) {
      const entity = await engine.call("add-entity", {
        preset: PRESETS[index % PRESETS.length],
      });
      const width = Math.ceil(Math.sqrt(INSTANCE_COUNT));
      const x = (index % width) - (width - 1) * 0.5;
      const z = Math.floor(index / width) - (Math.ceil(INSTANCE_COUNT / width) - 1) * 0.5;
      await engine.call("set-transform", {
        entity: entity.id,
        translation: { x: x * 1.5, y: 0, z: z * 1.5 },
      });
    }
    await engine.call("add-entity", { preset: "point-light" });
    await engine.call("add-entity", { preset: "spot-light" });
    if (initialStats.rtSupported) {
      await engine.call("set-rt-shadows", { enabled: true });
    }

    const profiler = await engine.call("profiler.set-mode", {
      mode: "timestamps",
    });
    await engine.settle(1000);
    await engine.call("profiler.capture-start", {
      mode: "frames",
      frames: CAPTURE_FRAMES,
      includeCpu: true,
    });

    const statsSamples: RenderStatsDto[] = [];
    let previousFrame = -1;
    for (let poll = 0; poll < CAPTURE_FRAMES * 20; poll++) {
      const status = await engine.call("profiler.capture-status");
      if (status.capturedFrames !== previousFrame) {
        statsSamples.push(await engine.call("render-stats"));
        previousFrame = status.capturedFrames;
      }
      if (status.state === "ready") break;
      await engine.settle(5);
    }
    const capture = await engine.call("profiler.capture-stop");
    if (!capture.ready) throw new Error("profile capture did not become ready");

    const history = await engine.call("frame-history", { samples: CAPTURE_FRAMES });
    const capturedHistory = history.samples.slice(-capture.frameCount);
    const finalStats = await engine.call("render-stats");
    const observed: BaselineObserved = {
      sceneGatherMs: distribution(statsSamples.map((sample) => sample.sceneGatherMs)),
      cpuFrameMs: distribution(capturedHistory.map((sample) => sample.cpuMs)),
      gpuFrameMs: distribution(capturedHistory.map((sample) => sample.gpuMs)),
      drawCalls: finalStats.drawCalls,
      batches: finalStats.batches,
      instances: finalStats.instances,
      triangles: finalStats.triangles,
      instanceUploadBytes: finalStats.instanceUploadBytes,
      shadowDrawCalls: finalStats.shadowDrawCalls,
      rtInstances: finalStats.rtInstances,
      retainedMeshCpuBytes: finalStats.retainedMeshCpuBytes,
      vramUsageBytes: finalStats.vramUsageBytes,
      vramBudgetBytes: finalStats.vramBudgetBytes,
    };

    return {
      schemaVersion: 1,
      recordedAt: new Date().toISOString(),
      fixture: {
        name: "phase-1-heterogeneous-mesh-baseline",
        primitiveInstances: INSTANCE_COUNT,
        primitiveCycle: PRESETS,
        pointLights: 1,
        spotLights: 1,
        starterDirectionalLights: 1,
        captureFrames: capture.frameCount,
        viewport: [1280, 720],
      },
      platform: {
        os: platform(),
        osRelease: release(),
        architecture: arch(),
        cpu: cpus()[0]?.model ?? "unknown",
        rustc: command("rustc", ["--version", "--verbose"]),
        revision: command("git", ["rev-parse", "HEAD"]),
        workingTreeDirty: command("git", ["status", "--porcelain"]) !== "",
        gpu: capture.capture.metadata.deviceName,
        softwareGpu: capture.capture.metadata.softwareGpu,
        rtSupported: finalStats.rtSupported,
        profilerMode: profiler.mode,
        timestampsSupported: profiler.timestampsSupported,
      },
      observed,
      budgets: deriveBudgets(observed),
      validationErrors: engine.validationErrors(),
    };
  } finally {
    await engine.shutdown();
  }
}
