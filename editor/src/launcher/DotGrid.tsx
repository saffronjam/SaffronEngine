/// The launcher's animated backdrop: a regular dot grid displaced by a slowly drifting noise
/// field, so the whole surface sways like grass in a light wind. Pure canvas — no engine, no
/// external assets. Capped at 30 fps, static under `prefers-reduced-motion`, and unmounted with
/// the launcher, so it costs nothing while editing.
import { useEffect, useRef } from "react";

const FRAME_MS = 1000 / 30;
/// Grid pitch in CSS px.
const SPACING = 26;
/// Peak displacement in CSS px — a fraction of the pitch, so the grid reads as swaying, not noise.
const AMPLITUDE = 13;
/// Spatial frequency of the flow field (per CSS px): low, so bands span hundreds of px.
const FIELD_SCALE = 1 / 340;
/// Field drift per second — the sway is clearly alive without ever hurrying.
const DRIFT = 0.12;

/// Deterministic per-lattice-point hash in [0, 1).
function latticeHash(ix: number, iy: number): number {
  let h = (ix * 374761393 + iy * 668265263) | 0;
  h = Math.imul(h ^ (h >>> 13), 1274126177);
  return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
}

function smooth(t: number): number {
  return t * t * (3 - 2 * t);
}

/// Bilinear value noise in [0, 1).
function noise2(x: number, y: number): number {
  const ix = Math.floor(x);
  const iy = Math.floor(y);
  const fx = smooth(x - ix);
  const fy = smooth(y - iy);
  const a = latticeHash(ix, iy);
  const b = latticeHash(ix + 1, iy);
  const c = latticeHash(ix, iy + 1);
  const d = latticeHash(ix + 1, iy + 1);
  return a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy;
}

/// Two-octave value noise in [0, 1).
function fbm(x: number, y: number): number {
  return noise2(x, y) * 0.667 + noise2(x * 2.13 + 71.7, y * 2.13 + 19.3) * 0.333;
}

export function DotGrid() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const parent = canvas?.parentElement;
    if (!canvas || !parent) {
      return;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      return;
    }

    let width = 0;
    let height = 0;
    let dpr = 1;
    const resize = (): void => {
      dpr = window.devicePixelRatio || 1;
      width = parent.clientWidth;
      height = parent.clientHeight;
      canvas.width = Math.max(1, Math.round(width * dpr));
      canvas.height = Math.max(1, Math.round(height * dpr));
    };
    resize();
    const observer = new ResizeObserver(() => {
      resize();
      drawFrame(lastT);
    });
    observer.observe(parent);

    // The dots inherit the theme's foreground; per-dot intensity rides on globalAlpha.
    const dotColor = getComputedStyle(canvas).color;

    let lastT = 0;
    const drawFrame = (t: number): void => {
      lastT = t;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);
      ctx.fillStyle = dotColor;
      // Overscan one pitch so displaced edge dots never pop in and out.
      const cols = Math.ceil(width / SPACING) + 2;
      const rows = Math.ceil(height / SPACING) + 2;
      for (let gy = 0; gy < rows; gy++) {
        for (let gx = 0; gx < cols; gx++) {
          const x = (gx - 0.5) * SPACING;
          const y = (gy - 0.5) * SPACING;
          const fx = x * FIELD_SCALE;
          const fy = y * FIELD_SCALE;
          // One field steers, one modulates: direction bands drift one way, intensity
          // bands another, which is what makes it read as wind rather than jitter.
          const steer = fbm(fx + t, fy + t * 0.31);
          const gain = fbm(fx * 1.4 - t * 0.73 + 40.7, fy * 1.4 + 13.3);
          const angle = steer * Math.PI * 4;
          const push = AMPLITUDE * (0.3 + 0.7 * gain);
          const px = x + Math.cos(angle) * push;
          const py = y + Math.sin(angle) * push;
          ctx.globalAlpha = 0.05 + 0.4 * gain * gain;
          ctx.beginPath();
          ctx.arc(px, py, 0.9 + 1.5 * gain, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      ctx.globalAlpha = 1;
    };

    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reducedMotion) {
      drawFrame(0);
      return () => observer.disconnect();
    }

    let raf = 0;
    let lastFrame = 0;
    const tick = (now: number): void => {
      raf = requestAnimationFrame(tick);
      if (now - lastFrame < FRAME_MS) {
        return;
      }
      lastFrame = now;
      drawFrame((now / 1000) * DRIFT);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="pointer-events-none absolute inset-0 text-foreground"
      aria-hidden="true"
    />
  );
}
