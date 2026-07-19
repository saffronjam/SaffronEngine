/// An SVG tone-curve editor: a `[0,1]×[0,1]` display-space spline with draggable control points for a
/// master + per-channel (R/G/B) curve. Click empty space to add a point, drag to move, right-click an
/// interior point to remove; the two endpoints keep their x and move only in y. The curve is a
/// monotone cubic (Fritsch–Carlson) through the points, so a tone curve never overshoots into a
/// contrast reversal. The widget owns drag-local rendering (`useScrubValue`) and brackets a gesture
/// with `onDragStart`/`onDragEnd`; the panel bakes the sampled curve into the creative-LUT slot.
import { useRef, useState } from "react";
import { useScrubValue } from "@/lib/useScrubValue";
import { cn } from "@/lib/utils";

export interface CurvePoint {
  x: number;
  y: number;
}

export interface ToneCurveChannels {
  master: CurvePoint[];
  r: CurvePoint[];
  g: CurvePoint[];
  b: CurvePoint[];
}

export type ToneChannel = keyof ToneCurveChannels;

export interface ToneCurveProps {
  channels: ToneCurveChannels;
  onChange(channels: ToneCurveChannels): void;
  visibleChannels?: ToneChannel[];
  masterLabel?: string;
  onDragStart?(): void;
  onDragEnd?(): void;
}

/// The identity curve: a straight line from black to white.
export const IDENTITY_CURVE: CurvePoint[] = [
  { x: 0, y: 0 },
  { x: 1, y: 1 },
];

export function identityChannels(): ToneCurveChannels {
  return {
    master: [...IDENTITY_CURVE],
    r: [...IDENTITY_CURVE],
    g: [...IDENTITY_CURVE],
    b: [...IDENTITY_CURVE],
  };
}

const CHANNELS: { key: ToneChannel; label: string; color: string }[] = [
  { key: "master", label: "RGB", color: "var(--foreground)" },
  { key: "r", label: "R", color: "#f87171" },
  { key: "g", label: "G", color: "#4ade80" },
  { key: "b", label: "B", color: "#60a5fa" },
];

const clamp01 = (v: number): number => Math.min(1, Math.max(0, v));

/// Monotone cubic slopes (Fritsch–Carlson) over x-sorted points, so the curve is monotone where the
/// data is and never overshoots.
function monotoneSlopes(pts: CurvePoint[]): number[] {
  const n = pts.length;
  if (n < 2) {
    return new Array(n).fill(0);
  }
  const dx: number[] = [];
  const dy: number[] = [];
  const secant: number[] = [];
  for (let i = 0; i < n - 1; i += 1) {
    const hx = pts[i + 1].x - pts[i].x || 1e-6;
    dx.push(hx);
    dy.push(pts[i + 1].y - pts[i].y);
    secant.push(dy[i] / hx);
  }
  const m: number[] = new Array(n).fill(0);
  m[0] = secant[0];
  m[n - 1] = secant[n - 2];
  for (let i = 1; i < n - 1; i += 1) {
    if (secant[i - 1] * secant[i] <= 0) {
      m[i] = 0;
    } else {
      m[i] = (secant[i - 1] + secant[i]) / 2;
    }
  }
  for (let i = 0; i < n - 1; i += 1) {
    if (secant[i] === 0) {
      m[i] = 0;
      m[i + 1] = 0;
      continue;
    }
    const a = m[i] / secant[i];
    const b = m[i + 1] / secant[i];
    const h = Math.hypot(a, b);
    if (h > 3) {
      const t = 3 / h;
      m[i] = t * a * secant[i];
      m[i + 1] = t * b * secant[i];
    }
  }
  return m;
}

/// Evaluate the monotone-cubic curve at `x` (0..1), clamped to the endpoints outside the range.
export function evalCurve(points: CurvePoint[], x: number): number {
  const pts = [...points].sort((a, b) => a.x - b.x);
  if (pts.length === 0) {
    return x;
  }
  if (x <= pts[0].x) {
    return clamp01(pts[0].y);
  }
  if (x >= pts[pts.length - 1].x) {
    return clamp01(pts[pts.length - 1].y);
  }
  const m = monotoneSlopes(pts);
  let i = 0;
  while (i < pts.length - 1 && x > pts[i + 1].x) {
    i += 1;
  }
  const h = pts[i + 1].x - pts[i].x || 1e-6;
  const t = (x - pts[i].x) / h;
  const t2 = t * t;
  const t3 = t2 * t;
  const h00 = 2 * t3 - 3 * t2 + 1;
  const h10 = t3 - 2 * t2 + t;
  const h01 = -2 * t3 + 3 * t2;
  const h11 = t3 - t2;
  const y = h00 * pts[i].y + h10 * h * m[i] + h01 * pts[i + 1].y + h11 * h * m[i + 1];
  return clamp01(y);
}

const VB = 100;

export function ToneCurve({
  channels,
  onChange,
  visibleChannels,
  masterLabel,
  onDragStart,
  onDragEnd,
}: ToneCurveProps) {
  const scrub = useScrubValue<ToneCurveChannels>(channels, onChange);
  const [active, setActive] = useState<ToneChannel>("master");
  const svgRef = useRef<SVGSVGElement>(null);
  const dragIndex = useRef<number | null>(null);

  const points = [...scrub.value[active]].sort((a, b) => a.x - b.x);
  const activeColor = CHANNELS.find((c) => c.key === active)?.color ?? "var(--foreground)";

  const toSvg = (p: CurvePoint): { x: number; y: number } => ({
    x: p.x * VB,
    y: (1 - p.y) * VB,
  });

  const fromClient = (clientX: number, clientY: number): CurvePoint => {
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) {
      return { x: 0, y: 0 };
    }
    return {
      x: clamp01((clientX - rect.left) / rect.width),
      y: clamp01(1 - (clientY - rect.top) / rect.height),
    };
  };

  const commit = (next: CurvePoint[]): void => {
    scrub.set({ ...scrub.value, [active]: next });
  };

  // Sample the curve into an SVG path (the monotone eval), so the drawn line matches the baked LUT.
  const pathFor = (pts: CurvePoint[]): string => {
    const samples = 48;
    let d = "";
    for (let i = 0; i <= samples; i += 1) {
      const x = i / samples;
      const y = evalCurve(pts, x);
      const s = toSvg({ x, y });
      d += `${i === 0 ? "M" : "L"} ${s.x.toFixed(2)} ${s.y.toFixed(2)} `;
    }
    return d.trim();
  };

  const beginPoint = (index: number, e: React.PointerEvent): void => {
    e.preventDefault();
    e.stopPropagation();
    // Capture on the (always-mounted) svg root, not the circle: a control point re-keys as it moves,
    // so a capture on the circle would drop mid-drag.
    svgRef.current?.setPointerCapture(e.pointerId);
    dragIndex.current = index;
    scrub.begin();
    onDragStart?.();
  };

  const movePoint = (e: React.PointerEvent): void => {
    const index = dragIndex.current;
    if (index === null) {
      return;
    }
    const p = fromClient(e.clientX, e.clientY);
    const next = [...points];
    const isEnd = index === 0 || index === next.length - 1;
    // Endpoints keep their x (0 or 1); interior points stay strictly between their neighbours.
    const x = isEnd
      ? next[index].x
      : Math.min(next[index + 1].x - 0.01, Math.max(next[index - 1].x + 0.01, p.x));
    next[index] = { x, y: p.y };
    commit(next);
  };

  const endDrag = (e: React.PointerEvent): void => {
    if (dragIndex.current === null) {
      return;
    }
    svgRef.current?.releasePointerCapture?.(e.pointerId);
    dragIndex.current = null;
    scrub.end();
    onDragEnd?.();
  };

  // A click on empty canvas inserts a control point and immediately drags it.
  const addPoint = (e: React.PointerEvent<SVGSVGElement>): void => {
    if (dragIndex.current !== null) {
      return;
    }
    const p = fromClient(e.clientX, e.clientY);
    const next = [...points, p].sort((a, b) => a.x - b.x);
    const index = next.findIndex((q) => q === p);
    scrub.set({ ...scrub.value, [active]: next });
    e.currentTarget.setPointerCapture(e.pointerId);
    dragIndex.current = index;
    scrub.begin();
    onDragStart?.();
  };

  const removePoint = (index: number, e: React.MouseEvent): void => {
    e.preventDefault();
    if (index === 0 || index === points.length - 1) {
      return;
    }
    onChange({ ...scrub.value, [active]: points.filter((_, i) => i !== index) });
  };

  const reset = (): void => {
    onChange({ ...scrub.value, [active]: [...IDENTITY_CURVE] });
  };

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-1">
        {CHANNELS.filter((channel) =>
          visibleChannels ? visibleChannels.includes(channel.key) : true,
        ).map((c) => (
          <button
            key={c.key}
            type="button"
            onClick={() => setActive(c.key)}
            className={cn(
              "flex-1 rounded-sm border px-1.5 py-0.5 text-[10px] font-medium",
              active === c.key
                ? "border-border bg-background text-foreground"
                : "border-transparent bg-muted text-muted-foreground hover:text-foreground",
            )}
            style={active === c.key ? { color: c.color } : undefined}
          >
            {c.key === "master" && masterLabel ? masterLabel : c.label}
          </button>
        ))}
        <button
          type="button"
          onClick={reset}
          className="rounded-sm border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground hover:text-foreground"
        >
          Reset
        </button>
      </div>
      <svg
        ref={svgRef}
        viewBox={`0 0 ${VB} ${VB}`}
        preserveAspectRatio="none"
        className="h-32 w-full touch-none rounded-sm border border-border bg-card"
        onPointerDown={addPoint}
        onPointerMove={movePoint}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        {[25, 50, 75].map((g) => (
          <g key={g}>
            <line x1={g} y1={0} x2={g} y2={VB} stroke="var(--border)" strokeWidth={0.4} />
            <line x1={0} y1={g} x2={VB} y2={g} stroke="var(--border)" strokeWidth={0.4} />
          </g>
        ))}
        <line
          x1={0}
          y1={VB}
          x2={VB}
          y2={0}
          stroke="var(--muted-foreground)"
          strokeWidth={0.4}
          strokeDasharray="2 2"
          opacity={0.5}
        />
        <path d={pathFor(points)} fill="none" stroke={activeColor} strokeWidth={1.4} />
        {points.map((p, index) => {
          const s = toSvg(p);
          return (
            <circle
              key={`${p.x}:${p.y}`}
              cx={s.x}
              cy={s.y}
              r={2.6}
              fill={activeColor}
              stroke="var(--background)"
              strokeWidth={0.8}
              className="cursor-grab"
              onPointerDown={(e) => beginPoint(index, e)}
              onContextMenu={(e) => removePoint(index, e)}
            />
          );
        })}
      </svg>
    </div>
  );
}
