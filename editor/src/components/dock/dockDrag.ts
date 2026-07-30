/// The cross-strip drag channel, module-scope like `layoutBus`. While a dock tab is torn out, the
/// drag state, the cursor ghost, and the hovered drop target live here: a foreign strip's
/// `useTabStripDrag` instance is idle and cannot render a parting preview for a tab it does not own,
/// so every `TabStrip` and the drop overlay subscribe to this single source.
///
/// Hit-testing is manual because `setPointerCapture` retargets every pointer event to the source
/// tab, so candidate leaves never see `pointerover`. Each move re-snapshots the mounted
/// `[data-dock-leaf]` rects — cheap, and it also catches the reveal bands that mount mid-drag.
import { useSyncExternalStore } from "react";
import type { DockEdge, DockNodeId, DockPanelId, DropTarget } from "../../state/dockLayout";

/// A split is offered on a leaf edge only when the resulting half stays at least this wide/
/// tall — so a too-small leaf merges instead of splitting into unusable slivers (rrp's px
/// minSize then enforces the viewport/sidebar minima on the live tree).
const MIN_SPLIT_PX = 120;
/// The outer fraction of a leaf body that triggers an edge split; the center merges (VS Code).
const EDGE_FRACTION = 1 / 3;

interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

export interface LeafDropRect {
  leafId: DockNodeId;
  bodyRect: Rect;
  /// null for a reveal band (no tab strip of its own).
  stripRect: Rect | null;
  /// Tab center x's in strip order, for the insertion index.
  stripCenters: number[];
  acceptsTabs: boolean;
  acceptsSplits: boolean;
}

export interface DockDragState {
  panelId: DockPanelId;
  fromLeafId: DockNodeId;
  ghostWidth: number;
  ghostLabel: string;
  pointer: { x: number; y: number };
  hovered: DropTarget | null;
}

let state: DockDragState | null = null;
let onDropResolved: ((panelId: DockPanelId, target: DropTarget) => void) | null = null;
const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of listeners) {
    listener();
  }
}

function rectOf(el: Element): Rect {
  const r = el.getBoundingClientRect();
  return {
    left: r.left,
    top: r.top,
    right: r.right,
    bottom: r.bottom,
    width: r.width,
    height: r.height,
  };
}

function contains(r: Rect, x: number, y: number): boolean {
  return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
}

/// The insertion index in the without-moving-tab space, from snapshotted tab centers — the
/// same semantics as the in-strip `insertionIndexForPointer`.
export function insertionIndexForCenters(centers: number[], x: number): number {
  for (let i = 0; i < centers.length; i += 1) {
    if (x < centers[i]) {
      return i;
    }
  }
  return centers.length;
}

/// The leaf edge the pointer is in the outer third of (split zone), or null for the center
/// (merge). A split is suppressed when the resulting half would be too small.
export function edgeZone(rect: Rect, x: number, y: number): DockEdge | null {
  const fx = rect.width > 0 ? (x - rect.left) / rect.width : 0.5;
  const fy = rect.height > 0 ? (y - rect.top) / rect.height : 0.5;
  const candidates: { edge: DockEdge; d: number }[] = [
    { edge: "left", d: fx },
    { edge: "right", d: 1 - fx },
    { edge: "top", d: fy },
    { edge: "bottom", d: 1 - fy },
  ];
  const nearest = candidates.reduce((best, c) => (c.d < best.d ? c : best));
  if (nearest.d > EDGE_FRACTION) {
    return null;
  }
  const half =
    nearest.edge === "left" || nearest.edge === "right" ? rect.width / 2 : rect.height / 2;
  return half >= MIN_SPLIT_PX ? nearest.edge : null;
}

/// The drop target under the pointer: a hovered strip → insert at the computed index; a leaf
/// body's outer-third edge → split; its center → merge (append). Strips win over bodies (a
/// strip rect sits inside its body rect).
export function hitTestRects(rects: LeafDropRect[], x: number, y: number): DropTarget | null {
  for (const r of rects) {
    if (r.acceptsTabs && r.stripRect && contains(r.stripRect, x, y)) {
      return { kind: "tab", leafId: r.leafId, index: insertionIndexForCenters(r.stripCenters, x) };
    }
  }
  for (const r of rects) {
    if (!r.acceptsTabs || !contains(r.bodyRect, x, y)) {
      continue;
    }
    if (r.acceptsSplits) {
      const edge = edgeZone(r.bodyRect, x, y);
      if (edge) {
        return { kind: "split", leafId: r.leafId, edge };
      }
    }
    return { kind: "tab", leafId: r.leafId, index: r.stripCenters.length };
  }
  return null;
}

/// Snapshot every mounted `[data-dock-leaf]` (and reveal band) into drop rects. Reads DOM
/// attributes only — no store coupling — so a leaf renders its own acceptance policy.
export function snapshotLeafRects(): LeafDropRect[] {
  const out: LeafDropRect[] = [];
  for (const el of document.querySelectorAll<HTMLElement>("[data-dock-leaf]")) {
    const leafId = el.dataset.dockLeaf;
    if (leafId === undefined) {
      continue;
    }
    const strip = el.querySelector<HTMLElement>("[data-dock-strip]");
    const stripCenters = strip
      ? [...strip.querySelectorAll<HTMLElement>("[role='tab']")].map((tab) => {
          const r = tab.getBoundingClientRect();
          return r.left + r.width / 2;
        })
      : [];
    const acceptsTabs = el.dataset.dockAcceptsTabs !== "false";
    out.push({
      leafId,
      bodyRect: rectOf(el),
      stripRect: strip ? rectOf(strip) : null,
      stripCenters,
      acceptsTabs,
      // Splits default to following tab-acceptance; reveal bands opt out (tab-only).
      acceptsSplits:
        el.dataset.dockAcceptsSplits !== undefined
          ? el.dataset.dockAcceptsSplits !== "false"
          : acceptsTabs,
    });
  }
  return out;
}

function onWindowMove(event: PointerEvent): void {
  if (!state) {
    return;
  }
  moveDockDrag({ x: event.clientX, y: event.clientY });
}

function onWindowUp(): void {
  if (!state) {
    return;
  }
  const { panelId, hovered } = state;
  const onDrop = onDropResolved;
  // Commit the drop BEFORE clearing, so the `setBranchSizes` gate (`getDockDrag() !== null`)
  // stays closed through the move-commit remount; then tear down.
  if (hovered) {
    onDrop?.(panelId, hovered);
  }
  teardown();
}

function onWindowKey(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    cancelDockDrag();
  }
}

function teardown(): void {
  window.removeEventListener("pointermove", onWindowMove);
  window.removeEventListener("pointerup", onWindowUp);
  window.removeEventListener("pointercancel", cancelDockDrag);
  window.removeEventListener("keydown", onWindowKey);
  window.removeEventListener("blur", cancelDockDrag);
  state = null;
  onDropResolved = null;
  notify();
}

/// Arm a torn drag. The source strip releases pointer capture and hands off here, so the drag
/// survives the source tab/leaf unmounting (collapsing). Window listeners drive move/drop/cancel;
/// `onDrop` commits the move — kept as an indirection so this module never imports the store.
export function beginDockDrag(
  init: Omit<DockDragState, "hovered">,
  onDrop: (panelId: DockPanelId, target: DropTarget) => void,
): void {
  state = { ...init, hovered: null };
  onDropResolved = onDrop;
  window.addEventListener("pointermove", onWindowMove);
  window.addEventListener("pointerup", onWindowUp);
  window.addEventListener("pointercancel", cancelDockDrag);
  window.addEventListener("keydown", onWindowKey);
  window.addEventListener("blur", cancelDockDrag);
  notify();
}

export function moveDockDrag(pointer: { x: number; y: number }): void {
  if (!state) {
    return;
  }
  const hovered = hitTestRects(snapshotLeafRects(), pointer.x, pointer.y);
  state = { ...state, pointer, hovered };
  notify();
}

export function cancelDockDrag(): void {
  if (state === null) {
    return;
  }
  teardown();
}

export function getDockDrag(): DockDragState | null {
  return state;
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/// Subscribe a component to the torn-drag state (null when idle).
export function useDockDrag(): DockDragState | null {
  return useSyncExternalStore(subscribe, getDockDrag, () => null);
}

/// The torn panel id (stable while a drag is torn), so `DockRoot` can subtract it from the
/// rendered tree without re-rendering on every pointer move.
export function useTornPanelId(): DockPanelId | null {
  return useSyncExternalStore(
    subscribe,
    () => state?.panelId ?? null,
    () => null,
  );
}
