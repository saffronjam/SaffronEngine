/// The no-drag route for moving a docked panel: a right-click menu on a dock tab with a
/// per-destination submenu, plus Close for a closable panel. Everything routes through the same
/// `movePanel`/`closePanel` the drag layer uses, so it is an alternative entry point rather than a
/// second code path.
import type { ReactNode } from "react";
import { useEditorStore } from "../../state/store";
import {
  isLeaf,
  panelKind,
  type DockEdge,
  type DockLeaf,
  type DockNodeId,
  type DockPanelId,
} from "../../state/dockLayout";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { panelDef, panelTitle } from "./panelRegistry";

const EDGES: DockEdge[] = ["left", "right", "top", "bottom"];

function leafLabel(leaf: DockLeaf): string {
  const lead = leaf.activeTab ?? leaf.tabs[0];
  const title = lead ? panelTitle(lead) : "Empty";
  return leaf.tabs.length > 1 ? `${title} (+${leaf.tabs.length - 1})` : title;
}

export function DockTabContextMenu({
  panelId,
  leafId,
  children,
}: {
  panelId: DockPanelId;
  leafId: DockNodeId;
  children: ReactNode;
}) {
  const kind = panelKind(panelId);
  const layout = useEditorStore((s) => s.dockLayouts[kind]);
  const movePanel = useEditorStore((s) => s.movePanel);
  const closePanel = useEditorStore((s) => s.closePanel);

  const destinations = Object.values(layout.nodes).filter(
    (node): node is DockLeaf => isLeaf(node) && !node.locked && node.id !== leafId,
  );
  const closable = panelDef(panelId)?.closable ?? true;

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
      <ContextMenuContent className="min-w-44">
        {destinations.length === 0 ? (
          <ContextMenuItem disabled>No other panels to move to</ContextMenuItem>
        ) : (
          destinations.map((dest) => (
            <ContextMenuSub key={dest.id}>
              <ContextMenuSubTrigger>Move beside {leafLabel(dest)}</ContextMenuSubTrigger>
              <ContextMenuSubContent>
                <ContextMenuItem
                  onSelect={() =>
                    movePanel(panelId, { kind: "tab", leafId: dest.id, index: dest.tabs.length })
                  }
                >
                  Tab together
                </ContextMenuItem>
                <ContextMenuSeparator />
                {EDGES.map((edge) => (
                  <ContextMenuItem
                    key={edge}
                    onSelect={() => movePanel(panelId, { kind: "split", leafId: dest.id, edge })}
                  >
                    Split {edge}
                  </ContextMenuItem>
                ))}
              </ContextMenuSubContent>
            </ContextMenuSub>
          ))
        )}
        {closable ? (
          <>
            <ContextMenuSeparator />
            <ContextMenuItem onSelect={() => closePanel(panelId)}>Close</ContextMenuItem>
          </>
        ) : null}
      </ContextMenuContent>
    </ContextMenu>
  );
}
