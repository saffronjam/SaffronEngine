/// The tab-strip drag machine, shared by the main view tabs and the compact dock strips:
/// pointer-capture drag with a 4 px latch, one centers snapshot at drag start, a transform-only live
/// reorder preview, click-vs-drag activation, and a WAAPI FLIP settle on drop. The model is never
/// mutated until drop, so every cancel path is free.
///
/// For a dock strip with a `leafId` the machine also tears out: once the pointer escapes the strip
/// band vertically it hands the drag to `dockDrag` and commits via `onDrop` instead of `onReorder`.
/// Tear-out keeps the same pointer capture, so the source strip drives the drag throughout.
import { useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, PointerEvent } from "react";
import type { DockNodeId, DockPanelId, DropTarget } from "../../state/dockLayout";
import { beginDockDrag, moveDockDrag } from "./dockDrag";

const TAB_DRAG_THRESHOLD_PX = 4;
/// Vertical escape past the strip band that tears a dock tab out — at which point the drag is
/// handed to `dockDrag`, which drives it from window listeners so the source leaf can collapse.
const TEAR_OUT_PX = 32;

interface StripBox {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

interface TabDragState {
  id: string;
  pointerId: number;
  startX: number;
  currentX: number;
  dragging: boolean;
  startIndex: number;
  previewIndex: number;
  width: number;
  order: string[];
  centers: Record<string, number>;
  stripBox: StripBox;
}

export interface UseTabStripDragOptions {
  /// Tear-out exists only for "dock" strips with a `leafId`; "view" tabs never leave the strip.
  domain: "view" | "dock";
  /// The source leaf id (dock strips) — used as the tear-out origin and drop fallback.
  leafId?: DockNodeId;
  /// Ids that are both excluded as insertion targets and non-draggable (e.g. the scene tab).
  pinnedIds?: string[];
  /// A press whose target matches this never arms a drag; defaults to the close affordance.
  shouldIgnoreDragStart?(target: Element): boolean;
  /// Commit a same-strip reorder; `index` is in the without-moving-tab space.
  onReorder(id: string, index: number): void;
  /// The pointerup-without-threshold (click) path.
  onActivate(id: string): void;
  /// Commit a torn-out drop to a cross-dock target (dock strips only).
  onDrop?(id: string, target: DropTarget): void;
}

export interface TabStripDragHandlers {
  onPointerDown(event: PointerEvent<HTMLButtonElement>): void;
  onPointerMove(event: PointerEvent<HTMLButtonElement>): void;
  onPointerUp(event: PointerEvent<HTMLButtonElement>): void;
  onPointerCancel(event: PointerEvent<HTMLButtonElement>): void;
  onLostPointerCapture(event: PointerEvent<HTMLButtonElement>): void;
}

export interface UseTabStripDragApi {
  handlersFor(id: string): TabStripDragHandlers;
  styleFor(id: string): CSSProperties | undefined;
  registerTabRef(id: string, el: HTMLButtonElement | null): void;
  /// Resolved draggability: pinned tabs are non-draggable, and the strip routes their
  /// activation through onClick while draggable tabs go through the click-vs-drag pointerup.
  canDrag(id: string): boolean;
  /// A threshold-crossed drag is live in this strip.
  dragging: boolean;
  /// The armed/dragged tab id (null when idle).
  draggedId: string | null;
}

function defaultShouldIgnoreDragStart(target: Element): boolean {
  return target.closest("[data-tab-close='true']") !== null;
}

export function useTabStripDrag(
  itemIds: string[],
  options: UseTabStripDragOptions,
): UseTabStripDragApi {
  const [tabDrag, setTabDrag] = useState<TabDragState | null>(null);
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());
  const settleRef = useRef<Map<string, number> | null>(null);
  const pinned = options.pinnedIds ?? [];
  const shouldIgnore = options.shouldIgnoreDragStart ?? defaultShouldIgnoreDragStart;
  const canTearOut = options.domain === "dock" && options.leafId !== undefined;

  const canDrag = (id: string): boolean => !pinned.includes(id);

  // A handed-off torn drag lives entirely in `dockDrag` (`tabDrag` is null), so this no-ops then;
  // pre-tear it just clears the in-strip drag on capture loss / cancel.
  const reset = (): void => {
    setTabDrag(null);
  };

  // FLIP settle for a tab drop. The drop render reorders the DOM, clears the drag
  // transforms, and re-enables transitions in one commit — left alone, the browser
  // would transition each moved tab from `new slot + old drag transform`, a phantom
  // slide from the wrong side. Suppress transitions before the first rect read (style
  // writes do not force a recalc), so removing the transform commits without starting
  // an animation and measures true final layout; then WAAPI-settle each tab that would
  // jump from its pre-drop visual position. Neighbors have zero diff (the preview
  // already placed them), so only the dragged tab settles, from under the cursor.
  useLayoutEffect(() => {
    const before = settleRef.current;
    if (!before) {
      return;
    }
    settleRef.current = null;
    const nodes = [...tabRefs.current.entries()];
    for (const [, node] of nodes) {
      node.style.transition = "none";
    }
    const settles: [HTMLButtonElement, number][] = [];
    for (const [id, node] of nodes) {
      const left = before.get(id);
      if (left === undefined) {
        continue;
      }
      const diff = left - node.getBoundingClientRect().left;
      if (Math.abs(diff) >= 0.5) {
        settles.push([node, diff]);
      }
    }
    for (const [, node] of nodes) {
      node.style.transition = "";
    }
    for (const [node, diff] of settles) {
      node.animate([{ transform: `translateX(${diff}px)` }, { transform: "none" }], {
        duration: 150,
        easing: "ease-out",
      });
    }
  });

  const registerTabRef = (id: string, node: HTMLButtonElement | null): void => {
    if (node) {
      tabRefs.current.set(id, node);
    } else {
      tabRefs.current.delete(id);
    }
  };

  const insertionIndexForPointer = (drag: TabDragState, x: number): number => {
    const withoutMoving = drag.order.filter((id) => id !== drag.id);
    for (let i = 0; i < withoutMoving.length; i += 1) {
      const id = withoutMoving[i];
      if (pinned.includes(id)) {
        continue;
      }
      const center = drag.centers[id];
      if (center === undefined) {
        continue;
      }
      if (x < center) {
        return i;
      }
    }
    return withoutMoving.length;
  };

  const beginDrag = (id: string, event: PointerEvent<HTMLButtonElement>): void => {
    if (!canDrag(id) || event.button !== 0) {
      return;
    }
    const target = event.target;
    if (target instanceof Element && shouldIgnore(target)) {
      return;
    }
    const node = tabRefs.current.get(id);
    if (!node) {
      return;
    }
    const order = [...itemIds];
    const startIndex = order.indexOf(id);
    if (startIndex < 0) {
      return;
    }
    const centers: Record<string, number> = {};
    for (const candidate of order) {
      const tabNode = tabRefs.current.get(candidate);
      if (tabNode) {
        const rect = tabNode.getBoundingClientRect();
        centers[candidate] = rect.left + rect.width / 2;
      }
    }
    const nodeRect = node.getBoundingClientRect();
    const stripRect = (node.parentElement ?? node).getBoundingClientRect();
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    setTabDrag({
      id,
      pointerId: event.pointerId,
      startX: event.clientX,
      currentX: event.clientX,
      dragging: false,
      startIndex,
      previewIndex: startIndex,
      width: nodeRect.width,
      order,
      centers,
      stripBox: {
        top: stripRect.top,
        bottom: stripRect.bottom,
        left: stripRect.left,
        right: stripRect.right,
      },
    });
  };

  const moveDrag = (event: PointerEvent<HTMLButtonElement>): void => {
    if (!tabDrag) {
      return;
    }
    const delta = event.clientX - tabDrag.startX;
    const dragging = tabDrag.dragging || Math.abs(delta) >= TAB_DRAG_THRESHOLD_PX;
    const pointer = { x: event.clientX, y: event.clientY };

    if (canTearOut && dragging) {
      const { top, bottom } = tabDrag.stripBox;
      const escaped = event.clientY < top - TEAR_OUT_PX || event.clientY > bottom + TEAR_OUT_PX;
      if (escaped) {
        // Hand off to `dockDrag`: release pointer capture so the source tab/leaf can unmount
        // (DockRoot subtracts the torn panel), and stop driving the drag here — `dockDrag` owns
        // move/drop/cancel from window listeners from now on.
        const node = tabRefs.current.get(tabDrag.id);
        if (node?.hasPointerCapture(tabDrag.pointerId)) {
          node.releasePointerCapture(tabDrag.pointerId);
        }
        beginDockDrag(
          {
            panelId: tabDrag.id as DockPanelId,
            fromLeafId: options.leafId as DockNodeId,
            ghostWidth: tabDrag.width,
            ghostLabel: node?.textContent?.trim() ?? tabDrag.id,
            pointer,
          },
          (id, target) => options.onDrop?.(id, target),
        );
        moveDockDrag(pointer);
        setTabDrag(null);
        return;
      }
    }

    setTabDrag({
      ...tabDrag,
      currentX: event.clientX,
      dragging,
      previewIndex: dragging
        ? insertionIndexForPointer(tabDrag, event.clientX)
        : tabDrag.previewIndex,
    });
  };

  const endDrag = (id: string, event: PointerEvent<HTMLButtonElement>): void => {
    if (!tabDrag || tabDrag.id !== id) {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    const activate = !tabDrag.dragging;
    const nextIndex = tabDrag.previewIndex;
    if (tabDrag.dragging) {
      const lefts = new Map<string, number>();
      for (const [tabId, node] of tabRefs.current) {
        lefts.set(tabId, node.getBoundingClientRect().left);
      }
      settleRef.current = lefts;
    }
    setTabDrag(null);
    if (activate) {
      options.onActivate(id);
    } else {
      options.onReorder(tabDrag.id, nextIndex);
    }
  };

  const styleFor = (id: string): CSSProperties | undefined => {
    if (!tabDrag?.dragging) {
      return undefined;
    }
    if (id === tabDrag.id) {
      return { transform: `translateX(${tabDrag.currentX - tabDrag.startX}px)` };
    }
    const index = tabDrag.order.indexOf(id);
    if (
      tabDrag.previewIndex > tabDrag.startIndex &&
      index > tabDrag.startIndex &&
      index <= tabDrag.previewIndex
    ) {
      return { transform: `translateX(-${tabDrag.width}px)` };
    }
    if (
      tabDrag.previewIndex < tabDrag.startIndex &&
      index >= tabDrag.previewIndex &&
      index < tabDrag.startIndex
    ) {
      return { transform: `translateX(${tabDrag.width}px)` };
    }
    return undefined;
  };

  const handlersFor = (id: string): TabStripDragHandlers => ({
    onPointerDown: (event) => beginDrag(id, event),
    onPointerMove: moveDrag,
    onPointerUp: (event) => endDrag(id, event),
    onPointerCancel: reset,
    onLostPointerCapture: reset,
  });

  return {
    handlersFor,
    styleFor,
    registerTabRef,
    canDrag,
    dragging: tabDrag?.dragging ?? false,
    draggedId: tabDrag?.id ?? null,
  };
}
