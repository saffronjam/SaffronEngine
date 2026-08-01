import { client } from "../../control/client";
import { errorText, notify, notifyError } from "../../lib/flash";
import { useEditorStore, type VegetationPoint } from "../../state/store";
import { anchorRecord } from "../vegetationPlanting";
import { applyVegetationGesture, pushVegetationGesture } from "../vegetationGesture";
import {
  commitFill,
  commitStroke,
  recookRegion,
  togglePin,
  type BrushStamp,
} from "../vegetationPainting";
import { boundsAround, commitSpline, commitVolume } from "../vegetationShapes";
import type { Uv } from "./viewportInput";

/// Anchor-plants the first selected species at the picked ground position as one undoable edit.
export async function plantAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const position = picked.position;
    if (!position) {
      notifyError("No ground under the cursor to plant on");
      return;
    }
    const store = useEditorStore.getState();
    const family = [...store.vegetationSpecies][0];
    if (!family) {
      return;
    }
    pushVegetationGesture(
      "Plant anchor",
      await applyVegetationGesture([anchorRecord(position, family)]),
    );
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Commits a captured brush stroke as one authored-chunk transaction (with a
/// cell-scoped recook), recorded as one undoable edit.
export async function strokeCommit(stamps: BrushStamp[]): Promise<void> {
  const store = useEditorStore.getState();
  const target = store.vegetationActiveLayer;
  const first = stamps[0];
  if (!target || !first) {
    return;
  }
  const label =
    target.slot === "blocker"
      ? "Block vegetation"
      : first.blend.kind === "level"
        ? "Set vegetation density"
        : first.blend.sign === -1
          ? "Erase vegetation"
          : "Paint vegetation";
  try {
    const pair = await commitStroke(target, stamps);
    if (!pair) {
      return;
    }
    store.pushEdit({ label, undo: pair.undo, redo: pair.redo });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Recooks the region a reapply stroke touched — deterministic refresh, no
/// authored mutation and no undo entry.
export async function reapplyCommit(stamps: BrushStamp[]): Promise<void> {
  const target = useEditorStore.getState().vegetationActiveLayer;
  if (!target || stamps.length === 0) {
    return;
  }
  try {
    await recookRegion(target, stamps);
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Toggles the clicked macro plant's pin row in the active layer as one undoable
/// chunk transaction (a pin survives graph recooks).
export async function pinAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const plant = picked.plant;
    if (!plant) {
      return;
    }
    const store = useEditorStore.getState();
    const target = store.vegetationActiveLayer;
    if (!target || target.locked) {
      notifyError("Select an unlocked layer to pin into");
      return;
    }
    const inspected = await client.vegetationRuntimeInspect(plant);
    const cell = inspected.resident?.cell;
    if (!cell) {
      return;
    }
    const pair = await togglePin(target, plant, cell);
    if (!pair) {
      return;
    }
    store.pushEdit({
      label: pair.pinned ? "Pin plant" : "Unpin plant",
      undo: pair.undo,
      redo: pair.redo,
    });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Lays the brush's target density across the whole authored tile of the clicked cell as one
/// undoable transaction.
export async function fillAt({ u, v }: Uv): Promise<void> {
  const store = useEditorStore.getState();
  const target = store.vegetationActiveLayer;
  if (!target || target.locked) {
    notifyError("Select an unlocked field layer to fill");
    return;
  }
  try {
    const picked = await client.pick(u, v);
    const position = picked.position;
    if (!position) {
      notifyError("No ground under the cursor to fill from");
      return;
    }
    const pair = await commitFill(target, position, store.vegetationBrush.density);
    if (!pair) {
      return;
    }
    store.pushEdit({ label: "Fill cell", undo: pair.undo, redo: pair.redo });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Promotes the clicked macro plant to a transient entity view, recorded with its demotion as the
/// inverse. Promotion belongs to a running play world; outside one the engine says so.
export async function promoteAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const plant = picked.plant;
    if (!plant) {
      return;
    }
    await client.vegetationPromote(plant);
    useEditorStore.getState().pushEdit({
      label: "Promote plant",
      undo: () => client.vegetationDemote(plant),
      redo: () => client.vegetationPromote(plant),
    });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Point-in-polygon on the ground plane (ray casting over the traced XZ ring).
function inGroundPolygon(polygon: VegetationPoint[], x: number, z: number): boolean {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i, i += 1) {
    const a = polygon[i]!;
    const b = polygon[j]!;
    if (a[2] > z !== b[2] > z && x < ((b[0] - a[0]) * (z - a[2])) / (b[2] - a[2]) + a[0]) {
      inside = !inside;
    }
  }
  return inside;
}

/// Selects every resident macro plant rooted inside the lasso's traced ground ring. The trace is a
/// ground polygon rather than a screen one: a plant is selected by where it is rooted, so foliage
/// leaning across the ring from outside it does not join the selection.
export async function lassoSelect(ground: VegetationPoint[]): Promise<void> {
  const store = useEditorStore.getState();
  if (ground.length < 3) {
    store.setVegetationSelectedPlants([]);
    return;
  }
  try {
    const hits = await client.vegetationRuntimeQuery({
      query: { kind: "bounds", bounds: boundsAround(ground, 1) },
      filter: { families: [], requiredTags: [], lifecycles: [], interactionPolicies: [] },
      limit: 4096,
    });
    const selected = hits.hits
      .filter((hit) => {
        const ticks = hit.plant.positionTicks;
        return inGroundPolygon(
          ground,
          Number(BigInt(ticks[0])) / 4096,
          Number(BigInt(ticks[2])) / 4096,
        );
      })
      .map((hit) => hit.plant.plant);
    store.setVegetationSelectedPlants(selected);
    notify(
      hits.truncated
        ? `${selected.length} plant(s) selected of the first ${hits.hits.length} in range`
        : `${selected.length} plant(s) selected`,
    );
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Writes the dragged box into the active volume layer as one undoable layer transaction. The box
/// rises the brush radius above the ground the drag traced, so a drag on terrain encloses what
/// stands on it.
export async function volumeCommit(corners: [VegetationPoint, VegetationPoint]): Promise<void> {
  const store = useEditorStore.getState();
  const target = store.vegetationActiveLayer;
  if (!target || target.operator !== "volume" || target.locked) {
    notifyError("Select an unlocked volume layer to shape");
    return;
  }
  try {
    const brush = store.vegetationBrush;
    const pair = await commitVolume(target, corners, brush.radius, brush.falloff);
    if (!pair) {
      return;
    }
    store.pushEdit({ label: "Shape volume", undo: pair.undo, redo: pair.redo });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Writes the picked control polyline into the active spline layer as one undoable layer
/// transaction, then clears the in-progress points.
export async function splineCommit(): Promise<void> {
  const store = useEditorStore.getState();
  const target = store.vegetationActiveLayer;
  const points = store.vegetationShapePoints;
  if (!target || target.operator !== "spline" || target.locked) {
    notifyError("Select an unlocked spline layer to shape");
    return;
  }
  if (points.length < 2) {
    notifyError("A spline needs at least two control points");
    return;
  }
  try {
    const pair = await commitSpline(target, points, store.vegetationBrush.radius);
    if (!pair) {
      return;
    }
    store.clearVegetationShapePoints();
    store.pushEdit({ label: "Shape spline", undo: pair.undo, redo: pair.redo });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Appends the picked ground position to the Spline tool's in-progress control polyline.
export async function splinePointAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const position = picked.position;
    if (!position) {
      notifyError("No ground under the cursor to route through");
      return;
    }
    useEditorStore.getState().addVegetationShapePoint(position);
  } catch (err) {
    notifyError(errorText(err));
  }
}
