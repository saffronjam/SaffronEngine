/// The eased orbit camera shared by every preview pane that drives the `assetPreview` view — the
/// asset editor (model / texture / HDRI subjects) and the material-graph editor's live sphere.
/// Input moves a target; a rAF loop drains current→target with the engine's tau (refs only, no React
/// re-render), so a slight lag reads as smooth motion, and one coalesced `set-camera` is in flight at
/// a time (never one call per frame). `exit-asset-preview` restores the engine-stashed camera, so
/// orbiting never dirties the saved `editorCamera`.
import { useCallback, useEffect, useMemo, useRef } from "react";
import type { PointerEvent as ReactPointerEvent, WheelEvent as ReactWheelEvent } from "react";
import { client } from "../control/client";
import { makeCoalescer } from "../control/coalesce";

/// Orbit drag sensitivity (degrees of yaw/pitch per CSS pixel) and zoom factor per wheel notch.
const ORBIT_SENS_DEG_PER_PX = 0.4;
const ZOOM_PER_WHEEL = 1.1;
/// Orbit easing: drain current→target each frame at this time constant (mirrors the engine's tau=0.025
/// gizmo/edit smoothing); stop when within the epsilons. The distance/target epsilon is a fraction of
/// the live distance (an absolute epsilon would never settle a tiny model and would over-shoot a large one).
const ORBIT_TAU_S = 0.025;
const ORBIT_EPS_DEG = 0.01;
const ORBIT_EPS_DIST_FRAC = 0.0005;
/// Zoom bounds relative to the framed distance, so a small model can be dollied in close while a
/// ceiling stops it flying away (scaling the floor to the model lets a tiny one zoom past a fixed limit).
const ZOOM_MIN_FRAC = 0.02;
const ZOOM_MAX_FRAC = 8;
const ZOOM_MIN_ABS = 0.001;
/// A pointer-up within this many pixels of pointer-down is a click (e.g. a joint pick), not an orbit drag.
const CLICK_SLOP_PX = 4;

export interface OrbitState {
  target: { x: number; y: number; z: number };
  distance: number;
  yaw: number;
  pitch: number;
}

const INITIAL_ORBIT: OrbitState = {
  target: { x: 0, y: 0, z: 0 },
  distance: 5,
  yaw: -37,
  pitch: -29,
};

/// The engine's fly-cam forward basis from yaw/pitch (mirrors sceneEditCameraForward), so the editor's
/// orbit reconstructs the eye as target - forward * distance.
function forwardFromYawPitch(
  yawDeg: number,
  pitchDeg: number,
): { x: number; y: number; z: number } {
  const yaw = (yawDeg * Math.PI) / 180;
  const pitch = (pitchDeg * Math.PI) / 180;
  return {
    x: Math.cos(pitch) * Math.sin(yaw),
    y: Math.sin(pitch),
    z: -Math.cos(pitch) * Math.cos(yaw),
  };
}

export function cloneOrbit(o: OrbitState): OrbitState {
  return { target: { ...o.target }, distance: o.distance, yaw: o.yaw, pitch: o.pitch };
}

export interface OrbitControls {
  onPointerDown: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onWheel: (e: ReactWheelEvent<HTMLDivElement>) => void;
  /// Snap the orbit to a framed pose (on preview enter) — seeds both target and current, no ease, no push.
  setFramed: (o: OrbitState) => void;
}

/// Wire up the eased orbit for one preview pane. `enableZoom` gates the wheel dolly (a lone framed
/// sphere is orbit-only); `onClick(u, v)` fires on a click (negligible drag) with normalized pane
/// coordinates — used by the asset editor to pick a skeleton joint, omitted where there is nothing to
/// pick.
export function useOrbitCamera(opts: {
  enableZoom: boolean;
  onClick?: (u: number, v: number) => void;
}): OrbitControls {
  const { enableZoom, onClick } = opts;

  // Both refs (the rAF loop must not re-render). framedDistance seeds the zoom bounds so they scale
  // to the subject.
  const targetOrbit = useRef<OrbitState>(cloneOrbit(INITIAL_ORBIT));
  const currentOrbit = useRef<OrbitState>(cloneOrbit(INITIAL_ORBIT));
  const framedDistance = useRef(INITIAL_ORBIT.distance);
  const rafId = useRef<number | null>(null);
  const lastFrameTs = useRef(0);
  const dragging = useRef(false);
  const lastPointer = useRef({ x: 0, y: 0 });
  const downPos = useRef({ x: 0, y: 0 });

  // One coalesced set-camera in flight at a time (the serialized wire — never one call per frame tick).
  const cameraCoalescer = useMemo(
    () =>
      makeCoalescer<OrbitState>({
        throttleMs: 16,
        send: (o) => {
          const f = forwardFromYawPitch(o.yaw, o.pitch);
          return client
            .setCamera({
              position: {
                x: o.target.x - f.x * o.distance,
                y: o.target.y - f.y * o.distance,
                z: o.target.z - f.z * o.distance,
              },
              yaw: o.yaw,
              pitch: o.pitch,
            })
            .then(() => {});
        },
      }),
    [],
  );

  // Ease current→target one frame, push the eased camera, and either re-arm or settle. Refs only.
  const tickOrbit = useCallback(() => {
    const now = performance.now();
    const dt = lastFrameTs.current ? (now - lastFrameTs.current) / 1000 : 0;
    lastFrameTs.current = now;
    const t = targetOrbit.current;
    const c = currentOrbit.current;
    const alpha = 1 - Math.exp(-dt / ORBIT_TAU_S);
    c.yaw += (t.yaw - c.yaw) * alpha;
    c.pitch += (t.pitch - c.pitch) * alpha;
    c.distance += (t.distance - c.distance) * alpha;
    c.target.x += (t.target.x - c.target.x) * alpha;
    c.target.y += (t.target.y - c.target.y) * alpha;
    c.target.z += (t.target.z - c.target.z) * alpha;
    cameraCoalescer.push(cloneOrbit(c));

    const distEps = ORBIT_EPS_DIST_FRAC * Math.max(c.distance, 1e-4);
    const settled =
      Math.abs(t.yaw - c.yaw) < ORBIT_EPS_DEG &&
      Math.abs(t.pitch - c.pitch) < ORBIT_EPS_DEG &&
      Math.abs(t.distance - c.distance) < distEps &&
      Math.abs(t.target.x - c.target.x) < distEps &&
      Math.abs(t.target.y - c.target.y) < distEps &&
      Math.abs(t.target.z - c.target.z) < distEps;
    if (settled) {
      currentOrbit.current = cloneOrbit(t);
      cameraCoalescer.push(cloneOrbit(t)); // land exactly on the target
      rafId.current = null;
      lastFrameTs.current = 0;
      return;
    }
    rafId.current = requestAnimationFrame(tickOrbit);
  }, [cameraCoalescer]);

  const ensureOrbitLoop = useCallback(() => {
    if (rafId.current === null) {
      lastFrameTs.current = 0;
      rafId.current = requestAnimationFrame(tickOrbit);
    }
  }, [tickOrbit]);

  // Cancel any in-flight ease when the pane unmounts.
  useEffect(
    () => () => {
      if (rafId.current !== null) {
        cancelAnimationFrame(rafId.current);
        rafId.current = null;
      }
    },
    [],
  );

  const onPointerDown = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) {
      return;
    }
    dragging.current = true;
    lastPointer.current = { x: e.clientX, y: e.clientY };
    downPos.current = { x: e.clientX, y: e.clientY };
    e.currentTarget.setPointerCapture(e.pointerId);
  }, []);

  const onPointerMove = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => {
      if (!dragging.current) {
        return;
      }
      const dx = e.clientX - lastPointer.current.x;
      const dy = e.clientY - lastPointer.current.y;
      lastPointer.current = { x: e.clientX, y: e.clientY };
      const o = targetOrbit.current;
      o.yaw += dx * ORBIT_SENS_DEG_PER_PX;
      o.pitch = Math.max(-89, Math.min(89, o.pitch - dy * ORBIT_SENS_DEG_PER_PX));
      ensureOrbitLoop();
    },
    [ensureOrbitLoop],
  );

  const onPointerUp = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => {
      dragging.current = false;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
      // A click (negligible movement) forwards normalized pane coordinates to the pane's picker.
      if (
        !onClick ||
        Math.hypot(e.clientX - downPos.current.x, e.clientY - downPos.current.y) >= CLICK_SLOP_PX
      ) {
        return;
      }
      const rect = e.currentTarget.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) {
        return;
      }
      const u = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
      const v = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
      onClick(u, v);
    },
    [onClick],
  );

  const onWheel = useCallback(
    (e: ReactWheelEvent<HTMLDivElement>) => {
      // A lone framed sphere is orbit-only: the wheel does not dolly (the "pan around the sphere, no
      // zoom" interaction). Subjects with depth to explore (a model, the HDRI three-ball rig) enable it.
      if (!enableZoom) {
        return;
      }
      const o = targetOrbit.current;
      const minD = Math.max(ZOOM_MIN_ABS, framedDistance.current * ZOOM_MIN_FRAC);
      const maxD = framedDistance.current * ZOOM_MAX_FRAC;
      const next = o.distance * (e.deltaY > 0 ? ZOOM_PER_WHEEL : 1 / ZOOM_PER_WHEEL);
      o.distance = Math.min(maxD, Math.max(minD, next));
      ensureOrbitLoop();
    },
    [ensureOrbitLoop, enableZoom],
  );

  const setFramed = useCallback((o: OrbitState) => {
    targetOrbit.current = cloneOrbit(o);
    currentOrbit.current = cloneOrbit(o);
    framedDistance.current = o.distance;
  }, []);

  return useMemo(
    () => ({ onPointerDown, onPointerMove, onPointerUp, onWheel, setFramed }),
    [onPointerDown, onPointerMove, onPointerUp, onWheel, setFramed],
  );
}
