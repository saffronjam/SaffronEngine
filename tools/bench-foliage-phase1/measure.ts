import { spawnSync } from "node:child_process";
import { arch, cpus, platform, release } from "node:os";
import { Engine } from "../../tests/e2e/harness.ts";
import type {
  CaptureStatusResult,
  CaptureStopResult,
  EntityRef,
  FrameHistoryDto,
  ProfilerModeResult,
  RenderStatsDto,
} from "../../editor/src/protocol/sa-types.ts";

const INSTANCE_COUNT = Number(process.env.SAFFRON_BASELINE_INSTANCES ?? 512);
const CAPTURE_FRAMES = Number(process.env.SAFFRON_BASELINE_FRAMES ?? 32);
const PRESETS = ["cube", "plane", "sphere"] as const;

function command(program: string, args: string[]): string {
  const result = spawnSync(program, args, { encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : "unavailable";
}

function percentile(values: number[], fraction: number): number {
  if (values.length === 0) return 0;
  const sorted = values.toSorted((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)];
}

function distribution(values: number[]) {
  return {
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
    max: values.length === 0 ? 0 : Math.max(...values),
  };
}

function ceiling(value: number, absoluteHeadroom: number): number {
  return Number(Math.max(value * 1.25, value + absoluteHeadroom).toFixed(3));
}

async function main(): Promise<void> {
  if (!Number.isSafeInteger(INSTANCE_COUNT) || INSTANCE_COUNT <= 0) {
    throw new Error("SAFFRON_BASELINE_INSTANCES must be a positive safe integer");
  }
  if (!Number.isSafeInteger(CAPTURE_FRAMES) || CAPTURE_FRAMES < 16) {
    throw new Error("SAFFRON_BASELINE_FRAMES must be a safe integer of at least 16");
  }

  const engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  try {
    for (let index = 0; index < INSTANCE_COUNT; index++) {
      const entity = await engine.call<EntityRef>("add-entity", {
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
    const initialStats = await engine.call<RenderStatsDto>("render-stats");
    if (initialStats.rtSupported) {
      await engine.call("set-rt-shadows", { enabled: true });
    }

    const profiler = await engine.call<ProfilerModeResult>("profiler.set-mode", {
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
      const status = await engine.call<CaptureStatusResult>("profiler.capture-status");
      if (status.capturedFrames !== previousFrame) {
        statsSamples.push(await engine.call<RenderStatsDto>("render-stats"));
        previousFrame = status.capturedFrames;
      }
      if (status.state === "ready") break;
      await engine.settle(5);
    }
    const capture = await engine.call<CaptureStopResult>("profiler.capture-stop");
    if (!capture.ready) throw new Error("profile capture did not become ready");

    const history = await engine.call<FrameHistoryDto>("frame-history", {
      samples: CAPTURE_FRAMES,
    });
    const capturedHistory = history.samples.slice(-capture.frameCount);
    const finalStats = await engine.call<RenderStatsDto>("render-stats");
    const gather = distribution(statsSamples.map((sample) => sample.sceneGatherMs));
    const cpu = distribution(capturedHistory.map((sample) => sample.cpuMs));
    const gpu = distribution(capturedHistory.map((sample) => sample.gpuMs));

    const output = {
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
      observed: {
        sceneGatherMs: gather,
        cpuFrameMs: cpu,
        gpuFrameMs: gpu,
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
      },
      budgets: {
        derivation:
          "Anima steady-state p95 plus 25% or an absolute noise floor, whichever is larger",
        sceneGatherP95Ms: ceiling(gather.p95, 0.05),
        cpuFrameP95Ms: ceiling(cpu.p95, 0.25),
        gpuFrameP95Ms: ceiling(gpu.p95, 0.25),
        drawCallsMax: finalStats.drawCalls,
        instanceUploadBytesMax: finalStats.instanceUploadBytes,
        retainedMeshCpuBytesMax: finalStats.retainedMeshCpuBytes,
      },
      validationErrors: engine.validationErrors(),
    };
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
  } finally {
    await engine.shutdown();
  }
}

await main();
