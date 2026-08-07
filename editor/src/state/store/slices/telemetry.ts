import {
  loadCaptureIncludeStats,
  loadCaptureWindowFrames,
  loadMetricsBucketMs,
  loadMetricsRangeSec,
  loadMetricsRefreshMs,
  persistCaptureIncludeStats,
  persistCaptureWindowFrames,
  persistMetricsBucketMs,
  persistMetricsRangeSec,
  persistMetricsRefreshMs,
} from "../persistence";
import type { SetEditorState, TelemetrySlice } from "../types";

const ALARM_LOG_LIMIT = 200;
const CONTACT_LOG_LIMIT = 200;
/// The engine's own ring is bounded too; the editor keeps a deeper window.
const SCRIPT_LOG_LIMIT = 2000;

export function createTelemetrySlice(set: SetEditorState): TelemetrySlice {
  return {
    renderStats: null,
    physicsState: null,
    physicsBodies: [],
    contactLog: [],
    contactsOverflowed: false,
    scriptLogs: [],
    scriptLogsOverflowed: false,
    debugOverlays: null,
    targetFpsMode: "default",
    upscale: null,
    perfConfig: null,
    frameHistory: null,
    passTimings: null,
    activeAlarms: [],
    alarmLog: [],
    metricsRangeSec: loadMetricsRangeSec(),
    metricsBucketMs: loadMetricsBucketMs(),
    metricsRefreshMs: loadMetricsRefreshMs(),
    metricsPaused: false,
    captureState: "idle",
    captureProgress: { current: 0, total: 0 },
    capture: null,
    captureWindowFrames: loadCaptureWindowFrames(),
    captureIncludeStats: loadCaptureIncludeStats(),
    selectedPass: null,
    pollRateHz: 0,
    uiFrameRateHz: 0,
    uiFrameMs: 0,

    setRenderStats: (renderStats) => set({ renderStats }),
    setPhysicsState: (physicsState) => set({ physicsState }),
    setPhysicsBodies: (physicsBodies) => set({ physicsBodies }),
    appendContactEvents: (events, overflowed) =>
      set((s) => {
        if (events.length === 0) {
          return overflowed === s.contactsOverflowed ? {} : { contactsOverflowed: overflowed };
        }
        // Newest-first, bounded ring: the engine drains oldest→newest, so prepend reversed.
        const contactLog = [...events].reverse().concat(s.contactLog).slice(0, CONTACT_LOG_LIMIT);
        return { contactLog, contactsOverflowed: overflowed };
      }),
    clearContacts: () => set({ contactLog: [], contactsOverflowed: false }),
    appendScriptLogs: (events, overflowed) =>
      set((s) => {
        const nextOverflowed = s.scriptLogsOverflowed || overflowed; // a dropped line stays dropped
        if (events.length === 0) {
          return nextOverflowed === s.scriptLogsOverflowed
            ? {}
            : { scriptLogsOverflowed: nextOverflowed };
        }
        // Chronological (the engine drains oldest→newest by seq); keep the last N.
        const scriptLogs = s.scriptLogs.concat(events).slice(-SCRIPT_LOG_LIMIT);
        return { scriptLogs, scriptLogsOverflowed: nextOverflowed };
      }),
    clearScriptLogs: () => set({ scriptLogs: [], scriptLogsOverflowed: false }),
    setDebugOverlays: (debugOverlays) => set({ debugOverlays }),
    setPerfConfig: (perfConfig) => set({ perfConfig }),
    setUpscale: (upscale) => set({ upscale }),
    setTargetFpsMode: (targetFpsMode) => set({ targetFpsMode }),
    setFrameHistory: (frameHistory) => set({ frameHistory }),
    setPassTimings: (passTimings) => set({ passTimings }),
    setActiveAlarms: (activeAlarms) => set({ activeAlarms }),
    appendAlarmEvents: (events) =>
      set((s) => {
        if (events.length === 0) {
          return {};
        }
        const alarmLog = [...s.alarmLog, ...events];
        return { alarmLog: alarmLog.slice(Math.max(0, alarmLog.length - ALARM_LOG_LIMIT)) };
      }),
    setMetricsRangeSec: (metricsRangeSec) => {
      persistMetricsRangeSec(metricsRangeSec);
      set({ metricsRangeSec });
    },
    setMetricsBucketMs: (metricsBucketMs) => {
      persistMetricsBucketMs(metricsBucketMs);
      set({ metricsBucketMs });
    },
    setMetricsRefreshMs: (metricsRefreshMs) => {
      persistMetricsRefreshMs(metricsRefreshMs);
      set({ metricsRefreshMs });
    },
    setMetricsPaused: (metricsPaused) => set({ metricsPaused }),
    setCaptureState: (captureState) => set({ captureState }),
    setCaptureProgress: (current, total) => set({ captureProgress: { current, total } }),
    setCapture: (capture) => set({ capture }),
    setCaptureWindowFrames: (captureWindowFrames) => {
      persistCaptureWindowFrames(captureWindowFrames);
      set({ captureWindowFrames });
    },
    setCaptureIncludeStats: (captureIncludeStats) => {
      persistCaptureIncludeStats(captureIncludeStats);
      set({ captureIncludeStats });
    },
    setSelectedPass: (selectedPass) => set({ selectedPass }),
    setPollRateHz: (pollRateHz) => set({ pollRateHz }),
    setUiFrameStats: (uiFrameRateHz, uiFrameMs) => set({ uiFrameRateHz, uiFrameMs }),
  };
}
