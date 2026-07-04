/// During a torn dock drag, thin edge bands stand in for a dockspace's empty (collapsed) edge
/// regions — dropping there docks the tab into the well-known persistent leaf (which always exists
/// in the model), so the region re-expands. The bands carry `data-dock-leaf` so the drag registry
/// picks them up like any mounted leaf, and `pointer-events-none` so they never self-hit the manual
/// rect hit-test. Parameterized by dockspace so the Scene and asset-editor islands share it.
import { useShallow } from "zustand/react/shallow";
import { useEditorStore } from "../../state/store";
import { isNodeRendered, type DockNodeId, type DockSpaceKind } from "../../state/dockLayout";
import { useDockDrag } from "./dockDrag";

export interface RevealBand {
  /// The persistent leaf a drop on this band docks into (re-expanding the region).
  leafId: DockNodeId;
  /// Every leaf that makes up this edge region. The band appears only once *all* of them have
  /// collapsed (none rendered), so it never overlays a still-visible sibling. Keyed on the
  /// leaves — not their enclosing branch — because `normalize` elides a single-child branch out
  /// of existence (the Scene's left column becomes just `leftBottom` once `hierarchy` is emptied),
  /// which would make a branch-keyed test read a missing node and mis-fire. Defaults to `[leafId]`
  /// for a single-leaf region (the right/bottom docks, the asset-editor edges).
  regionLeaves?: DockNodeId[];
  edge: "left" | "right" | "bottom";
}

const BAND_POSITION: Record<RevealBand["edge"], string> = {
  left: "left-0 top-0 h-full w-10",
  right: "right-0 top-0 h-full w-10",
  bottom: "bottom-0 left-0 right-0 h-10",
};

export function RevealBands({ space, bands }: { space: DockSpaceKind; bands: RevealBand[] }) {
  const dragging = useDockDrag() !== null;
  // One selector returns whether each band's region has collapsed (nothing rendered). `bands` is a
  // stable-length constant per call site, so the hook count never varies; `useShallow` keeps the
  // boolean array stable.
  const collapsed = useEditorStore(
    useShallow((s) => {
      const layout = s.dockLayouts[space];
      return bands.map((band) =>
        (band.regionLeaves ?? [band.leafId]).every((id) => !isNodeRendered(layout, id)),
      );
    }),
  );
  if (!dragging) {
    return null;
  }
  return (
    <>
      {bands.map((band, i) =>
        collapsed[i] ? (
          <div
            key={band.leafId}
            data-dock-leaf={band.leafId}
            data-dock-accepts-splits="false"
            className={`pointer-events-none absolute z-20 border border-dashed border-primary/40 bg-primary/5 ${BAND_POSITION[band.edge]}`}
          />
        ) : null,
      )}
    </>
  );
}
