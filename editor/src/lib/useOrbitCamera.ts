/// The orbit camera shared by every preview pane that drives the `assetPreview` view — the
/// asset editor (model / texture / HDRI subjects) and the material-graph editor's live sphere.
/// Input moves an orbit target (pivot / distance / yaw / pitch); each change streams that pose to
/// the engine, and the engine eases pivot / distance / angles toward it every rendered frame at
/// tau=0.025 (the same ease the fly-cam look and gizmo drag use), sweeping the eye along the arc —
/// so motion stays smooth at the engine's render FPS, not the ~60 Hz control rate, and a fast drag
/// follows the circle instead of cutting a chord across it. One coalesced `set-camera` is in flight
/// at a time (never one call per pointer sample). `exit-asset-preview` restores the engine-stashed
/// camera, so orbiting never dirties the saved `editorCamera`.
import { useCallback, useEffect, useMemo, useRef } from "react";
import type { PointerEvent as ReactPointerEvent, WheelEvent as ReactWheelEvent } from "react";
import { client } from "../control/client";
import { makeCoalescer } from "../control/coalesce";

/// Orbit drag sensitivity (degrees of yaw/pitch per CSS pixel) and zoom factor per wheel notch.
const ORBIT_SENS_DEG_PER_PX = 0.4;
const ZOOM_PER_WHEEL = 1.1;
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

export function cloneOrbit(o: OrbitState): OrbitState {
  return { target: { ...o.target }, distance: o.distance, yaw: o.yaw, pitch: o.pitch };
}

export interface OrbitControls {
  onPointerDown: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onWheel: (e: ReactWheelEvent<HTMLDivElement>) => void;
  /// Snap the orbit to a framed pose (on preview enter) — seeds the local state, no push (the engine
  /// already framed the preview camera on enter-asset-preview).
  setFramed: (o: OrbitState) => void;
}

/// Wire up the orbit for one preview pane. `enableZoom` gates the wheel dolly (a lone framed
/// sphere is orbit-only); `onClick(u, v)` fires on a click (negligible drag) with normalized pane
/// coordinates — used by the asset editor to pick a skeleton joint, omitted where there is nothing to
/// pick.
export function useOrbitCamera(opts: {
  enableZoom: boolean;
  onClick?: (u: number, v: number) => void;
}): OrbitControls {
  const { enableZoom, onClick } = opts;

  // The orbit target the engine eases toward (a ref — input mutates it, no React re-render).
  // framedDistance seeds the zoom bounds so they scale to the subject.
  const orbit = useRef<OrbitState>(cloneOrbit(INITIAL_ORBIT));
  const framedDistance = useRef(INITIAL_ORBIT.distance);
  const dragging = useRef(false);
  const lastPointer = useRef({ x: 0, y: 0 });
  const downPos = useRef({ x: 0, y: 0 });

  // One coalesced set-camera in flight at a time (the serialized wire — never one call per pointer
  // sample). We stream the raw pose with `smooth`; the engine eases its eye toward it per rendered
  // frame, so the ~60 Hz sample stream becomes continuous motion at render FPS.
  const cameraCoalescer = useMemo(
    () =>
      makeCoalescer<OrbitState>({
        throttleMs: 16,
        send: (o) =>
          client
            .setOrbit({ pivot: o.target, distance: o.distance, yaw: o.yaw, pitch: o.pitch })
            .then(() => {}),
      }),
    [],
  );

  // Drop any buffered pose when the pane unmounts, so a late set-camera can't nudge the restored
  // scene camera after exit-asset-preview parks the preview.
  useEffect(() => () => cameraCoalescer.reset(), [cameraCoalescer]);

  const pushOrbit = useCallback(() => {
    cameraCoalescer.push(cloneOrbit(orbit.current));
  }, [cameraCoalescer]);

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
      const o = orbit.current;
      o.yaw += dx * ORBIT_SENS_DEG_PER_PX;
      o.pitch = Math.max(-89, Math.min(89, o.pitch - dy * ORBIT_SENS_DEG_PER_PX));
      pushOrbit();
    },
    [pushOrbit],
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
      const o = orbit.current;
      const minD = Math.max(ZOOM_MIN_ABS, framedDistance.current * ZOOM_MIN_FRAC);
      const maxD = framedDistance.current * ZOOM_MAX_FRAC;
      const next = o.distance * (e.deltaY > 0 ? ZOOM_PER_WHEEL : 1 / ZOOM_PER_WHEEL);
      o.distance = Math.min(maxD, Math.max(minD, next));
      pushOrbit();
    },
    [pushOrbit, enableZoom],
  );

  const setFramed = useCallback((o: OrbitState) => {
    orbit.current = cloneOrbit(o);
    framedDistance.current = o.distance;
  }, []);

  return useMemo(
    () => ({ onPointerDown, onPointerMove, onPointerUp, onWheel, setFramed }),
    [onPointerDown, onPointerMove, onPointerUp, onWheel, setFramed],
  );
}
