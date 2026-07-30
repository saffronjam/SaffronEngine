import { call } from "./call";
import type { ProfilerMode } from "./types";
import type {
  ActiveAlarmsDto,
  CaptureStartParams,
  CaptureStartResult,
  CaptureStatusResult,
  CaptureStopResult,
  DrainAlarmsResult,
  FrameHistoryDto,
  GetUpscaleResult,
  PerfConfigDto,
  ProfilerModeResult,
  RenderPassTimingsDto,
  RenderStats,
  SetPerfConfigParams,
  SetUpscaleParams,
  SetUpscaleResult,
} from "../../protocol";

/// Frame statistics, the GPU profiler, upscale, and the perf-alarm stream.
export const telemetryCommands = {
  renderStats(): Promise<RenderStats> {
    return call("render-stats");
  },

  /// Set the GPU profiler depth. `timestamps` enables per-pass GPU timing + the VMA
  /// budget read; `off` returns to baseline cost. Reports device capability.
  setProfilerMode(mode: ProfilerMode): Promise<ProfilerModeResult> {
    return call("profiler.set-mode", { mode });
  },
  /// Last frame's per-pass GPU times (needs the profiler in timestamps mode).
  passTimings(): Promise<RenderPassTimingsDto> {
    return call("pass-timings");
  },
  /// Arm a bounded profiler capture (single frame by default). Forces timestamps mode +
  /// sub-scopes for the duration; returns the capture id + ack.
  captureStart(params: CaptureStartParams): Promise<CaptureStartResult> {
    return call("profiler.capture-start", params);
  },
  /// Non-destructive capture progress — poll while recording to drive the live counter and
  /// detect readiness without draining the capture.
  captureStatus(): Promise<CaptureStatusResult> {
    return call("profiler.capture-status");
  },
  /// Finish + return the armed capture. A single capture comes back inline (`capture` +
  /// `chromeTrace`); a multi-frame one is written to `path`. `ready` is false when none armed.
  captureStop(): Promise<CaptureStopResult> {
    return call("profiler.capture-stop");
  },
  /// Frame-time percentiles + stutter count, optionally with the recent raw samples
  /// (the live-graph source). Always recorded, independent of the profiler.
  frameHistory(samples?: number): Promise<FrameHistoryDto> {
    return call("frame-history", samples === undefined ? {} : { samples });
  },
  /// The shared budget / green-amber-red threshold config.
  getPerfConfig(): Promise<PerfConfigDto> {
    return call("get-perf-config");
  },
  setPerfConfig(params: SetPerfConfigParams): Promise<PerfConfigDto> {
    return call("set-perf-config", params);
  },
  /// The TAAU ratio + dynamic-resolution state + live input/display extents.
  getUpscale(): Promise<GetUpscaleResult> {
    return call("get-upscale", {});
  },
  /// Partial update of the upscale surface (ratio / dynamic / targetMs); omitted fields hold.
  setUpscale(params: SetUpscaleParams): Promise<SetUpscaleResult> {
    return call("set-upscale", params);
  },
  /// Drain perf-alarm events with seq > since (non-blocking) plus the cursor metadata.
  drainAlarms(since: number): Promise<DrainAlarmsResult> {
    return call("drain-alarms", { since });
  },
  /// The currently firing perf alarms (the badge + per-pass highlight source).
  listActiveAlarms(): Promise<ActiveAlarmsDto> {
    return call("list-active-alarms");
  },
};
