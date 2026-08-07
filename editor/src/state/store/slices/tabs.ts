import { client } from "../../../control/client";
import type {
  EditorState,
  GetEditorState,
  SetEditorState,
  TabNavHistory,
  TabSlice,
  ViewTab,
} from "../types";

/// Cap on retained cross-tab navigation entries; older ones drop off the front.
const TAB_HISTORY_CAP = 100;

/// The Scene tab — always present, never closable, the ultimate navigation fallback.
export const SCENE_TAB: ViewTab = {
  id: "scene",
  kind: "scene",
  title: "Scene",
  closable: false,
};

/// Fold a tab activation into a state patch: set the active tab, remember the one we left for the
/// close-to-last-active behaviour, and push the landed-on tab onto the navigation history
/// (truncating any forward tail). Re-activating the already-active tab records nothing.
function recordActivation(
  s: EditorState,
  patch: Partial<EditorState>,
  activeViewTabId: string,
): Partial<EditorState> {
  const previousActiveTabId =
    activeViewTabId === s.activeViewTabId ? s.previousActiveTabId : s.activeViewTabId;
  const viewTabs = patch.viewTabs ?? s.viewTabs;
  const tab = viewTabs.find((t) => t.id === activeViewTabId);
  let tabHistory: TabNavHistory = s.tabHistory;
  if (tab && s.tabHistory.entries[s.tabHistory.cursor]?.id !== activeViewTabId) {
    const entries = [...s.tabHistory.entries.slice(0, s.tabHistory.cursor + 1), tab].slice(
      -TAB_HISTORY_CAP,
    );
    tabHistory = { entries, cursor: entries.length - 1 };
  }
  return { ...patch, activeViewTabId, previousActiveTabId, tabHistory };
}

/// Open (or focus) a main tab, appending it only when it is not already present.
function openTab(s: EditorState, tab: ViewTab): Partial<EditorState> {
  const existing = s.viewTabs.some((t) => t.id === tab.id);
  return recordActivation(s, { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] }, tab.id);
}

export function createTabSlice(set: SetEditorState, get: GetEditorState): TabSlice {
  return {
    viewTabs: [SCENE_TAB],
    hoveredTabId: null,
    activeViewTabId: "scene",
    previousActiveTabId: null,
    tabHistory: { entries: [SCENE_TAB], cursor: 0 },

    setHoveredTabId: (hoveredTabId) => set({ hoveredTabId }),
    openImageViewerTab: (asset) =>
      set((s) =>
        openTab(s, {
          id: `imageViewer:${asset.id}`,
          kind: "imageViewer",
          assetId: asset.id,
          title: asset.name,
          assetType: asset.type,
          closable: true,
        }),
      ),
    openFlameTab: () =>
      set((s) =>
        openTab(s, { id: "flamegraph", kind: "flamegraph", title: "Flame graph", closable: true }),
      ),
    openStoreTab: () =>
      set((s) => openTab(s, { id: "store", kind: "store", title: "Store", closable: true })),
    openMaterialGraphTab: (materialId) =>
      set((s) =>
        openTab(s, {
          id: `materialGraph:${materialId}`,
          kind: "materialGraph",
          materialId,
          title: "Material graph",
          closable: true,
        }),
      ),
    openAssetEditorTab: (assetId, title) =>
      set((s) =>
        openTab(s, {
          id: `assetEditor:${assetId}`,
          kind: "assetEditor",
          assetId,
          title,
          closable: true,
        }),
      ),
    openAssetEditorForAsset: (assetId, fallbackName) => {
      const asset = get().assets.find((entry) => entry.id === assetId);
      if (asset?.type === "plant" || asset?.type === "biome" || asset?.type === "vegetation-map") {
        // Vegetation subjects have no model container; the workspace skips the preview.
        get().openAssetEditorTab(assetId, asset.name);
        return;
      }
      void (async () => {
        try {
          const model = await client.getAssetModel(assetId);
          get().openAssetEditorTab(model.mesh, model.name);
        } catch {
          get().openAssetEditorTab(assetId, fallbackName);
        }
      })();
    },
    closeViewTab: (id) =>
      set((s) => {
        if (id === "scene") {
          return {};
        }
        const index = s.viewTabs.findIndex((tab) => tab.id === id);
        const viewTabs = s.viewTabs.filter((tab) => tab.id !== id);
        const patch: Partial<EditorState> = { viewTabs };
        if (id in s.historyByTab) {
          const historyByTab = { ...s.historyByTab };
          delete historyByTab[id];
          patch.historyByTab = historyByTab;
        }
        if (s.activeViewTabId !== id) {
          return {
            ...patch,
            previousActiveTabId: s.previousActiveTabId === id ? null : s.previousActiveTabId,
          };
        }
        // Land on the last active tab if it is still open, otherwise the tab to the left, else Scene.
        const previous = s.previousActiveTabId;
        const target =
          previous && previous !== id && viewTabs.some((tab) => tab.id === previous)
            ? previous
            : (viewTabs[Math.max(0, index - 1)]?.id ?? "scene");
        return recordActivation(s, patch, target);
      }),
    setActiveViewTab: (id) =>
      set((s) => (s.viewTabs.some((tab) => tab.id === id) ? recordActivation(s, {}, id) : {})),
    navigateTabHistory: (step) =>
      set((s) => {
        const cursor = s.tabHistory.cursor + step;
        if (cursor < 0 || cursor >= s.tabHistory.entries.length) {
          return {};
        }
        const entry = s.tabHistory.entries[cursor];
        const reopened = !s.viewTabs.some((tab) => tab.id === entry.id);
        return {
          activeViewTabId: entry.id,
          viewTabs: reopened ? [...s.viewTabs, entry] : s.viewTabs,
          tabHistory: { ...s.tabHistory, cursor },
          previousActiveTabId: s.activeViewTabId,
        };
      }),
    moveViewTab: (id, index) =>
      set((s) => {
        if (id === "scene") {
          return {};
        }
        const moving = s.viewTabs.find((tab) => tab.id === id);
        if (!moving) {
          return {};
        }
        const without = s.viewTabs.filter((tab) => tab.id !== id);
        const nextIndex = Math.min(Math.max(1, index), without.length);
        return {
          viewTabs: [...without.slice(0, nextIndex), moving, ...without.slice(nextIndex)],
        };
      }),
  };
}
