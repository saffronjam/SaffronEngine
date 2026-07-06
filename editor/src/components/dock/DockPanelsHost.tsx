/// The portal panel host: dockview's state-preservation technique, hand-rolled. Every open
/// panel renders exactly once, flat at app root, portaled into a per-panel host div that the
/// module map owns for the panel's open lifetime. A leaf body claims the hosts of the tabs
/// it owns with `appendChild` and toggles `display`, so a panel's React tree shape never
/// changes when it moves between docks — component state, refs, and DOM survive the move.
///
/// The map is keyed by `DockPanelId` (unique across both island kinds), so one map serves
/// every dockspace; each `DockPanelsHost` instance manages only its own kind's panels.
import { createPortal } from "react-dom";
import { useEffect, useLayoutEffect, useRef } from "react";
import { useEditorStore } from "../../state/store";
import {
  allOpenPanels,
  panelKind,
  visiblePanels,
  type DockNodeId,
  type DockPanelId,
  type DockSpaceKind,
} from "../../state/dockLayout";
import { panelDef } from "./panelRegistry";
import { cn } from "@/lib/utils";

const panelHosts = new Map<DockPanelId, HTMLDivElement>();

/// The host div for a panel, created on first claim and owned by the map until `closePanel`.
function hostFor(id: DockPanelId): HTMLDivElement {
  let host = panelHosts.get(id);
  if (!host) {
    host = document.createElement("div");
    // `contain-panel`: each panel is a layout/paint containment boundary, so a document-wide
    // reflow/repaint (e.g. an overlay's scroll-lock body write) can't cascade into sibling panels.
    host.className = "contain-panel flex h-full min-h-0 w-full min-w-0 flex-col";
    host.dataset.panelHost = id;
    panelHosts.set(id, host);
  }
  return host;
}

function destroyHost(id: DockPanelId): void {
  panelHosts.get(id)?.remove();
  panelHosts.delete(id);
}

/// Renders every open panel of `space` once, portaled into its host div. `always` panels
/// stay mounted (hidden) when not the active tab; `onlyWhenVisible` panels unmount when
/// hidden, leaving the empty host div attached. Mounted once per active dockspace in App.
export function DockPanelsHost({ space }: { space: DockSpaceKind }) {
  const layout = useEditorStore((s) => s.dockLayouts[space]);
  const open = allOpenPanels(layout);
  const visible = new Set(visiblePanels(layout));

  // Destroy hosts for this space's panels that have closed (left every leaf). `layout` is
  // a stable store reference that changes only on a dock mutation.
  useEffect(() => {
    const openSet = new Set(allOpenPanels(layout));
    for (const id of [...panelHosts.keys()]) {
      if (panelKind(id) === space && !openSet.has(id)) {
        destroyHost(id);
      }
    }
  }, [layout, space]);

  return (
    <>
      {open.map((id) => {
        const def = panelDef(id);
        if (!def || (def.renderer === "onlyWhenVisible" && !visible.has(id))) {
          return null;
        }
        const Component = def.component;
        return createPortal(<Component />, hostFor(id), id);
      })}
    </>
  );
}

/// A dock leaf's body: claims the host div of every tab it owns into one container and shows
/// only the active tab. React detaches/attaches refs within a commit, so a host can be
/// momentarily detached — only the map destroys hosts, never this claim, so the move is safe.
export function LeafBody({
  leafId,
  tabs,
  activeTab,
  className,
}: {
  leafId: DockNodeId;
  tabs: DockPanelId[];
  activeTab: DockPanelId | null;
  className?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const container = ref.current;
    if (!container) {
      return;
    }
    // Detach hosts this leaf no longer owns — e.g. a tab subtracted mid-drag by DockRoot's
    // effective layout — so the torn body hides. The module map keeps the host alive (never
    // destroyed here), so the panel re-claims it on drop/cancel with its state intact.
    for (const child of [...container.children]) {
      const id = (child as HTMLElement).dataset.panelHost;
      if (id && !tabs.includes(id as DockPanelId)) {
        container.removeChild(child);
      }
    }
    for (const id of tabs) {
      const host = hostFor(id);
      host.style.display = id === activeTab ? "" : "none";
      if (host.parentElement !== container) {
        container.appendChild(host);
      }
    }
  }, [tabs, activeTab]);

  return (
    <div ref={ref} data-leaf-body={leafId} className={cn("min-h-0 min-w-0 flex-1", className)} />
  );
}
