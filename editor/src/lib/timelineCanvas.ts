/// The timeline's lane/ruler/playhead renderer: a 2D canvas driven imperatively, created once and
/// fed a model and playhead, redrawing on a coalesced rAF tick. The webview composites over the live
/// engine viewport, so the playhead advancing every poll must touch only this canvas, never the
/// component tree. Area-virtualized: only ticks and bars overlapping the visible width draw.

/// Fixed dark-theme palette (the app is dark-only) — the 2D canvas cannot reliably read the
/// oklch theme tokens, mirroring `GRAPH_COLORS` in `perfThresholds.ts`.
const COLORS = {
  rulerLine: "rgba(255,255,255,0.10)",
  rulerTick: "rgba(255,255,255,0.18)",
  rulerLabel: "rgba(255,255,255,0.55)",
  laneLine: "rgba(255,255,255,0.06)",
  laneAlt: "rgba(255,255,255,0.015)",
  clipBorder: "rgba(255,255,255,0.22)",
  clipLabel: "rgba(255,255,255,0.92)",
  playhead: "#e5e7eb",
} as const;

/// One track row. `accent` is the type-color swatch shown in the header and tinting the bar.
export interface TimelineTrack {
  id: string;
  accent: string;
}

/// One animation channel of a clip: a display label and its keyframe sample times (seconds,
/// relative to the clip start) — the real per-channel keyframe data from `AnimationChannelDto`.
export interface TimelineChannel {
  label: string;
  times: number[];
}

/// One clip bar on a track: a span [start, start+duration] in seconds with a label. `channels`
/// carries the clip's per-channel keyframe strips, drawn as keyframe ticks under the bar.
export interface TimelineClip {
  trackId: string;
  label: string;
  start: number;
  duration: number;
  channels?: TimelineChannel[];
}

export interface TimelineModel {
  /// Total timeline length in seconds (the ruler/lane horizontal extent).
  duration: number;
  /// Track rows top-to-bottom; index → vertical lane.
  tracks: TimelineTrack[];
  clips: TimelineClip[];
}

const RULER_HEIGHT = 22;
const ROW_HEIGHT = 24;
/// Target spacing (px) between ruler ticks before label thinning kicks in.
const TARGET_TICK_PX = 64;
/// Candidate tick steps in milliseconds, ascending — the ruler picks the smallest that keeps
/// ticks at least TARGET_TICK_PX apart, so labels never crowd.
const TICK_STEPS_MS = [10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, 60000];

function emptyModel(): TimelineModel {
  return { duration: 1, tracks: [], clips: [] };
}

/// Pick the ms tick step that yields the widest spacing still under the px target.
export function chooseTickStepMs(durationSec: number, widthPx: number): number {
  const pxPerMs = widthPx / Math.max(durationSec * 1000, 1);
  for (const step of TICK_STEPS_MS) {
    if (step * pxPerMs >= TARGET_TICK_PX) {
      return step;
    }
  }
  return TICK_STEPS_MS[TICK_STEPS_MS.length - 1];
}

export function formatTick(ms: number): string {
  if (ms === 0) {
    return "0ms";
  }
  if (ms % 1000 === 0) {
    return `${ms / 1000}s`;
  }
  return `${Math.round(ms)}ms`;
}

export class TimelineCanvas {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private model: TimelineModel = emptyModel();
  private playheadSec = 0;
  private widthCss = 0;
  private heightCss = 0;
  private dpr = 1;
  private raf = 0;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      throw new Error("2d canvas context unavailable");
    }
    this.ctx = ctx;
  }

  /// The lane area below the ruler — track rows live here; the playhead spans both.
  get rulerHeight(): number {
    return RULER_HEIGHT;
  }

  get rowHeight(): number {
    return ROW_HEIGHT;
  }

  /// Seconds → x in CSS px across the full content width (the lane is not horizontally
  /// scrolled — the clip fits the panel; the transform centralizes any zoom/pan).
  secToX(sec: number): number {
    const d = Math.max(this.model.duration, 0.0001);
    return (sec / d) * this.widthCss;
  }

  /// x in CSS px → seconds, clamped to [0, duration]. Used by the scrub hit target.
  xToSec(x: number): number {
    const d = Math.max(this.model.duration, 0.0001);
    const sec = (x / Math.max(this.widthCss, 1)) * d;
    return Math.min(d, Math.max(0, sec));
  }

  setSize(widthCss: number, heightCss: number, dpr: number): void {
    this.widthCss = widthCss;
    this.heightCss = heightCss;
    this.dpr = dpr;
    this.canvas.width = Math.max(1, Math.round(widthCss * dpr));
    this.canvas.height = Math.max(1, Math.round(heightCss * dpr));
    this.canvas.style.width = `${widthCss}px`;
    this.canvas.style.height = `${heightCss}px`;
    this.scheduleDraw();
  }

  setModel(model: TimelineModel): void {
    this.model = model;
    this.scheduleDraw();
  }

  /// Move the playhead (seconds). The cheapest update in the panel — only this canvas redraws,
  /// so the playhead advancing on every reconcile poll never re-renders React.
  setPlayhead(sec: number): void {
    if (sec === this.playheadSec) {
      return;
    }
    this.playheadSec = sec;
    this.scheduleDraw();
  }

  private scheduleDraw(): void {
    if (this.raf !== 0) {
      return;
    }
    this.raf = requestAnimationFrame(() => {
      this.raf = 0;
      this.draw();
    });
  }

  destroy(): void {
    if (this.raf !== 0) {
      cancelAnimationFrame(this.raf);
      this.raf = 0;
    }
  }

  private draw(): void {
    const { ctx } = this;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, this.widthCss, this.heightCss);
    this.drawRuler();
    this.drawLanes();
    this.drawPlayhead();
  }

  private drawRuler(): void {
    const { ctx, model, widthCss } = this;
    const stepMs = chooseTickStepMs(model.duration, widthCss);
    const durationMs = model.duration * 1000;

    ctx.font = "10px ui-monospace, monospace";
    ctx.textBaseline = "alphabetic";
    ctx.fillStyle = COLORS.rulerLabel;
    ctx.strokeStyle = COLORS.rulerTick;
    ctx.lineWidth = 1;

    for (let ms = 0; ms <= durationMs + 0.5; ms += stepMs) {
      const x = Math.round(this.secToX(ms / 1000)) + 0.5;
      if (x < -1 || x > widthCss + 1) {
        continue; // area-virtualized: skip ticks outside the visible width
      }
      ctx.beginPath();
      ctx.moveTo(x, RULER_HEIGHT - 6);
      ctx.lineTo(x, RULER_HEIGHT);
      ctx.stroke();
      ctx.fillText(formatTick(ms), x + 3, RULER_HEIGHT - 8);
    }

    // The ruler/lane divider.
    ctx.strokeStyle = COLORS.rulerLine;
    ctx.beginPath();
    ctx.moveTo(0, RULER_HEIGHT + 0.5);
    ctx.lineTo(widthCss, RULER_HEIGHT + 0.5);
    ctx.stroke();
  }

  private drawLanes(): void {
    const { ctx, model, widthCss } = this;
    for (let row = 0; row < model.tracks.length; row++) {
      const top = RULER_HEIGHT + row * ROW_HEIGHT;
      if (row % 2 === 1) {
        ctx.fillStyle = COLORS.laneAlt;
        ctx.fillRect(0, top, widthCss, ROW_HEIGHT);
      }
      ctx.strokeStyle = COLORS.laneLine;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, top + ROW_HEIGHT + 0.5);
      ctx.lineTo(widthCss, top + ROW_HEIGHT + 0.5);
      ctx.stroke();
    }

    this.drawBars();
  }

  private rowOf(trackId: string): number {
    return this.model.tracks.findIndex((t) => t.id === trackId);
  }

  private drawBars(): void {
    const { ctx, model, widthCss } = this;
    for (const clip of model.clips) {
      const row = this.rowOf(clip.trackId);
      if (row < 0) {
        continue;
      }
      const x0 = this.secToX(clip.start);
      const x1 = this.secToX(clip.start + clip.duration);
      if (x1 < 0 || x0 > widthCss) {
        continue; // off-screen bar
      }
      const accent = model.tracks[row].accent;
      const top = RULER_HEIGHT + row * ROW_HEIGHT + 3;
      const h = ROW_HEIGHT - 6;
      const left = Math.max(0, x0);
      const w = Math.max(2, Math.min(widthCss, x1) - left);

      ctx.fillStyle = withAlpha(accent, 0.32);
      this.roundRect(left, top, w, h, 3);
      ctx.fill();
      ctx.strokeStyle = COLORS.clipBorder;
      ctx.lineWidth = 1;
      this.roundRect(left + 0.5, top + 0.5, w - 1, h - 1, 3);
      ctx.stroke();
      // A 2px accent rail down the bar's leading edge — the type-color signal.
      ctx.fillStyle = accent;
      ctx.fillRect(left, top, 2, h);

      if (w > 28) {
        ctx.save();
        ctx.beginPath();
        ctx.rect(left, top, w, h);
        ctx.clip();
        ctx.fillStyle = COLORS.clipLabel;
        ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
        ctx.textBaseline = "middle";
        ctx.fillText(clip.label, left + 7, top + h / 2 + 0.5);
        ctx.restore();
      }

      // Real per-channel keyframe ticks: a short vertical mark at every channel keyframe
      // time, drawn along the bar's lower edge so the clip bar shows where keys actually
      // sit (the per-channel keyframe data from `AnimationChannelDto.times`). Clipped to the
      // bar so off-bar ticks never bleed.
      if (clip.channels && clip.channels.length > 0) {
        ctx.save();
        ctx.beginPath();
        ctx.rect(left, top, w, h);
        ctx.clip();
        ctx.strokeStyle = withAlpha(accent, 0.85);
        ctx.lineWidth = 1;
        const tickTop = top + h - 5;
        const tickBottom = top + h - 1;
        ctx.beginPath();
        for (const channel of clip.channels) {
          for (const t of channel.times) {
            const x = Math.round(this.secToX(clip.start + t)) + 0.5;
            if (x < left || x > left + w) {
              continue;
            }
            ctx.moveTo(x, tickTop);
            ctx.lineTo(x, tickBottom);
          }
        }
        ctx.stroke();
        ctx.restore();
      }
    }
  }

  private drawPlayhead(): void {
    const { ctx, heightCss } = this;
    const x = Math.round(this.secToX(this.playheadSec)) + 0.5;
    ctx.strokeStyle = COLORS.playhead;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, heightCss);
    ctx.stroke();
    // A small downward chevron head sitting in the ruler band.
    ctx.fillStyle = COLORS.playhead;
    ctx.beginPath();
    ctx.moveTo(x - 4, 0);
    ctx.lineTo(x + 4, 0);
    ctx.lineTo(x, 6);
    ctx.closePath();
    ctx.fill();
  }

  private roundRect(x: number, y: number, w: number, h: number, r: number): void {
    const rr = Math.min(r, w / 2, h / 2);
    const { ctx } = this;
    ctx.beginPath();
    ctx.moveTo(x + rr, y);
    ctx.arcTo(x + w, y, x + w, y + h, rr);
    ctx.arcTo(x + w, y + h, x, y + h, rr);
    ctx.arcTo(x, y + h, x, y, rr);
    ctx.arcTo(x, y, x + w, y, rr);
    ctx.closePath();
  }
}

/// Blend a hex or rgb(a) color toward transparent (canvas fills want an explicit alpha string).
export function withAlpha(color: string, alpha: number): string {
  const hex = color.trim();
  if (hex.startsWith("#")) {
    const n = hex.slice(1);
    const full =
      n.length === 3
        ? n
            .split("")
            .map((c) => c + c)
            .join("")
        : n;
    const r = parseInt(full.slice(0, 2), 16);
    const g = parseInt(full.slice(2, 4), 16);
    const b = parseInt(full.slice(4, 6), 16);
    return `rgba(${r}, ${g}, ${b}, ${alpha})`;
  }
  return color;
}
