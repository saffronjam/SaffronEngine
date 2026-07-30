import { getDockDrag } from "../../../components/dock/dockDrag";
import {
  DEFAULT_LEAF,
  type DockLayout,
  type DockPanelId,
  type DockSpaceKind,
  allOpenPanels,
  defaultDockLayout,
  defaultDockLayouts,
  findPanelLeaf,
  hasNode,
  hasRequiredPanels,
  isPanelOpenIn,
  knownPanelIds,
  movePanel as movePanelInLayout,
  normalize,
  openPanelResolve,
  panelKind,
  pruneLastLocation,
  removePanel,
  reorderTab as reorderTabInLayout,
  resetLayoutPreservingOpen,
  setBranchSizes as setBranchSizesInLayout,
  setLeafActiveTab,
  validate as validateLayout,
} from "../../dockLayout";
import { loadDockLayouts } from "../persistence";
import type { DockSlice, EditorState, SetEditorState } from "../types";

/// True when a panel is open in its island's tree, in any leaf, active or not. The metrics poll
/// keys on *open*, not *active*, so a hidden-but-mounted Stats panel keeps polling.
export function isPanelOpen(state: EditorState, id: DockPanelId): boolean {
  return isPanelOpenIn(state.dockLayouts[panelKind(id)], id);
}

export function createDockSlice(set: SetEditorState): DockSlice {
  return {
    dockLayouts: defaultDockLayouts(),
    lastLocation: {},

    openPanel: (id) =>
      set((s) => {
        const kind = panelKind(id);
        const resolved = openPanelResolve(s.dockLayouts[kind], id, {
          defaultLeafId: DEFAULT_LEAF[id],
          lastLeafId: s.lastLocation[id],
        });
        return { dockLayouts: { ...s.dockLayouts, [kind]: resolved.layout } };
      }),
    closePanel: (id) =>
      set((s) => {
        const kind = panelKind(id);
        const leafId = findPanelLeaf(s.dockLayouts[kind], id);
        if (leafId === null) {
          return {};
        }
        const layout = normalize(removePanel(s.dockLayouts[kind], id));
        return {
          dockLayouts: { ...s.dockLayouts, [kind]: layout },
          lastLocation: { ...s.lastLocation, [id]: leafId },
        };
      }),
    activatePanel: (id) =>
      set((s) => {
        const kind = panelKind(id);
        const layout = setLeafActiveTab(s.dockLayouts[kind], id);
        return layout === s.dockLayouts[kind]
          ? {}
          : { dockLayouts: { ...s.dockLayouts, [kind]: layout } };
      }),
    movePanel: (id, target) =>
      set((s) => {
        const kind = panelKind(id);
        const layout = movePanelInLayout(s.dockLayouts[kind], id, target);
        const leafId = findPanelLeaf(layout, id);
        return {
          dockLayouts: { ...s.dockLayouts, [kind]: layout },
          lastLocation: leafId ? { ...s.lastLocation, [id]: leafId } : s.lastLocation,
        };
      }),
    reorderTab: (leafId, id, index) =>
      set((s) => {
        const kind = panelKind(id);
        return {
          dockLayouts: {
            ...s.dockLayouts,
            [kind]: reorderTabInLayout(s.dockLayouts[kind], leafId, id, index),
          },
        };
      }),
    setBranchSizes: (branchId, sizes) =>
      set((s) => {
        // A torn drag renders an effective (panel-subtracted) layout whose collapse remounts an rrp
        // group and fires `onLayoutChanged` with transient sizes — those must never reach the real
        // tree; real resizes resume the moment the drag clears.
        if (getDockDrag() !== null) {
          return {};
        }
        const kind: DockSpaceKind = hasNode(s.dockLayouts.scene, branchId)
          ? "scene"
          : "assetEditor";
        return {
          dockLayouts: {
            ...s.dockLayouts,
            [kind]: setBranchSizesInLayout(s.dockLayouts[kind], branchId, sizes),
          },
        };
      }),
    resetDockLayout: () =>
      set((s) => ({
        dockLayouts: {
          scene: resetLayoutPreservingOpen("scene", allOpenPanels(s.dockLayouts.scene)),
          assetEditor: resetLayoutPreservingOpen(
            "assetEditor",
            allOpenPanels(s.dockLayouts.assetEditor),
          ),
        },
        lastLocation: {},
      })),
    hydrateDockLayouts: () =>
      set((s) => {
        const loaded = loadDockLayouts(s.project?.path);
        if (!loaded) {
          return {};
        }
        // Reject a tree that lacks a structural panel rather than migrating it.
        const accept = (raw: DockLayout | undefined, kind: DockSpaceKind): DockLayout => {
          const validated = raw ? validateLayout(raw, knownPanelIds(kind)) : null;
          return validated && hasRequiredPanels(validated, kind)
            ? validated
            : defaultDockLayout(kind);
        };
        const scene = accept(loaded.layouts?.scene, "scene");
        const assetEditor = accept(loaded.layouts?.assetEditor, "assetEditor");
        const lastLocation = {
          ...pruneLastLocation(scene, loaded.lastLocation),
          ...pruneLastLocation(assetEditor, loaded.lastLocation),
        };
        return { dockLayouts: { scene, assetEditor }, lastLocation };
      }),
  };
}
