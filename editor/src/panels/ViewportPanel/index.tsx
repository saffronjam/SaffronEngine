/// The viewport region: a transparent div the engine's render shows through (the
/// presenter holds a wayland subsurface glued to this rect, below the webview). It
/// never renders pixels — it owns the screen rectangle and forwards pointer input to
/// the engine over the control plane. A <LoadingOverlay/> sibling covers the region
/// while the renderer is not yet ready.
import { useCallback, useEffect, useMemo, useRef } from "react";
import { client } from "../../control/client";
import { makeCoalescer } from "../../control/coalesce";
import { useEditorStore } from "../../state/store";
import { LoadingOverlay } from "../../app/LoadingOverlay";
import { VegetationViewportToolbar } from "../VegetationViewportToolbar";
import { mutationRecord } from "../vegetationPlanting";
import { type BrushStamp } from "../vegetationPainting";
import { findPanelLeaf } from "../../state/dockLayout";
import { useSubsurfaceBounds } from "../../lib/useSubsurfaceBounds";
import {
  ASSET_DND_MIME,
  assetIdsFromPayload,
  firstModelAssetId,
  readAssetPayload,
} from "../../components/AssetTile";
import { errorText, notify, notifyError } from "../../lib/flash";
import { useEngineAttachProbe } from "./useEngineAttachProbe";
import { useFlyCamera } from "./useFlyCamera";
import { useScriptInputForwarding } from "./useScriptInputForwarding";
import { pinAt, plantAt, reapplyCommit, strokeCommit } from "./viewportVegetation";
import {
  DRAG_THRESHOLD_PX,
  GIZMO_STREAM_MS,
  clientPointToUv,
  eventToUv,
  type Uv,
} from "./viewportInput";

export function ViewportPanel() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  // RMB fly-cam active: the shell has the cursor natively locked (CEF OSR can't do DOM pointer lock).
  // Shared between the fly effect (owner) and the gizmo effect (which stands down while it's set).
  const flyingRef = useRef(false);
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
        const setPlant = useEditorStore.getState().setVegetationSelectedPlant;
        if (result.hit && result.id) {
          setPlant(null);
          setSelectedId(result.id);
        } else if (result.hit && result.plant) {
          // A macro plant: nonpersistent vegetation selection beside the entity one.
          setPlant(result.plant);
          setSelectedId(null);
        } else {
          setPlant(null);
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

  useEngineAttachProbe();

  // Bounds-sync: keep the Scene view's subsurface glued to the host div on resize / dock-split / layout
  // changes. The shared hook is parameterized by view id — the asset editor's preview pane drives its
  // own "assetPreview" surface through the same hook.
  useSubsurfaceBounds(hostRef, "scene");

  useScriptInputForwarding(playState);
  useFlyCamera(hostRef, flyingRef);

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
    // A vegetation brush stroke in flight: stamps accumulate along the drag (one
    // serialized pick at a time; new stamps land at brush-spacing intervals) and
    // the release commits them as one transaction. Non-null suppresses the gizmo
    // pointer stream for the whole press.
    let vegStroke: {
      stamps: BrushStamp[];
      last: [number, number, number] | null;
      picking: boolean;
      sign: 1 | -1;
      /// A reapply stroke recooks its region instead of committing tile edits.
      reapply: boolean;
      /// Latest pointer pressure (0..1; mice report a constant while pressed).
      pressure: number;
    } | null = null;

    // A macro-plant move drag in flight: the press picked the already-selected
    // plant; samples stream transform-override mutations preserving the plant's
    // orientation/scale, and the release records one undoable edit.
    let plantDrag: {
      plant: string;
      cell: { coordinates: [string, string, string]; level: number } | null;
      prior: {
        ticks: [string, string, string];
        orientation: [number, number, number, number];
        scaleBits: [number, number, number];
      } | null;
      current: [string, string, string] | null;
      picking: boolean;
      confirmed: boolean;
    } | null = null;

    /// Applies one move sample: picks the ground and streams a transform-override
    /// with the captured orientation/scale. One pick+mutate in flight at a time.
    const samplePlantDrag = (uv: Uv): void => {
      const drag = plantDrag;
      if (!drag || drag.picking || !drag.prior || !drag.cell) {
        return;
      }
      drag.picking = true;
      void (async () => {
        try {
          const picked = await client.pick(uv.u, uv.v);
          const position = picked.position;
          if (!position || !drag.prior || !drag.cell) {
            return;
          }
          const ticks: [string, string, string] = [
            String(Math.round(position[0] * 4096)),
            String(Math.round(position[1] * 4096)),
            String(Math.round(position[2] * 4096)),
          ];
          await client.vegetationMutate([
            mutationRecord(drag.cell, {
              kind: "transform-override",
              plant: drag.plant,
              transform: {
                globalTicks: ticks,
                orientation: drag.prior.orientation,
                scaleBits: drag.prior.scaleBits,
              },
            }),
          ]);
          drag.current = ticks;
        } catch {
          // Busy engine; the next move retries.
        } finally {
          drag.picking = false;
        }
      })();
    };

    /// The stroke mode when the current tool routes this press to the brush:
    /// paint/erase need an armed paintable layer; reapply needs any active layer
    /// (it recooks the touched region without an authored edit).
    const strokeMode = (): { sign: 1 | -1; reapply: boolean } | null => {
      const state = useEditorStore.getState();
      const target = state.vegetationActiveLayer;
      if (
        !target ||
        findPanelLeaf(state.dockLayouts.scene, "vegetation") === null ||
        state.playState !== "edit"
      ) {
        return null;
      }
      if (state.vegetationTool === "reapply") {
        return { sign: 1, reapply: true };
      }
      const sign =
        state.vegetationTool === "paint" ? 1 : state.vegetationTool === "erase" ? -1 : null;
      if (sign === null || target.channel === null || target.locked) {
        return null;
      }
      return { sign, reapply: false };
    };

    /// Picks the ground under `uv` and appends a stamp once the pointer has
    /// travelled the brush spacing. One pick in flight; extra samples drop.
    /// The brush's projection re-lands a "down" sample by a straight-down cast
    /// above the view hit, and its slope limit drops samples on steep surfaces.
    const sampleStroke = (uv: Uv): void => {
      const stroke = vegStroke;
      if (!stroke || stroke.picking) {
        return;
      }
      stroke.picking = true;
      void (async () => {
        try {
          const picked = await client.pick(uv.u, uv.v);
          let position = picked.position ?? null;
          let normal = picked.normal ?? null;
          const brush = useEditorStore.getState().vegetationBrush;
          if (position && brush.projection === "down") {
            const cast = await client.querySurfaceRay({
              originM: [position[0], position[1] + 100, position[2]],
              direction: [0, -1, 0],
            });
            position = cast.position ?? null;
            normal = cast.normal ?? null;
          }
          if (!position) {
            return;
          }
          if (normal && brush.maxSlopeDeg < 90 && !stroke.reapply) {
            const tilt = (Math.acos(Math.min(1, Math.max(-1, normal[1]))) * 180) / Math.PI;
            if (tilt > brush.maxSlopeDeg) {
              return;
            }
          }
          if (
            stroke.last &&
            Math.hypot(
              position[0] - stroke.last[0],
              position[1] - stroke.last[1],
              position[2] - stroke.last[2],
            ) < brush.spacing
          ) {
            return;
          }
          stroke.last = position;
          stroke.stamps.push({
            position,
            radius: brush.radius,
            falloff: brush.falloff,
            pressure: stroke.pressure,
            sign: stroke.sign,
          });
        } catch {
          // The engine may be briefly busy; the next move retries.
        } finally {
          stroke.picking = false;
        }
      })();
    };

    const ndc = (uv: Uv): { x: number; y: number } => ({
      x: uv.u * 2 - 1,
      y: uv.v * 2 - 1,
    });

    const onPointerDown = (event: PointerEvent): void => {
      // Engaging the viewport takes over input: drop focus off the chrome so a
      // lingering-focused control (a just-clicked toolbar button, an open text field)
      // can't swallow the next Space/Enter — e.g. Space re-toggling Play/Pause.
      (document.activeElement as HTMLElement | null)?.blur();
      // Left button only; RMB is the fly-cam gesture, which owns the (natively locked) pointer.
      if (event.button !== 0 || pointerId !== null || flyingRef.current) {
        return;
      }
      pointerId = event.pointerId;
      startUv = eventToUv(el, event);
      startClientX = event.clientX;
      startClientY = event.clientY;
      dragging = false;
      el.setPointerCapture(event.pointerId);
      // A paint/erase/reapply press with an armed layer is a brush stroke: it owns
      // the whole press (no gizmo stream, no transform snapshot) and samples its
      // first stamp at the press point.
      const mode = strokeMode();
      if (mode !== null) {
        vegStroke = {
          stamps: [],
          last: null,
          picking: false,
          sign: mode.sign,
          reapply: mode.reapply,
          pressure: event.pressure > 0 ? event.pressure : 1,
        };
        gizmoGesture = null;
        sampleStroke(startUv);
        return;
      }
      // A press with the Select tool while a macro plant is selected may become a
      // plant move: confirm asynchronously (the press must pick that same plant)
      // and capture its transform for orientation/scale-preserving overrides. An
      // unconfirmed press stays an ordinary click.
      {
        const state = useEditorStore.getState();
        if (
          state.vegetationTool === "select" &&
          state.vegetationSelectedPlant !== null &&
          findPanelLeaf(state.dockLayouts.scene, "vegetation") !== null &&
          state.playState === "edit"
        ) {
          const plant = state.vegetationSelectedPlant;
          const drag = {
            plant,
            cell: null,
            prior: null,
            current: null,
            picking: true,
            confirmed: false,
          } as NonNullable<typeof plantDrag>;
          plantDrag = drag;
          const pressUv = startUv;
          void (async () => {
            try {
              const picked = await client.pick(pressUv.u, pressUv.v);
              if (picked.plant !== plant) {
                return;
              }
              const inspected = await client.vegetationRuntimeInspect(plant);
              const resident = inspected.resident;
              if (!resident) {
                return;
              }
              drag.cell = resident.cell;
              drag.prior = {
                ticks: resident.positionTicks,
                orientation: resident.orientation,
                scaleBits: resident.scaleBits,
              };
              drag.confirmed = true;
            } catch {
              // Busy engine; the press degrades to a click.
            } finally {
              drag.picking = false;
            }
          })();
        }
      }
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
      // While the fly-cam owns the (locked) pointer, DOM client coords are stale — stand down.
      if (flyingRef.current) {
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
      if (vegStroke) {
        vegStroke.pressure = event.pressure > 0 ? event.pressure : 1;
        sampleStroke(uv);
        return;
      }
      if (plantDrag) {
        if (plantDrag.confirmed) {
          samplePlantDrag(uv);
        }
        return;
      }
      dragCoalescer.push(uv);
    };

    const finishPress = (event: PointerEvent): void => {
      if (pointerId === null || event.pointerId !== pointerId) {
        return;
      }
      const uv = eventToUv(el, event);
      if (vegStroke) {
        // The stroke owns this press: commit the captured stamps (waiting out any
        // pick still in flight) and skip the gizmo/pick paths entirely.
        const stroke = vegStroke;
        vegStroke = null;
        pointerId = null;
        startUv = null;
        if (el.hasPointerCapture(event.pointerId)) {
          el.releasePointerCapture(event.pointerId);
        }
        if (dragging) {
          dragging = false;
          setDragActive(false);
        }
        const waitForPick = (): Promise<void> =>
          stroke.picking
            ? new Promise((resolve) => setTimeout(resolve, 32)).then(waitForPick)
            : Promise.resolve();
        void waitForPick().then(() =>
          stroke.reapply ? reapplyCommit(stroke.stamps) : strokeCommit(stroke.stamps),
        );
        return;
      }
      if (plantDrag) {
        const drag = plantDrag;
        plantDrag = null;
        const wasPlantDragging = dragging;
        const downUv = startUv;
        pointerId = null;
        startUv = null;
        dragging = false;
        gizmoGesture = null;
        if (el.hasPointerCapture(event.pointerId)) {
          el.releasePointerCapture(event.pointerId);
        }
        if (wasPlantDragging) {
          setDragActive(false);
        }
        const waitForSample = (): Promise<void> =>
          drag.picking
            ? new Promise((resolve) => setTimeout(resolve, 32)).then(waitForSample)
            : Promise.resolve();
        void waitForSample().then(() => {
          if (drag.confirmed && drag.current && drag.prior && drag.cell) {
            // A confirmed move: one undoable edit restoring the captured transform.
            const { cell, plant, prior } = drag;
            const current = drag.current;
            const override = (ticks: [string, string, string]) => () =>
              client.vegetationMutate([
                mutationRecord(cell, {
                  kind: "transform-override",
                  plant,
                  transform: {
                    globalTicks: ticks,
                    orientation: prior.orientation,
                    scaleBits: prior.scaleBits,
                  },
                }),
              ]);
            useEditorStore.getState().pushEdit({
              label: "Move plant",
              undo: override(prior.ticks),
              redo: override(current),
            });
          } else if (!wasPlantDragging && downUv) {
            // Never confirmed and never dragged: an ordinary selection click.
            void runPick(downUv);
          }
        });
        return;
      }
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
        // No drag: a plain left-click. The vegetation Single/Anchor tool plants at
        // the picked ground; every other case ray-picks selection.
        const state = useEditorStore.getState();
        const vegetationMode =
          findPanelLeaf(state.dockLayouts.scene, "vegetation") !== null &&
          state.playState === "edit";
        if (
          vegetationMode &&
          state.vegetationTool === "single" &&
          state.vegetationSpecies.size > 0
        ) {
          void plantAt(downUv);
        } else if (vegetationMode && state.vegetationTool === "pin") {
          void pinAt(downUv);
        } else {
          void runPick(downUv);
        }
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
      <VegetationViewportToolbar />
      <LoadingOverlay />
    </div>
  );
}
