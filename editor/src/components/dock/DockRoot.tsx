/// Renders a `DockLayout` tree: a branch becomes a `ResizablePanelGroup`, a leaf a `TabStrip` plus a
/// host-claiming body (a locked leaf — the live subsurface — drops the strip). Recursive and
/// dockspace-parameterized, so both islands share one renderer.
///
/// react-resizable-panels does not reconcile a changing child set in place, so each group is keyed
/// by a STRUCTURE HASH (node id + orientation + rendered child ids, sizes excluded): a child
/// add/remove/reorder remounts the group against the new `defaultLayout`, while a resize never
/// changes the key and so never remounts. An empty non-locked leaf is skipped, so its region
/// collapses while the leaf stays in the model.
import { useMemo } from "react";
import { useEditorStore } from "../../state/store";
import {
  isBranch,
  isLeaf,
  isPanelOpenIn,
  normalize,
  panelKind,
  removePanel,
  renderedChildIds,
  subtreeMinPx,
  type DockBranch,
  type DockLayout,
  type DockLeaf,
  type DockNodeId,
  type DockPanelId,
  type DockSpaceKind,
} from "../../state/dockLayout";
import type { Layout as PanelLayout } from "react-resizable-panels";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { TabStrip } from "./TabStrip";
import { LeafBody } from "./DockPanelsHost";
import { useTornPanelId } from "./dockDrag";
import { panelDef, panelTitle } from "./panelRegistry";

export function DockRoot({ space }: { space: DockSpaceKind }) {
  const real = useEditorStore((s) => s.dockLayouts[space]);
  const tornId = useTornPanelId();
  // While a tab of THIS island is torn out, render a tree with it subtracted: the tab + its body
  // vanish from the source, and a leaf left empty collapses (reclaiming space). Render-only — the
  // store is untouched until drop — and a collapsed leaf has no `[data-dock-leaf]`, so it can never
  // be a drop target (no dock-to-self). The `setBranchSizes` store action ignores writes while a
  // drag is active, so the collapse remount never corrupts the real sizes.
  const layout = useMemo(
    () =>
      tornId !== null && panelKind(tornId) === space && isPanelOpenIn(real, tornId)
        ? normalize(removePanel(real, tornId))
        : real,
    [real, tornId, space],
  );
  return <DockNodeView layout={layout} nodeId={layout.rootId} />;
}

function DockNodeView({ layout, nodeId }: { layout: DockLayout; nodeId: DockNodeId }) {
  const node = layout.nodes[nodeId];
  if (isLeaf(node)) {
    return <DockLeafView leaf={node} />;
  }
  if (isBranch(node)) {
    return <DockBranchView layout={layout} branch={node} />;
  }
  return null;
}

function structureHash(branch: DockBranch, children: DockNodeId[]): string {
  return `${branch.id}:${branch.orientation}:${children.join(",")}`;
}

function DockBranchView({ layout, branch }: { layout: DockLayout; branch: DockBranch }) {
  const setBranchSizes = useEditorStore((s) => s.setBranchSizes);
  const children = renderedChildIds(layout, branch);
  if (children.length === 0) {
    return null;
  }
  if (children.length === 1) {
    return <DockNodeView layout={layout} nodeId={children[0]} />;
  }

  const axis = branch.orientation === "horizontal" ? "width" : "height";
  const total = children.reduce((sum, id) => sum + (branch.sizes[id] ?? 0), 0);
  const defaultLayout: PanelLayout = {};
  for (const id of children) {
    defaultLayout[id] = total > 0 ? ((branch.sizes[id] ?? 0) / total) * 100 : 100 / children.length;
  }

  return (
    <ResizablePanelGroup
      key={structureHash(branch, children)}
      orientation={branch.orientation}
      defaultLayout={defaultLayout}
      onLayoutChanged={(next) => setBranchSizes(branch.id, next)}
      className="min-h-0 min-w-0"
    >
      {children.flatMap((id, index) => {
        const panel = (
          <ResizablePanel
            key={id}
            id={id}
            defaultSize={defaultLayout[id]}
            minSize={`${subtreeMinPx(layout, id, axis)}px`}
            className="min-h-0 min-w-0"
          >
            <DockNodeView layout={layout} nodeId={id} />
          </ResizablePanel>
        );
        return index === 0 ? [panel] : [<ResizableHandle key={`handle-${id}`} />, panel];
      })}
    </ResizablePanelGroup>
  );
}

function DockLeafView({ leaf }: { leaf: DockLeaf }) {
  const activatePanel = useEditorStore((s) => s.activatePanel);
  const closePanel = useEditorStore((s) => s.closePanel);
  const reorderTab = useEditorStore((s) => s.reorderTab);
  const movePanel = useEditorStore((s) => s.movePanel);

  // A locked leaf (the live-subsurface viewport/preview) has no strip chrome and accepts no
  // drops; its body is the host-claiming div the subsurface paints under.
  if (leaf.locked) {
    return (
      <div
        data-dock-leaf={leaf.id}
        data-dock-accepts-tabs="false"
        className="relative flex h-full min-h-0 min-w-0 flex-col"
      >
        <LeafBody
          leafId={leaf.id}
          tabs={leaf.tabs}
          activeTab={leaf.activeTab}
          className="overflow-hidden"
        />
      </div>
    );
  }

  return (
    <div data-dock-leaf={leaf.id} className="flex h-full min-h-0 min-w-0 flex-col bg-background">
      <TabStrip
        items={leaf.tabs.map((id) => ({
          id,
          title: panelTitle(id),
          closable: panelDef(id)?.closable ?? true,
        }))}
        activeId={leaf.activeTab}
        size="dock"
        containerProps={{ "data-dock-strip": "true" }}
        onActivate={(id) => activatePanel(id as DockPanelId)}
        onClose={(id) => closePanel(id as DockPanelId)}
        drag={{
          domain: "dock",
          leafId: leaf.id,
          onReorder: (id, index) => reorderTab(leaf.id, id as DockPanelId, index),
          onDrop: (id, target) => movePanel(id as DockPanelId, target),
        }}
      />
      <LeafBody leafId={leaf.id} tabs={leaf.tabs} activeTab={leaf.activeTab} />
    </div>
  );
}
