/// A long client-side history of per-frame timing, accumulated across metrics polls and deduplicated
/// by the engine's absolute frame index (consecutive polls return overlapping windows of the
/// engine's ring). Module scope and a fixed typed-array ring rather than React state, because it is
/// high-frequency, high-volume data.
import type { FrameSampleDto } from "../protocol";

/// Retained frames (~5 min even at 240 fps; ~20 min at 60 fps). The graph never plots these
/// raw — it averages a chosen Range into bucket-sized groups — so retention only bounds how
/// far back the Range can reach. Memory ≈ frames × 20 B (Float64 index + 3× Float32) ≈ 1.4 MB.
/// A tiered raw+downsampled scheme would buy longer horizons but is overkill at this size.
const CAPACITY = 72000;

const frameIndex = new Float64Array(CAPACITY);
const total = new Float32Array(CAPACITY);
const cpu = new Float32Array(CAPACITY);
const gpu = new Float32Array(CAPACITY);
let head = 0;
let count = 0;
let lastIndex = -1;

/// Append a poll's samples, keeping only frames newer than the last one seen (the windows
/// overlap, so this dedup is what makes accumulation correct).
export function appendFrameSamples(samples: FrameSampleDto[]): void {
  // A restarted engine resets its frame index to 0; if the newest incoming frame is older
  // than the last one we recorded, the run changed — drop the stale history.
  if (samples.length > 0 && samples[samples.length - 1].frameIndex < lastIndex) {
    resetFrameSeries();
  }
  for (const s of samples) {
    if (s.frameIndex <= lastIndex) {
      continue;
    }
    lastIndex = s.frameIndex;
    frameIndex[head] = s.frameIndex;
    total[head] = s.cpuMs + s.cpuWaitMs;
    cpu[head] = s.cpuMs;
    gpu[head] = s.gpuMs;
    head = (head + 1) % CAPACITY;
    if (count < CAPACITY) {
      count += 1;
    }
  }
}

/// Reset on engine restart so a stale frame index never rejects the new run's frames.
export function resetFrameSeries(): void {
  head = 0;
  count = 0;
  lastIndex = -1;
}

export interface BucketedSeries {
  /// Seconds-ago, NEGATIVE, ascending left→right: the newest bucket sits at ≈ 0 (the right
  /// edge), older buckets are more negative. Lets the x axis read as a "last N seconds" trend.
  x: number[];
  total: number[];
  cpu: number[];
  gpu: number[];
}

/// Grafana-style downsample for the graph: take the most recent `rangeFrames` of history and
/// average them into buckets of `bucketFrames` each (the group-by / smoothness knob). Bucket
/// count = clamp(ceil(range / bucket), 1, maxBuckets); when that cap bites — a finer bucket
/// than range/maxBuckets — the effective bucket widens (Grafana's min-interval coarsening), so
/// the line never renders more than `maxBuckets` points. `bucketSeconds` is the wall-clock width
/// of one bucket (the caller derives it from fps, or passes the chosen ms interval), used only
/// to position `x` as seconds-ago — this module never touches fps, so it cannot divide-by-zero.
export function bucketSeries(
  rangeFrames: number,
  bucketFrames: number,
  maxBuckets: number,
  bucketSeconds: number,
): BucketedSeries {
  const range = Math.min(Math.round(rangeFrames), count);
  if (range < 1) {
    return { x: [], total: [], cpu: [], gpu: [] }; // no history yet (or just reset)
  }
  const bucket = Math.max(1, Math.round(bucketFrames));
  const buckets = Math.max(1, Math.min(maxBuckets, Math.ceil(range / bucket)));
  const size = range / buckets; // ≥ bucketFrames when the maxBuckets cap coarsened it
  // Physical ring index of the oldest frame in the chosen range.
  const base = (head + CAPACITY - range) % CAPACITY;
  const out: BucketedSeries = { x: [], total: [], cpu: [], gpu: [] };
  for (let b = 0; b < buckets; b++) {
    const lo = Math.floor(b * size);
    const hi = Math.min(range, Math.floor((b + 1) * size));
    let ts = 0;
    let cs = 0;
    let gs = 0;
    let k = 0;
    for (let i = lo; i < hi; i++) {
      const ring = (base + i) % CAPACITY;
      ts += total[ring];
      cs += cpu[ring];
      gs += gpu[ring];
      k += 1;
    }
    if (k === 0) {
      continue;
    }
    // Newest bucket (highest b) at x ≈ 0; older buckets at increasingly negative seconds-ago.
    out.x.push(-(buckets - 1 - b) * bucketSeconds);
    out.total.push(ts / k);
    out.cpu.push(cs / k);
    out.gpu.push(gs / k);
  }
  return out;
}
