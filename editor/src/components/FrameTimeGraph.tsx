/// The live frame-time graph on a uPlot canvas — the webview composites over the live engine
/// viewport, so editor CPU is not free. The instance is created once and data is pushed
/// imperatively on a rAF tick, never through React state. Raw per-frame samples are far too jittery
/// to plot, so the chosen range is averaged into buckets, drawn as a monotone-cubic spline against a
/// sticky, budget-anchored Y ceiling that does not jump.
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { useEffect, useRef } from "react";
import { useEditorStore } from "../state/store";
import { bucketSeries } from "../lib/frameSeries";
import { GRAPH_COLORS } from "../lib/perfThresholds";

const HEIGHT = 150;
const MAX_BUCKETS = 200;

// Stable-Y tuning: grow the ceiling immediately, shrink it only when the target sits well
// below the current ceiling for a dwell — so a passing spike does not snap the axis back.
const SHRINK_FRAC = 0.6; // shrink only when target < 60% of the current ceiling
const SHRINK_DWELL = 5; // …and stays there this many range() calls
const BUDGET_ANCHOR = 1.6; // ceiling ≥ 1.6× budget, so the dashed line sits at a fixed height
const HEADROOM = 1.1; // 10% headroom over the live data max before nice-rounding

// The only spline uPlot ships is monotone-cubic (no overshoot below 0); linear is the default.
const splinePaths = uPlot.paths.spline?.();

function series(label: string, stroke: string): uPlot.Series {
  return { label, stroke, width: 1.25, points: { show: false }, paths: splinePaths };
}

/// Round up to the nearest 1 / 2 / 5 × 10^k — so the axis snaps to stable levels.
export function niceCeil(v: number): number {
  if (v <= 0) {
    return 1;
  }
  const base = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 5, 10]) {
    if (v <= m * base) {
      return m * base;
    }
  }
  return 10 * base;
}

function maxFinite(arr: ArrayLike<number | null | undefined> | undefined): number {
  let m = 0;
  if (!arr) {
    return m;
  }
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (typeof v === "number" && Number.isFinite(v) && v > m) {
      m = v;
    }
  }
  return m;
}

export function FrameTimeGraph({
  budgetMs,
  rangeFrames,
  bucketFrames,
  bucketSeconds,
}: {
  budgetMs: number;
  rangeFrames: number;
  bucketFrames: number;
  bucketSeconds: number;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const budgetRef = useRef(budgetMs);
  const rangeRef = useRef(rangeFrames);
  const bucketFramesRef = useRef(bucketFrames);
  const bucketSecondsRef = useRef(bucketSeconds);
  const applyRef = useRef<(() => void) | null>(null);
  const plotRef = useRef<uPlot | null>(null);
  const stickyMaxRef = useRef(0);
  const dwellRef = useRef(0);
  budgetRef.current = budgetMs;
  rangeRef.current = rangeFrames;
  bucketFramesRef.current = bucketFrames;
  bucketSecondsRef.current = bucketSeconds;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }

    const axis: uPlot.Axis = {
      stroke: GRAPH_COLORS.axis,
      grid: { stroke: GRAPH_COLORS.grid, width: 1 },
      ticks: { stroke: GRAPH_COLORS.grid, width: 1 },
      font: "10px monospace",
    };

    // Sticky, nice-rounded, budget-anchored Y ceiling — the readability win.
    const yRange = (self: uPlot): uPlot.Range.MinMax => {
      const data = self.data[1] as ArrayLike<number> | undefined;
      if (!data || data.length === 0) {
        // No history (startup / engine restart) — re-anchor from scratch.
        stickyMaxRef.current = 0;
        dwellRef.current = 0;
      }
      const budget = budgetRef.current;
      const target = niceCeil(
        Math.max(maxFinite(data) * HEADROOM, budget > 0 ? budget * BUDGET_ANCHOR : 0, 1),
      );
      const current = stickyMaxRef.current;
      if (target >= current) {
        stickyMaxRef.current = target;
        dwellRef.current = 0;
      } else if (target < current * SHRINK_FRAC) {
        dwellRef.current += 1;
        if (dwellRef.current >= SHRINK_DWELL) {
          stickyMaxRef.current = target;
          dwellRef.current = 0;
        }
      } else {
        dwellRef.current = 0;
      }
      return [0, stickyMaxRef.current];
    };

    const opts: uPlot.Options = {
      width: host.clientWidth || 320,
      height: HEIGHT,
      cursor: { show: false },
      legend: { show: false },
      scales: { x: { time: false }, y: { auto: false, range: yRange } },
      axes: [
        {
          ...axis,
          show: true,
          // Enough vertical room for the tick + gap + the 10px label (a smaller `size`, e.g.
          // 18, clips the labels — the axis row renders but the text is cut off).
          size: 28,
          gap: 2,
          ticks: { stroke: GRAPH_COLORS.grid, width: 1, size: 4 },
          // x is seconds-ago (negative); show it as positive relative time counting down to
          // "now" at the right edge: e.g. 30s … 5s … now.
          values: (_u, splits) =>
            splits.map((v) => (Math.round(v) === 0 ? "now" : `${Math.abs(Math.round(v))}s`)),
        },
        { ...axis, size: 34 },
      ],
      series: [
        {},
        series("total", GRAPH_COLORS.total),
        series("cpu", GRAPH_COLORS.cpu),
        series("gpu", GRAPH_COLORS.gpu),
      ],
      hooks: {
        draw: [
          (u: uPlot) => {
            const budget = budgetRef.current;
            if (budget <= 0) {
              return;
            }
            const y = u.valToPos(budget, "y", true);
            if (!Number.isFinite(y)) {
              return;
            }
            const ctx = u.ctx;
            ctx.save();
            ctx.strokeStyle = GRAPH_COLORS.budget;
            ctx.setLineDash([4, 3]);
            ctx.lineWidth = 1;
            ctx.beginPath();
            ctx.moveTo(u.bbox.left, y);
            ctx.lineTo(u.bbox.left + u.bbox.width, y);
            ctx.stroke();
            ctx.restore();
          },
        ],
      },
    };

    const empty: uPlot.AlignedData = [[], [], [], []];
    const plot = new uPlot(opts, empty, host);
    plotRef.current = plot;

    const apply = (): void => {
      const s = bucketSeries(
        rangeRef.current,
        bucketFramesRef.current,
        MAX_BUCKETS,
        bucketSecondsRef.current,
      );
      plot.setData([s.x, s.total, s.cpu, s.gpu]);
    };
    applyRef.current = apply;

    let raf = 0;
    let lastHistory = useEditorStore.getState().frameHistory;
    const unsub = useEditorStore.subscribe((state) => {
      if (state.frameHistory !== lastHistory) {
        lastHistory = state.frameHistory;
        if (raf === 0) {
          raf = requestAnimationFrame(() => {
            raf = 0;
            apply();
          });
        }
      }
    });
    apply();

    const ro = new ResizeObserver(() => {
      plot.setSize({ width: host.clientWidth || 320, height: HEIGHT });
    });
    ro.observe(host);

    return () => {
      if (raf !== 0) {
        cancelAnimationFrame(raf);
      }
      unsub();
      ro.disconnect();
      applyRef.current = null;
      plotRef.current = null;
      plot.destroy();
    };
  }, []);

  // Re-bucket immediately when Range or Window changes (not only on the next poll).
  useEffect(() => {
    applyRef.current?.();
  }, [rangeFrames, bucketFrames, bucketSeconds]);

  // A budget change moves the anchor + the dashed line without new data — force a redraw.
  useEffect(() => {
    plotRef.current?.redraw();
  }, [budgetMs]);

  // The uPlot host is `absolute inset-0` inside an `overflow-hidden min-w-0` box so uPlot's
  // explicit pixel width can never prop the box (or the shrink-to-content ScrollArea wrapper
  // above it) open: the ResizeObserver then measures the true available width and the chart
  // shrinks with the sidebar instead of pinning it wide.
  return (
    <div className="relative w-full min-w-0 overflow-hidden" style={{ height: HEIGHT }}>
      <div ref={hostRef} className="absolute inset-0" />
    </div>
  );
}
