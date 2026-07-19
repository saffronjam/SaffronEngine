+++
title = 'Dock system'
weight = 11
+++

# Dock system

The dock system arranges editor panels as resizable splits and tab groups. A user can reorder tabs, merge a panel into another group, split a leaf at an edge, or close and reopen a tool without losing its last useful location.

Scene tools and asset-editor tools occupy separate dock islands. Each island owns its own tree, panel identifiers, live viewport leaf, and persistence state.

## Layout tree

A `DockLayout` is a DOM-free tree. Branches describe horizontal or vertical splits with percentage sizes; leaves describe an ordered tab group and its active tab.

```ts
interface DockLayout {
  version: 2;
  rootId: DockNodeId;
  nodes: Record<DockNodeId, DockBranch | DockLeaf>;
}

interface DockLeaf {
  type: "leaf";
  tabs: DockPanelId[];
  activeTab: DockPanelId | null;
  locked?: boolean;
  persistent?: boolean;
}
```

`DockRoot` renders branches with [react-resizable-panels](https://github.com/bvaughn/react-resizable-panels). The pure tree functions insert, remove, reorder, split, and move panels. `normalize` deletes disposable empty leaves, collapses single-child branches, and merges redundant nesting. `validate` removes unknown panel identifiers from persisted data or rejects an incompatible tree.

A persistent leaf remains in the model when empty, but `isNodeRendered` collapses its screen region. This keeps a stable destination for reopening a panel without reserving blank space. Locked leaves always render because they expose a native viewport surface.

## Drag and drop

`useTabStripDrag` handles both titlebar tabs and dock tabs. It distinguishes clicks from drags with a 4-pixel threshold, snapshots tab centers for reorder previews, and uses a Web Animations API FLIP transition after a reorder.

A dock tab that leaves its strip vertically becomes a tear-out drag. The hook releases [pointer capture](https://www.w3.org/TR/pointerevents3/#dom-element-releasepointercapture), then `dockDrag` takes ownership through window-level pointer listeners. The source panel is removed only from the effective render layout during the gesture; the store tree changes once on a successful drop.

```mermaid
stateDiagram-v2
    [*] --> Pressed
    Pressed --> Click: release before 4 px
    Pressed --> Reorder: move within strip
    Reorder --> TearOut: leave strip vertically
    Reorder --> Commit: release in strip
    TearOut --> Commit: valid leaf target
    TearOut --> Cancel: Escape or invalid target
    Commit --> [*]
    Click --> [*]
    Cancel --> [*]
```

Pointer capture retargets pointer events to the captured element, so candidate leaves cannot rely on `pointerover`. `snapshotLeafRects` measures mounted `[data-dock-leaf]` elements, and `hitTestRects` tests the pointer against those rectangles. The drag ghost and drop overlay use `pointer-events: none` so they do not interfere with the test.

The center of a leaf merges the panel into its tab group. Its outer thirds create edge splits, while a strip target inserts at a computed tab index. The Move to context menu invokes the same merge and split operations without a drag gesture.

## Stable panel hosts

Moving a React component to another parent normally remounts it and discards local state. `DockPanelsHost` renders an open panel through React's [`createPortal`](https://react.dev/reference/react-dom/createPortal) into one host element owned by a module-level map. `LeafBody` moves that host element with `appendChild` and changes `display` for inactive tabs.

The React tree therefore keeps the same portal for the panel while its host moves between leaves. Registry entries choose `always` when hidden panels must stay mounted or `onlyWhenVisible` when an inactive panel may unmount. Closing a panel removes its host from the map.

## Locked viewport leaves

Each island has one locked leaf: `viewport` in the Scene island and `preview` in the asset editor. A locked leaf has no tab strip, rejects drops, and cannot be closed. Edge splits create siblings instead of placing web content over the native surface.

Minimum pixel sizes propagate from leaves through their ancestor branches. The Scene viewport contributes a 520-pixel minimum width and 200-pixel minimum height; the asset preview contributes 320 by 200 pixels. This prevents a resize from collapsing a live attachment region.

Every `dockLayouts` identity change schedules a forced `emitLayoutSettled`. The active viewport then recommits its native surface bounds. A hidden island has a degenerate host rectangle, which the bounds calculation ignores.

## Dock islands

The Scene and asset-editor panel identifiers form disjoint sets, and store actions choose a tree with `panelKind`. Drag hit-testing measures leaves in the mounted island, so a drop target comes from the same surface as its source panel.

The asset editor opens panels from the previewed asset's capabilities. `preview` is always present; a rig adds `skeleton`, clips add `clips` and `assetTimeline`, materials add `materialEdit`, and `assetStats` is available from the preview toolbar. Empty persistent leaves collapse until one of those panels opens.

## Persistence

The editor stores both trees and `lastLocation` under a key derived from the project path. Dock mutations debounce writes by 300 milliseconds. Closing or moving a panel records its leaf, and reopening resolves a destination in this order: remembered leaf, canonical default leaf, first unlocked leaf, then a new leaf beside the root.

Hydration validates each tree against its island's known panel identifiers and required structural panels. Invalid stored data falls back to the default layout. Without a loaded project, changes remain in the current session.

The default Scene tree keeps Hierarchy above Inspector on the left, gives the viewport the centre,
and places Environment, Render, and Post in one full-height right dock. A layout-version change
intentionally discards older persisted trees when the canonical panel ownership or placement changes.

## In the code

| What | File | Symbols |
|---|---|---|
| Pure tree model and defaults | `editor/src/state/dockLayout.ts` | `DockLayout`, `movePanel`, `normalize`, `validate`, `defaultSceneLayout`, `defaultAssetEditorLayout` |
| Dock store and persistence | `editor/src/state/store.ts` | `dockLayouts`, `lastLocation`, `openPanel`, `persistDockLayouts`, `hydrateDockLayouts` |
| Shared tab drag | `editor/src/components/dock/useTabStripDrag.ts` | `useTabStripDrag`, `TAB_DRAG_THRESHOLD_PX` |
| Tear-out targeting | `editor/src/components/dock/dockDrag.ts` | `beginDockDrag`, `moveDockDrag`, `cancelDockDrag`, `snapshotLeafRects`, `hitTestRects` |
| Stable portal hosts | `editor/src/components/dock/DockPanelsHost.tsx` | `DockPanelsHost`, `LeafBody`, `hostFor` |
| Tree renderer and panel policy | `editor/src/components/dock/DockRoot.tsx`, `panelRegistry.tsx` | `DockRoot`, `panelDef`, `SCENE_PANEL_REGISTRY`, `ASSET_EDITOR_PANEL_REGISTRY` |
| Asset-editor capabilities | `editor/src/panels/AssetEditorWorkspace.tsx` | `AssetEditorWorkspace`, `AssetPreviewProvider` |

## Related

- [Viewport compositing](../viewport-compositing/) — explains the native surfaces exposed through locked leaves.
- [Viewport panel](../viewport-panel/) — synchronizes Scene viewport bounds and interaction state.
- [Asset editor](../asset-editor/) — supplies the asset capabilities that open island panels.
- [Theme and fonts](../theme-and-fonts/) — defines the visual tokens used by dock surfaces.
