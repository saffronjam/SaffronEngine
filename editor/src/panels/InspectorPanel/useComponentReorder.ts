import { useCallback, useEffect, useState } from "react";
import type { CSSProperties, PointerEvent, RefObject } from "react";
import { client } from "../../control/client";
import { errorText, notifyError } from "../../lib/flash";
import { canonicalComponentNames } from "../../lib/componentOrder";
import { useEditorStore } from "../../state/store";
import { SECTION_DRAG_THRESHOLD_PX, type ComponentDragState } from "./registry";

/// Drag-to-reorder for the Inspector's component sections, plus the Sort action. The order is
/// engine state (`set-component-order`), so a drop writes optimistically, records one undo entry
/// once accepted, and rolls back on rejection. `settleRef` receives the pre-commit section tops so
/// the panel can animate the settle after the reorder lands.
export function useComponentReorder({
  names,
  selectedId,
  componentsObj,
  sectionRefs,
  settleRef,
  scrollAreaRef,
}: {
  names: string[];
  selectedId: string | null;
  componentsObj: Record<string, unknown> | undefined;
  sectionRefs: RefObject<Map<string, HTMLElement>>;
  settleRef: RefObject<Map<string, number> | null>;
  scrollAreaRef: RefObject<HTMLDivElement | null>;
}): {
  componentDrag: ComponentDragState | null;
  beginComponentDrag(component: string, event: PointerEvent<HTMLButtonElement>): void;
  moveComponentDrag(event: PointerEvent<HTMLButtonElement>): void;
  endComponentDrag(component: string, event: PointerEvent<HTMLButtonElement>): void;
  resetComponentDrag(): void;
  componentDragStyle(component: string): CSSProperties | undefined;
  onSortComponents(): void;
} {
  const [componentDrag, setComponentDrag] = useState<ComponentDragState | null>(null);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const pushEdit = useEditorStore((s) => s.pushEdit);

  const scrollViewport = useCallback(
    (): HTMLElement | null =>
      scrollAreaRef.current?.querySelector(
        "[data-radix-scroll-area-viewport], [data-slot='scroll-area-viewport']",
      ) ?? null,
    [scrollAreaRef],
  );

  function insertionIndexForPointer(
    drag: ComponentDragState,
    y: number,
    scrollTop: number,
  ): number {
    const scrollDelta = scrollTop - drag.startScrollTop;
    const withoutMoving = drag.order.filter((component) => component !== drag.id);
    for (let i = 0; i < withoutMoving.length; i += 1) {
      const component = withoutMoving[i];
      const center = drag.centers[component];
      if (center !== undefined && y + scrollDelta < center) {
        return i;
      }
    }
    return withoutMoving.length;
  }

  useEffect(() => {
    const viewport = scrollViewport();
    if (!viewport || !componentDrag) {
      return;
    }
    const onScroll = (): void => {
      setComponentDrag((drag) => {
        if (!drag) {
          return null;
        }
        const scrollTop = viewport.scrollTop;
        const delta = drag.currentY - drag.startY + (scrollTop - drag.startScrollTop);
        const dragging = drag.dragging || Math.abs(delta) >= SECTION_DRAG_THRESHOLD_PX;
        return {
          ...drag,
          currentScrollTop: scrollTop,
          dragging,
          previewIndex: dragging
            ? insertionIndexForPointer(drag, drag.currentY, scrollTop)
            : drag.previewIndex,
        };
      });
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    return () => viewport.removeEventListener("scroll", onScroll);
  }, [componentDrag, scrollViewport]);

  const applyComponentOrderOptimistic = (order: string[]): void => {
    const current = useEditorStore.getState().componentsBySelected;
    if (!current) {
      return;
    }
    useEditorStore.getState().setComponentsBySelected({ ...current, componentOrder: order });
  };

  const applyComponentOrder = (id: string, order: string[]): Promise<unknown> =>
    client.setComponentOrder(id, order);

  const recordComponentOrderEdit = (id: string, prior: string[], after: string[]): void => {
    if (JSON.stringify(prior) === JSON.stringify(after)) {
      return;
    }
    pushEdit(
      {
        label: "Reorder components",
        selectionId: id,
        undo: () => applyComponentOrder(id, prior),
        redo: () => applyComponentOrder(id, after),
      },
      "scene",
    );
  };

  const commitComponentOrder = (next: string[]): void => {
    const id = selectedId;
    const prior = [...names];
    if (id === null || JSON.stringify(prior) === JSON.stringify(next)) {
      return;
    }
    applyComponentOrderOptimistic(next);
    void applyComponentOrder(id, next)
      .then(() => recordComponentOrderEdit(id, prior, next))
      .catch((err: unknown) => {
        applyComponentOrderOptimistic(prior);
        notifyError(errorText(err));
      });
  };

  const moveComponentToIndex = (component: string, index: number): void => {
    const without = names.filter((name) => name !== component);
    const insertAt = Math.min(Math.max(index, 0), without.length);
    commitComponentOrder([...without.slice(0, insertAt), component, ...without.slice(insertAt)]);
  };

  const beginComponentDrag = (component: string, event: PointerEvent<HTMLButtonElement>): void => {
    if (event.button !== 0) {
      return;
    }
    const node = sectionRefs.current.get(component);
    if (!node) {
      return;
    }
    const order = [...names];
    const startIndex = order.indexOf(component);
    if (startIndex < 0) {
      return;
    }
    const centers: Record<string, number> = {};
    for (const candidate of order) {
      const section = sectionRefs.current.get(candidate);
      if (section) {
        const rect = section.getBoundingClientRect();
        centers[candidate] = rect.top + rect.height / 2;
      }
    }
    const rect = node.getBoundingClientRect();
    const scrollTop = scrollViewport()?.scrollTop ?? 0;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    setDragActive(true);
    setComponentDrag({
      id: component,
      startY: event.clientY,
      currentY: event.clientY,
      startScrollTop: scrollTop,
      currentScrollTop: scrollTop,
      dragging: false,
      startIndex,
      previewIndex: startIndex,
      height: rect.height,
      order,
      centers,
    });
  };

  const moveComponentDrag = (event: PointerEvent<HTMLButtonElement>): void => {
    if (!componentDrag) {
      return;
    }
    const scrollTop = scrollViewport()?.scrollTop ?? componentDrag.currentScrollTop;
    const delta = event.clientY - componentDrag.startY + (scrollTop - componentDrag.startScrollTop);
    const dragging = componentDrag.dragging || Math.abs(delta) >= SECTION_DRAG_THRESHOLD_PX;
    setComponentDrag({
      ...componentDrag,
      currentY: event.clientY,
      currentScrollTop: scrollTop,
      dragging,
      previewIndex: dragging
        ? insertionIndexForPointer(componentDrag, event.clientY, scrollTop)
        : componentDrag.previewIndex,
    });
  };

  const endComponentDrag = (component: string, event: PointerEvent<HTMLButtonElement>): void => {
    if (!componentDrag || componentDrag.id !== component) {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    const nextIndex = componentDrag.previewIndex;
    if (componentDrag.dragging) {
      const tops = new Map<string, number>();
      for (const [name, node] of sectionRefs.current) {
        tops.set(name, node.getBoundingClientRect().top);
      }
      settleRef.current = tops;
    }
    setComponentDrag(null);
    setDragActive(false);
    if (componentDrag.dragging) {
      moveComponentToIndex(componentDrag.id, nextIndex);
    }
  };

  const resetComponentDrag = (): void => {
    setComponentDrag(null);
    setDragActive(false);
  };

  const componentDragStyle = (component: string): CSSProperties | undefined => {
    if (!componentDrag?.dragging) {
      return undefined;
    }
    if (component === componentDrag.id) {
      const scrollDelta = componentDrag.currentScrollTop - componentDrag.startScrollTop;
      return {
        transform: `translateY(${componentDrag.currentY - componentDrag.startY + scrollDelta}px)`,
      };
    }
    const index = componentDrag.order.indexOf(component);
    if (
      componentDrag.previewIndex > componentDrag.startIndex &&
      index > componentDrag.startIndex &&
      index <= componentDrag.previewIndex
    ) {
      return { transform: `translateY(-${componentDrag.height}px)` };
    }
    if (
      componentDrag.previewIndex < componentDrag.startIndex &&
      index >= componentDrag.previewIndex &&
      index < componentDrag.startIndex
    ) {
      return { transform: `translateY(${componentDrag.height}px)` };
    }
    return undefined;
  };

  const onSortComponents = (): void => {
    if (componentsObj) {
      commitComponentOrder(canonicalComponentNames(componentsObj));
    }
  };

  return {
    componentDrag,
    beginComponentDrag,
    moveComponentDrag,
    endComponentDrag,
    resetComponentDrag,
    componentDragStyle,
    onSortComponents,
  };
}
