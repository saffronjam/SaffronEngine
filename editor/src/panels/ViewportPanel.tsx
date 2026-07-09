/// The viewport region: a transparent div the engine's render shows through (the
/// presenter holds a wayland subsurface glued to this rect, below the webview). It
/// never renders pixels — it owns the screen rectangle and forwards pointer input to
/// the engine over the control plane. A <LoadingOverlay/> sibling covers the region
/// while the renderer is not yet ready.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { client } from "../control/client";
import { makeCoalescer } from "../control/coalesce";
import { useEditorStore } from "../state/store";
import { LoadingOverlay } from "../app/LoadingOverlay";
import { useSubsurfaceBounds } from "../lib/useSubsurfaceBounds";
import { bindingFor } from "../lib/keybindings";
import {
  ASSET_DND_MIME,
  assetIdsFromPayload,
  firstModelAssetId,
  readAssetPayload,
} from "../components/AssetTile";
import { errorText, notify, notifyError } from "../lib/flash";

/// Pointer travel (CSS px) below which a press-release is treated as a click
/// (ray-pick) rather than a gizmo drag.
const DRAG_THRESHOLD_PX = 3;

/// Throttle for streamed fly-cam input while pointer lock is held, in milliseconds.
/// Look deltas accumulate between sends, so nothing is lost to the throttle.
const FLY_STREAM_MS = 16;

/// Throttle for streamed gizmo pointer phases (hover/drag), in milliseconds.
const GIZMO_STREAM_MS = 16;

/// Normalized [0,1] viewport coordinate, (0,0) = top-left.
interface Uv {
  u: number;
  v: number;
}

function clientPointToUv(el: HTMLElement, clientX: number, clientY: number): Uv {
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) {
    return { u: 0, v: 0 };
  }
  const u = (clientX - rect.left) / rect.width;
  const v = (clientY - rect.top) / rect.height;
  return {
    u: Math.min(1, Math.max(0, u)),
    v: Math.min(1, Math.max(0, v)),
  };
}

/// Map a pointer event to {u,v} in [0,1] using the panel's own client rect.
function eventToUv(el: HTMLElement, event: PointerEvent): Uv {
  return clientPointToUv(el, event.clientX, event.clientY);
}

function scriptKeyFromEvent(event: KeyboardEvent): string | null {
  if (event.metaKey) {
    return null;
  }
  if (event.key === " ") {
    return "space";
  }
  if (event.key === "Shift") {
    return "shift";
  }
  if (event.key === "Control") {
    return "control";
  }
  if (event.key === "Alt") {
    return "alt";
  }
  return event.key.toLowerCase();
}

function targetOwnsTextInput(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) {
    return false;
  }
  return target.closest("input, textarea, select, [contenteditable='true']") !== null;
}

export function ViewportPanel() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const attachedRef = useRef(false);
  const setPhase = useEditorStore((s) => s.setPhase);
  const setSelectedId = useEditorStore((s) => s.setSelectedId);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const viewportHidden = useEditorStore((s) => s.viewportHidden);
  const playState = useEditorStore((s) => s.playState);

  // Coalescers stream the hover and drag phases to the engine at >= GIZMO_STREAM_MS
  // apart, buffering only the latest NDC so a burst of pointermove collapses to one
  // in-flight call. Stable across renders.
  const hoverCoalescer = useMemo(
    () =>
      makeCoalescer<Uv>({
        throttleMs: GIZMO_STREAM_MS,
        send: ({ u, v }) => client.gizmoPointer("hover", u * 2 - 1, v * 2 - 1),
      }),
    [],
  );
  const dragCoalescer = useMemo(
    () =>
      makeCoalescer<Uv>({
        throttleMs: GIZMO_STREAM_MS,
        send: ({ u, v }) => client.gizmoPointer("drag", u * 2 - 1, v * 2 - 1),
      }),
    [],
  );
  // Streams the asset-placement preview while dragging an asset over the viewport. Coalesced
  // (one round-trip in flight, latest position wins) so a 60 Hz drag-over burst can't pile up
  // behind the serialized control bridge and starve camera/gizmo input.
  const placementCoalescer = useMemo(
    () =>
      makeCoalescer<{ model: string; u: number; v: number }>({
        throttleMs: GIZMO_STREAM_MS,
        send: ({ model, u, v }) => client.previewAssetPlacement(model, u, v),
      }),
    [],
  );

  // Optimistic post-pick selection: a hit sets store.selectedId immediately so the
  // UI does not wait a full reconcile interval. Empty space deselects.
  const runPick = useCallback(
    async ({ u, v }: Uv): Promise<void> => {
      try {
        const result = await client.pick(u, v);
        if (result.hit && result.id) {
          setSelectedId(result.id);
        } else {
          setSelectedId(null);
        }
      } catch {
        // The engine may be briefly busy; the reconcile poll recovers selection.
      }
    },
    [setSelectedId],
  );

  const firstDraggedModel = useCallback(
    (dt: DataTransfer, preferCatalogDrag: boolean): string | null => {
      const state = useEditorStore.getState();
      if (preferCatalogDrag && state.catalogDrag) {
        const model = firstModelAssetId(state.catalogDrag.assetIds, state.assets);
        if (model) {
          return model;
        }
      }
      const ids = assetIdsFromPayload(readAssetPayload(dt));
      if (ids.length === 0) {
        return null;
      }
      return firstModelAssetId(ids, state.assets);
    },
    [],
  );

  const clearPlacementPreview = useCallback((): void => {
    placementCoalescer.reset();
    void client.clearAssetPlacement().catch(() => {});
  }, [placementCoalescer]);

  useEffect(() => clearPlacementPreview, [clearPlacementPreview]);

  // Readiness: probe the control plane until the engine has booted + bound its
  // socket, then flip the phase. The `engine-phase` events are emitted from the Rust
  // `.setup()` hook BEFORE this webview registers its listener (the shell does not buffer
  // pre-listen events), so the probe — not the event — is the gate. (`cancelled`
  // makes any pending retry a no-op after unmount.)
  useLayoutEffect(() => {
    let cancelled = false;

    const probe = async (): Promise<void> => {
      if (cancelled || attachedRef.current) {
        return;
      }
      try {
        await client.viewportNativeInfo();
      } catch {
        if (cancelled) {
          return;
        }
        setTimeout(() => void probe(), 150);
        return;
      }
      if (cancelled) {
        return;
      }
      attachedRef.current = true;
      setPhase("ready");
    };

    void probe();

    return () => {
      cancelled = true;
    };
  }, [setPhase]);

  // Bounds-sync: keep the Scene view's subsurface glued to the host div on resize / dock-split / layout
  // changes. The shared hook is parameterized by view id — the asset editor's preview pane drives its
  // own "assetPreview" surface through the same hook.
  useSubsurfaceBounds(hostRef, "scene");

  useEffect(() => {
    const pressed = new Set<string>();
    let lastSent = "";

    const send = (): void => {
      const keys = [...pressed].sort();
      const fingerprint = keys.join("\0");
      if (fingerprint === lastSent) {
        return;
      }
      lastSent = fingerprint;
      void client.scriptInput(keys).catch(() => {});
    };

    const clear = (): void => {
      if (pressed.size === 0 && lastSent === "") {
        return;
      }
      pressed.clear();
      send();
    };

    if (playState === "edit") {
      clear();
      return clear;
    }

    const onKeyDown = (event: KeyboardEvent): void => {
      if (targetOwnsTextInput(event.target)) {
        return;
      }
      const key = scriptKeyFromEvent(event);
      if (key === null) {
        return;
      }
      const size = pressed.size;
      pressed.add(key);
      if (pressed.size !== size) {
        send();
      }
    };

    const onKeyUp = (event: KeyboardEvent): void => {
      const key = scriptKeyFromEvent(event);
      if (key !== null && pressed.delete(key)) {
        send();
      }
    };

    const onVisibilityChange = (): void => {
      if (document.visibilityState !== "visible") {
        clear();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", clear);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      clear();
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", clear);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [playState]);

  // RMB fly-cam: hold RMB over the viewport to fly. Pointer lock gives relative
  // mouse deltas (movementX/Y), which accumulate and stream with the WASD/Space/
  // Shift key state over `fly-input`. ESC exits pointer lock natively → fly ends.
  useEffect(() => {
    const el = hostRef.current;
    if (!el) {
      return;
    }

    const keys = { forward: false, back: false, left: false, right: false, up: false, down: false };
    let lookDx = 0;
    let lookDy = 0;
    let flying = false;
    let sendTimer: ReturnType<typeof setTimeout> | null = null;

    const sendState = (active: boolean): void => {
      const dx = lookDx;
      const dy = lookDy;
      lookDx = 0;
      lookDy = 0;
      void client.flyInput({ active, lookDx: dx, lookDy: dy, ...keys }).catch(() => {});
    };

    const scheduleSend = (): void => {
      if (sendTimer !== null) {
        return;
      }
      sendTimer = setTimeout(() => {
        sendTimer = null;
        if (flying) {
          sendState(true);
        }
      }, FLY_STREAM_MS);
    };

    const endFly = (): void => {
      if (!flying) {
        return;
      }
      flying = false;
      if (sendTimer !== null) {
        clearTimeout(sendTimer);
        sendTimer = null;
      }
      for (const key of Object.keys(keys) as (keyof typeof keys)[]) {
        keys[key] = false;
      }
      lookDx = 0;
      lookDy = 0;
      if (document.pointerLockElement === el) {
        document.exitPointerLock();
      }
      sendState(false);
    };

    const onPointerDown = (event: PointerEvent): void => {
      if (event.button !== 2 || flying) {
        return;
      }
      event.preventDefault();
      flying = true;
      el.requestPointerLock();
      sendState(true);
    };

    const onPointerUp = (event: PointerEvent): void => {
      if (event.button === 2) {
        endFly();
      }
    };

    const onPointerMove = (event: PointerEvent): void => {
      if (!flying) {
        return;
      }
      lookDx += event.movementX;
      lookDy += event.movementY;
      scheduleSend();
    };

    // Map a physical key code to a fly direction via the configured (hold-kind)
    // bindings. Read live from the store so a rebind in settings applies without
    // re-running this pointer-lock effect.
    const keyFor = (code: string): keyof typeof keys | null => {
      const overrides = useEditorStore.getState().keyBindings;
      if (code === bindingFor("camera.flyForward", overrides)) {
        return "forward";
      }
      if (code === bindingFor("camera.flyBack", overrides)) {
        return "back";
      }
      if (code === bindingFor("camera.flyLeft", overrides)) {
        return "left";
      }
      if (code === bindingFor("camera.flyRight", overrides)) {
        return "right";
      }
      if (code === bindingFor("camera.flyUp", overrides)) {
        return "up";
      }
      if (code === bindingFor("camera.flyDown", overrides)) {
        return "down";
      }
      return null;
    };

    const onKey =
      (down: boolean) =>
      (event: KeyboardEvent): void => {
        if (!flying) {
          return;
        }
        const key = keyFor(event.code);
        if (!key) {
          return;
        }
        event.preventDefault();
        if (keys[key] !== down) {
          keys[key] = down;
          scheduleSend();
        }
      };

    const onLockChange = (): void => {
      if (flying && document.pointerLockElement !== el) {
        endFly();
      }
    };

    const onContextMenu = (event: Event): void => event.preventDefault();

    const keyDown = onKey(true);
    const keyUp = onKey(false);
    el.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("keydown", keyDown);
    window.addEventListener("keyup", keyUp);
    document.addEventListener("pointerlockchange", onLockChange);
    el.addEventListener("contextmenu", onContextMenu);

    return () => {
      endFly();
      el.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("keydown", keyDown);
      window.removeEventListener("keyup", keyUp);
      document.removeEventListener("pointerlockchange", onLockChange);
      el.removeEventListener("contextmenu", onContextMenu);
    };
  }, []);

  // Pointer interaction: every press sends `begin`; if the pointer then travels
  // past DRAG_THRESHOLD_PX it is a gizmo drag (streamed `drag` + dragActive guard),
  // otherwise the release is a click that ray-picks. Release always sends `end`.
  // A bare move (no button down) streams `hover` so the engine highlights handles.
  useEffect(() => {
    const el = hostRef.current;
    if (!el) {
      return;
    }

    // Press-gesture state, reset on each pointerdown / pointerup.
    let pointerId: number | null = null;
    let startUv: Uv | null = null;
    let startClientX = 0;
    let startClientY = 0;
    let dragging = false;
    // Undo capture for a gizmo manipulation: the selected entity + its Transform before
    // the drag + the active op, recorded as one entry when a drag ends.
    let gizmoGesture: { id: string; prior: object; op: string } | null = null;

    const ndc = (uv: Uv): { x: number; y: number } => ({
      x: uv.u * 2 - 1,
      y: uv.v * 2 - 1,
    });

    const onPointerDown = (event: PointerEvent): void => {
      // Engaging the viewport takes over input: drop focus off the chrome so a
      // lingering-focused control (a just-clicked toolbar button, an open text field)
      // can't swallow the next Space/Enter — e.g. Space re-toggling Play/Pause.
      (document.activeElement as HTMLElement | null)?.blur();
      // Left button only; RMB is the fly-cam (pointer lock) gesture.
      if (event.button !== 0 || pointerId !== null || document.pointerLockElement === el) {
        return;
      }
      pointerId = event.pointerId;
      startUv = eventToUv(el, event);
      startClientX = event.clientX;
      startClientY = event.clientY;
      dragging = false;
      el.setPointerCapture(event.pointerId);
      const { x, y } = ndc(startUv);
      void client.gizmoPointer("begin", x, y).catch(() => {});
      // Snapshot the selected entity's Transform so a drag records one undo entry; a
      // press with no selection captures nothing.
      const store = useEditorStore.getState();
      const components = store.componentsBySelected?.components as
        | Record<string, unknown>
        | undefined;
      const transform = components?.Transform;
      gizmoGesture =
        store.selectedId && transform
          ? {
              id: store.selectedId,
              prior: structuredClone(transform as object),
              op: store.gizmo.op,
            }
          : null;
    };

    const onPointerMove = (event: PointerEvent): void => {
      // While pointer lock is held the fly-cam owns the pointer; client coords are stale.
      if (document.pointerLockElement === el) {
        return;
      }
      const uv = eventToUv(el, event);
      if (pointerId === null) {
        // Hovering (no button down): keep the engine's handle highlight fresh.
        hoverCoalescer.push(uv);
        return;
      }
      if (event.pointerId !== pointerId) {
        return;
      }
      if (!dragging) {
        const moved =
          Math.abs(event.clientX - startClientX) > DRAG_THRESHOLD_PX ||
          Math.abs(event.clientY - startClientY) > DRAG_THRESHOLD_PX;
        if (!moved) {
          return;
        }
        dragging = true;
        setDragActive(true);
      }
      dragCoalescer.push(uv);
    };

    const finishPress = (event: PointerEvent): void => {
      if (pointerId === null || event.pointerId !== pointerId) {
        return;
      }
      const uv = eventToUv(el, event);
      const { x, y } = ndc(uv);
      void client.gizmoPointer("end", x, y).catch(() => {});
      if (el.hasPointerCapture(event.pointerId)) {
        el.releasePointerCapture(event.pointerId);
      }
      const wasDragging = dragging;
      const downUv = startUv;
      const gesture = gizmoGesture;
      pointerId = null;
      startUv = null;
      dragging = false;
      gizmoGesture = null;
      if (wasDragging) {
        // The authoritative transform is committed engine-side on `end`; let the poll
        // resume and reconcile it, then record one undo entry from the settled transform.
        setDragActive(false);
        if (gesture) {
          void client
            .inspect(gesture.id)
            .then((res) => {
              const after = (res.components as Record<string, unknown> | undefined)?.Transform;
              if (after && JSON.stringify(gesture.prior) !== JSON.stringify(after)) {
                useEditorStore.getState().pushEdit(
                  {
                    label: gesture.op,
                    selectionId: gesture.id,
                    undo: () =>
                      client.setTransform(
                        gesture.id,
                        gesture.prior as Parameters<typeof client.setTransform>[1],
                      ),
                    redo: () =>
                      client.setTransform(
                        gesture.id,
                        after as Parameters<typeof client.setTransform>[1],
                      ),
                  },
                  "scene",
                );
              }
            })
            .catch(() => {});
        }
      } else if (downUv) {
        // No drag: a plain left-click → ray-pick at the press location.
        void runPick(downUv);
      }
    };

    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("pointermove", onPointerMove);
    el.addEventListener("pointerup", finishPress);
    el.addEventListener("pointercancel", finishPress);

    return () => {
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("pointermove", onPointerMove);
      el.removeEventListener("pointerup", finishPress);
      el.removeEventListener("pointercancel", finishPress);
      if (pointerId !== null && el.hasPointerCapture(pointerId)) {
        el.releasePointerCapture(pointerId);
      }
      // Drop any drag guard the gesture left set.
      if (dragging) {
        setDragActive(false);
      }
    };
  }, [hoverCoalescer, dragCoalescer, runPick, setDragActive]);

  // h-full/w-full (not flex-1): the panel's content div is block-level, so flex-1
  // would be inert here — the viewport must fill its panel rect by explicit size.
  // Transparent while live (the hole down to the subsurface); opaque while parked so
  // a modal over the region does not show the desktop through the window.
  return (
    <div
      data-viewport-drop-target="true"
      className={`relative h-full w-full overflow-hidden ${
        viewportHidden ? "bg-background" : "bg-transparent"
      }`}
      // Asset drags carry `application/x-sa-asset`; model assets stream a transient placement
      // preview into the engine while hovering and commit only on drop.
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes(ASSET_DND_MIME)) {
          return;
        }
        e.preventDefault();
        e.dataTransfer.dropEffect = "copy";
        const model = firstDraggedModel(e.dataTransfer, true);
        if (!model) {
          return;
        }
        const uv = clientPointToUv(e.currentTarget, e.clientX, e.clientY);
        placementCoalescer.push({ model, u: uv.u, v: uv.v });
      }}
      onDragLeave={(e) => {
        const next = e.relatedTarget;
        if (next instanceof Node && e.currentTarget.contains(next)) {
          return;
        }
        clearPlacementPreview();
      }}
      onDrop={(e) => {
        const model = firstDraggedModel(e.dataTransfer, false);
        if (!model) {
          clearPlacementPreview();
          return;
        }
        e.preventDefault();
        // Stop the coalesced stream so no late preview fires after the commit, then place the
        // final position and commit as one ordered chain.
        placementCoalescer.reset();
        const uv = clientPointToUv(e.currentTarget, e.clientX, e.clientY);
        void client
          .previewAssetPlacement(model, uv.u, uv.v)
          .then(() => client.commitAssetPlacement())
          .then(() => notify("Added to scene"))
          .catch((err: unknown) => {
            clearPlacementPreview();
            notifyError(errorText(err));
          });
      }}
    >
      <div ref={hostRef} className="viewport-host" />
      <LoadingOverlay />
    </div>
  );
}
