/// Client-side Perfetto protobuf export (synthetic TrackEvent format). The engine stays free of a
/// protobuf dependency — the editor emits the bytes here and the panel saves them to disk; the
/// denser native format complements the Chrome-Trace JSON (chromeTrace.ts), which Perfetto also
/// ingests. Both are loaded into ui.perfetto.dev manually (see PERFETTO_URL).
import type { ProfileCaptureDto } from "../protocol";

// Minimal protobuf writer (only what the Trace proto needs).
// Varint over a JS number: values stay under 2^53 (host-ns timestamps ≈ 1e14, uuids tiny), so
// modulo arithmetic is exact — `& 0x7f` would wrongly truncate to 32 bits past 2^32.
function pushVarint(out: number[], value: number): void {
  let v = Math.max(0, Math.floor(value));
  while (v >= 128) {
    out.push((v % 128) + 128);
    v = Math.floor(v / 128);
  }
  out.push(v);
}

function pushTag(out: number[], field: number, wire: number): void {
  pushVarint(out, field * 8 + wire);
}

function pushVarintField(out: number[], field: number, value: number): void {
  pushTag(out, field, 0);
  pushVarint(out, value);
}

function pushBytesField(out: number[], field: number, bytes: number[]): void {
  pushTag(out, field, 2);
  pushVarint(out, bytes.length);
  for (const b of bytes) {
    out.push(b);
  }
}

function pushStringField(out: number[], field: number, value: string): void {
  pushBytesField(out, field, Array.from(new TextEncoder().encode(value)));
}

// Perfetto proto field numbers (stable; perfetto/protos/trace/*). Trace.packet, TracePacket
// fields, TrackDescriptor, and TrackEvent are the synthetic-track-event subset.
const FIELD_TRACE_PACKET = 1;
const FIELD_PKT_TIMESTAMP = 8;
const FIELD_PKT_SEQUENCE_ID = 10;
const FIELD_PKT_TRACK_EVENT = 11;
const FIELD_PKT_TRACK_DESCRIPTOR = 60;
const FIELD_TD_UUID = 1;
const FIELD_TD_NAME = 2;
const FIELD_TE_TYPE = 9;
const FIELD_TE_TRACK_UUID = 11;
const FIELD_TE_NAME = 23;

const TYPE_SLICE_BEGIN = 1;
const TYPE_SLICE_END = 2;
const SEQUENCE_ID = 1;
const CPU_TRACK = 1;
const GPU_TRACK = 2;

function trackDescriptorPacket(uuid: number, name: string): number[] {
  const td: number[] = [];
  pushVarintField(td, FIELD_TD_UUID, uuid);
  pushStringField(td, FIELD_TD_NAME, name);
  const pkt: number[] = [];
  pushBytesField(pkt, FIELD_PKT_TRACK_DESCRIPTOR, td);
  pushVarintField(pkt, FIELD_PKT_SEQUENCE_ID, SEQUENCE_ID);
  return pkt;
}

function slicePacket(ts: number, track: number, type: number, name: string): number[] {
  const te: number[] = [];
  pushVarintField(te, FIELD_TE_TYPE, type);
  pushVarintField(te, FIELD_TE_TRACK_UUID, track);
  if (type === TYPE_SLICE_BEGIN) {
    pushStringField(te, FIELD_TE_NAME, name);
  }
  const pkt: number[] = [];
  pushVarintField(pkt, FIELD_PKT_TIMESTAMP, ts);
  pushBytesField(pkt, FIELD_PKT_TRACK_EVENT, te);
  pushVarintField(pkt, FIELD_PKT_SEQUENCE_ID, SEQUENCE_ID);
  return pkt;
}

interface SliceEvent {
  ts: number;
  type: number;
  track: number;
  name: string;
  depth: number;
}

/// Encode a capture as a Perfetto protobuf trace: a CPU and a GPU track, then one SLICE_BEGIN /
/// SLICE_END pair per span. Events are emitted in timestamp order (ends before begins at a tie,
/// deeper-first), so each track's slices nest into a valid stack for the properly-nested spans.
export function toPerfettoTrace(capture: ProfileCaptureDto): Uint8Array<ArrayBuffer> {
  const trace: number[] = [];
  const addPacket = (pkt: number[]): void => pushBytesField(trace, FIELD_TRACE_PACKET, pkt);
  addPacket(trackDescriptorPacket(CPU_TRACK, "CPU render thread"));
  addPacket(trackDescriptorPacket(GPU_TRACK, "GPU queue"));

  const events: SliceEvent[] = [];
  for (const span of capture.spans) {
    const track = span.lane === "gpu" ? GPU_TRACK : CPU_TRACK;
    // A zero-or-negative-duration span (e.g. a sub-tick GPU pass like ssgi-history-restore, or a
    // pass whose timestamps were unavailable) must still get a strictly-positive width: with
    // begin and end at the same ts the tie-break below would emit SLICE_END before SLICE_BEGIN,
    // leaving the begin unmatched — Perfetto then renders it as "[Incomplete]". A 1 ns floor keeps
    // every slice a valid begin→end pair while staying visually instantaneous.
    const endTs = Math.max(span.endNs, span.startNs + 1);
    events.push({
      ts: span.startNs,
      type: TYPE_SLICE_BEGIN,
      track,
      name: span.name,
      depth: span.depth,
    });
    events.push({
      ts: endTs,
      type: TYPE_SLICE_END,
      track,
      name: "",
      depth: span.depth,
    });
  }
  events.sort((a, b) => {
    if (a.ts !== b.ts) {
      return a.ts - b.ts;
    }
    if (a.type !== b.type) {
      return a.type === TYPE_SLICE_END ? -1 : 1; // close before open at a shared instant
    }
    return a.type === TYPE_SLICE_END ? b.depth - a.depth : a.depth - b.depth;
  });
  for (const e of events) {
    addPacket(slicePacket(e.ts, e.track, e.type, e.name));
  }
  // Back the view with a fresh ArrayBuffer (not ArrayBufferLike) so it is a valid BlobPart.
  const bytes = new Uint8Array(trace.length);
  bytes.set(trace);
  return bytes;
}

/// The hosted Perfetto UI. Opened in the OS browser via the Tauri bridge — the postMessage
/// trace-handoff Perfetto documents only works between two windows in one browser context, which
/// the Tauri webview → desktop browser boundary is not, so the user loads a downloaded trace there.
export const PERFETTO_URL = "https://ui.perfetto.dev";
