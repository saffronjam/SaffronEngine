/// The Vegetation mode's viewport press gestures: which gesture the active tool begins, how each
/// one samples the pointer, and what it commits on release. The panel owns the pointer plumbing;
/// this owns what a vegetation press *means*.
import { client } from "../../control/client";
import { findPanelLeaf } from "../../state/dockLayout";
import { useEditorStore, type VegetationPoint } from "../../state/store";
import { isRuntimeTool, isStrokeTool } from "../vegetationTools";
import type { BrushBlend, BrushStamp } from "../vegetationPainting";
import {
  fillAt,
  lassoSelect,
  pinAt,
  plantAt,
  promoteAt,
  reapplyCommit,
  splinePointAt,
  strokeCommit,
  volumeCommit,
} from "./viewportVegetation";
import type { Uv } from "./viewportInput";

/// Ground samples one lasso release traces. The picks are serialized through the control bridge, so
/// the ring is resampled to this many points however long the drawn path was.
const LASSO_GROUND_SAMPLES = 48;
/// Screen-space spacing between captured lasso path points, as a fraction of the viewport.
const LASSO_PATH_SPACING = 0.01;

/// A vegetation gesture in flight for the current press.
export type VegetationPress =
  | {
      kind: "stroke";
      stamps: BrushStamp[];
      last: VegetationPoint | null;
      picking: boolean;
      blend: BrushBlend;
      /// A reapply stroke recooks its region instead of committing tile edits.
      reapply: boolean;
      /// Latest pointer pressure (0..1; mice report a constant while pressed).
      pressure: number;
    }
  | { kind: "lasso"; path: Uv[] }
  | { kind: "volume"; start: Uv };

/// Whether the Vegetation mode owns the viewport at all: its dock panel is open. Authoring
/// gestures additionally need edit mode; the runtime tools stay live while the world plays.
export function vegetationModeActive(): boolean {
  return findPanelLeaf(useEditorStore.getState().dockLayouts.scene, "vegetation") !== null;
}

/// Whether the active tool's gesture may run right now.
function toolArmed(): boolean {
  const state = useEditorStore.getState();
  return (
    vegetationModeActive() && (state.playState === "edit" || isRuntimeTool(state.vegetationTool))
  );
}

/// The blend a stroke tool writes with, or null when the tool is not a stroke tool or the active
/// layer cannot take the stroke.
function strokeBlend(): { blend: BrushBlend; reapply: boolean } | null {
  const state = useEditorStore.getState();
  const target = state.vegetationActiveLayer;
  const tool = state.vegetationTool;
  if (!target || !isStrokeTool(tool)) {
    return null;
  }
  if (tool === "reapply") {
    return { blend: { kind: "add", sign: 1 }, reapply: true };
  }
  if (target.channel === null || target.locked) {
    return null;
  }
  // Exclude writes the layer's signed-blocker slot; the density/scalar-field tools write the field
  // slot. A tool aimed at the wrong layer kind writes nothing rather than the wrong tile.
  if ((tool === "exclude") !== (target.slot === "blocker")) {
    return null;
  }
  const blend: BrushBlend =
    tool === "density"
      ? { kind: "level", level: state.vegetationBrush.density }
      : { kind: "add", sign: tool === "erase" ? -1 : 1 };
  return { blend, reapply: false };
}

/// The gesture a press with the active tool begins, or null when the press is an ordinary
/// viewport press (gizmo drag, selection click).
export function beginVegetationPress(uv: Uv, pressure: number): VegetationPress | null {
  if (!toolArmed()) {
    return null;
  }
  const tool = useEditorStore.getState().vegetationTool;
  if (tool === "lasso") {
    return { kind: "lasso", path: [uv] };
  }
  if (tool === "volume") {
    return { kind: "volume", start: uv };
  }
  const mode = strokeBlend();
  if (mode === null) {
    return null;
  }
  const press: VegetationPress = {
    kind: "stroke",
    stamps: [],
    last: null,
    picking: false,
    blend: mode.blend,
    reapply: mode.reapply,
    pressure: pressure > 0 ? pressure : 1,
  };
  sampleStroke(press, uv);
  return press;
}

/// Picks the ground under `uv` and appends a stamp once the pointer has travelled the brush
/// spacing. One pick in flight; extra samples drop. The brush's projection re-lands a "down" sample
/// by a straight-down cast above the view hit, and its slope limit drops samples on steep surfaces.
function sampleStroke(press: VegetationPress, uv: Uv): void {
  if (press.kind !== "stroke" || press.picking) {
    return;
  }
  press.picking = true;
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
      if (normal && brush.maxSlopeDeg < 90 && !press.reapply) {
        const tilt = (Math.acos(Math.min(1, Math.max(-1, normal[1]))) * 180) / Math.PI;
        if (tilt > brush.maxSlopeDeg) {
          return;
        }
      }
      if (
        press.last &&
        Math.hypot(
          position[0] - press.last[0],
          position[1] - press.last[1],
          position[2] - press.last[2],
        ) < brush.spacing
      ) {
        return;
      }
      press.last = position;
      press.stamps.push({
        position,
        radius: brush.radius,
        falloff: brush.falloff,
        pressure: press.pressure,
        blend: press.blend,
      });
    } catch {
      // The engine may be briefly busy; the next move retries.
    } finally {
      press.picking = false;
    }
  })();
}

/// Feeds one pointer move into a live gesture.
export function moveVegetationPress(press: VegetationPress, uv: Uv, pressure: number): void {
  if (press.kind === "stroke") {
    press.pressure = pressure > 0 ? pressure : 1;
    sampleStroke(press, uv);
    return;
  }
  if (press.kind === "lasso") {
    const last = press.path[press.path.length - 1];
    if (!last || Math.hypot(uv.u - last.u, uv.v - last.v) >= LASSO_PATH_SPACING) {
      press.path.push(uv);
    }
  }
}

/// The traced lasso ring in world metres: the path resampled to a bounded count, each sample
/// dropped onto the ground.
async function traceGroundRing(path: Uv[]): Promise<VegetationPoint[]> {
  const stride = Math.max(1, Math.ceil(path.length / LASSO_GROUND_SAMPLES));
  const ring: VegetationPoint[] = [];
  for (let index = 0; index < path.length; index += stride) {
    const sample = path[index]!;
    const picked = await client.pick(sample.u, sample.v);
    if (picked.position) {
      ring.push(picked.position);
    }
  }
  return ring;
}

/// Finishes a gesture, committing whatever it captured. A stroke waits out any pick still in
/// flight so the release never drops the last stamp.
export async function finishVegetationPress(press: VegetationPress, uv: Uv): Promise<void> {
  if (press.kind === "stroke") {
    while (press.picking) {
      await new Promise((resolve) => setTimeout(resolve, 32));
    }
    await (press.reapply ? reapplyCommit(press.stamps) : strokeCommit(press.stamps));
    return;
  }
  if (press.kind === "lasso") {
    await lassoSelect(await traceGroundRing([...press.path, press.path[0]!]));
    return;
  }
  const [from, to] = await Promise.all([
    client.pick(press.start.u, press.start.v),
    client.pick(uv.u, uv.v),
  ]);
  if (!from.position || !to.position) {
    return;
  }
  await volumeCommit([from.position, to.position]);
}

/// Runs the click gesture (a press that never dragged) for the active tool. Reports whether the
/// tool consumed the click; an unconsumed click falls through to ordinary ray-pick selection.
export function clickVegetationTool(uv: Uv): boolean {
  if (!toolArmed()) {
    return false;
  }
  const state = useEditorStore.getState();
  switch (state.vegetationTool) {
    case "single":
      if (state.vegetationSpecies.size === 0) {
        return false;
      }
      void plantAt(uv);
      return true;
    case "pin":
      void pinAt(uv);
      return true;
    case "fill":
      void fillAt(uv);
      return true;
    case "promote":
      void promoteAt(uv);
      return true;
    case "spline":
      void splinePointAt(uv);
      return true;
    default:
      return false;
  }
}
