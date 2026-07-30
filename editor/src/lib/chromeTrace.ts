/// Client-side Chrome Trace Event JSON from a `ProfileCaptureDto`, so the Profiler's Download works
/// uniformly for single- and multi-frame captures. `M` events name the two lanes; `X` complete
/// events carry µs ts/dur, and the viewer derives nesting from time containment.
import type { ProfileCaptureDto } from "../protocol";

const CPU_TID = 1;
const GPU_TID = 2;

export function captureToChromeTrace(capture: ProfileCaptureDto): string {
  const traceEvents: Record<string, unknown>[] = [
    { ph: "M", pid: "SaffronAnima", name: "process_name", args: { name: "SaffronAnima" } },
    {
      ph: "M",
      pid: "SaffronAnima",
      tid: CPU_TID,
      name: "thread_name",
      args: { name: "CPU render thread" },
    },
    {
      ph: "M",
      pid: "SaffronAnima",
      tid: GPU_TID,
      name: "thread_name",
      args: { name: "GPU queue" },
    },
  ];
  for (const span of capture.spans) {
    traceEvents.push({
      ph: "X",
      pid: "SaffronAnima",
      tid: span.lane === "gpu" ? GPU_TID : CPU_TID,
      name: span.name,
      ts: span.startNs / 1000,
      dur: Math.max(0, span.endNs - span.startNs) / 1000,
      args: { depth: span.depth },
    });
  }
  return JSON.stringify({
    traceEvents,
    displayTimeUnit: "ns",
    otherData: { ...capture.metadata },
  });
}
