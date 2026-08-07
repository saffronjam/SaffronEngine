import { client } from "../../../control/client";
import { loadAssetSort, persistAssetSort } from "../persistence";
import { entityCreationEdit } from "./history";
import type { AssetSlice, EditorState, GetEditorState, SetEditorState } from "../types";

export function createAssetSlice(set: SetEditorState, get: GetEditorState): AssetSlice {
  return {
    assets: [],
    assetFolders: [],
    selectedAssetIds: new Set<string>(),
    selectedFolderPaths: new Set<string>(),
    assetSelectionAnchor: null,
    assetMarqueeActive: false,
    assetMenuTarget: null,
    assetsPanelHovered: false,
    assetsFolderNav: null,
    assetSort: loadAssetSort(),
    catalogDrag: null,

    setAssetList: (assets, assetFolders) =>
      set((s) => ({
        assets,
        assetFolders,
        viewTabs: s.viewTabs.map((tab) => {
          if (tab.kind === "assetEditor") {
            const model = assets.find((entry) => entry.id === tab.assetId);
            return model ? { ...tab, title: model.name } : tab;
          }
          if (tab.kind !== "imageViewer") {
            return tab;
          }
          const asset = assets.find((entry) => entry.id === tab.assetId);
          return asset ? { ...tab, title: asset.name, assetType: asset.type } : tab;
        }),
      })),
    refreshAssets: async () => {
      try {
        const list = await client.listAssets();
        get().setAssetList(list.assets, list.folders);
      } catch {
        // Engine may be briefly busy; the next reconcile tick recovers.
      }
    },
    // These let a rejected control call propagate so the calling panel surfaces it through
    // notifyError; they refresh the catalog on success.
    instantiateModel: async (modelId, name) => {
      const entity = await client.instantiateModel(modelId, name);
      if (entity.id) {
        get().pushEdit(entityCreationEdit(entity.id, "Add to scene"), "scene");
      }
      return entity.id ?? null;
    },
    extractSubAsset: async (modelId, subAssetId) => {
      await client.extractSubAsset(modelId, subAssetId);
      await get().refreshAssets();
    },
    clearExtraction: async (modelId, subAssetId) => {
      await client.clearExtraction(modelId, subAssetId);
      await get().refreshAssets();
    },
    scanAssets: async () => {
      await client.scanAssets();
      await get().refreshAssets();
    },
    reimportModel: async (modelId) => {
      await client.reimportModel(modelId);
      await get().refreshAssets();
    },
    selectAssetGridItem: (kind, key, modifiers, gridOrder) =>
      set((s) => {
        const index = gridOrder.findIndex((item) => item.kind === kind && item.key === key);
        if (index < 0) {
          return {};
        }
        const anchor = s.assetSelectionAnchor;
        if (modifiers.shift && anchor) {
          const anchorIndex = gridOrder.findIndex(
            (item) => item.kind === anchor.kind && item.key === anchor.key,
          );
          if (anchorIndex >= 0) {
            const range = gridOrder.slice(
              Math.min(anchorIndex, index),
              Math.max(anchorIndex, index) + 1,
            );
            const selectedAssetIds = new Set(s.selectedAssetIds);
            const selectedFolderPaths = new Set(s.selectedFolderPaths);
            for (const item of range) {
              (item.kind === "asset" ? selectedAssetIds : selectedFolderPaths).add(item.key);
            }
            return { selectedAssetIds, selectedFolderPaths, assetSelectionAnchor: { kind, key } };
          }
        }
        if (modifiers.toggle) {
          const next = new Set(kind === "asset" ? s.selectedAssetIds : s.selectedFolderPaths);
          if (!next.delete(key)) {
            next.add(key);
          }
          return {
            ...(kind === "asset" ? { selectedAssetIds: next } : { selectedFolderPaths: next }),
            assetSelectionAnchor: { kind, key },
          };
        }
        return {
          selectedAssetIds: kind === "asset" ? new Set([key]) : new Set<string>(),
          selectedFolderPaths: kind === "folder" ? new Set([key]) : new Set<string>(),
          assetSelectionAnchor: { kind, key },
        };
      }),
    setAssetSelection: (assetIds, folderPaths) =>
      set((s) => {
        if (
          assetIds.length === 0 &&
          folderPaths.length === 0 &&
          s.selectedAssetIds.size === 0 &&
          s.selectedFolderPaths.size === 0 &&
          s.assetSelectionAnchor === null
        ) {
          return {};
        }
        const lastAsset = assetIds.at(-1);
        const lastFolder = folderPaths.at(-1);
        return {
          selectedAssetIds: new Set(assetIds),
          selectedFolderPaths: new Set(folderPaths),
          assetSelectionAnchor: lastAsset
            ? { kind: "asset", key: lastAsset }
            : lastFolder
              ? { kind: "folder", key: lastFolder }
              : null,
        };
      }),
    pruneAssetSelection: (visibleAssets, folders) =>
      set((s) => {
        const visibleIds = new Set(visibleAssets.map((asset) => asset.id));
        const folderSet = new Set(folders);
        const patch: Partial<EditorState> = {};
        const assetIds = [...s.selectedAssetIds].filter((id) => visibleIds.has(id));
        if (assetIds.length !== s.selectedAssetIds.size) {
          patch.selectedAssetIds = new Set(assetIds);
        }
        const folderPaths = [...s.selectedFolderPaths].filter((path) => folderSet.has(path));
        if (folderPaths.length !== s.selectedFolderPaths.size) {
          patch.selectedFolderPaths = new Set(folderPaths);
        }
        const anchor = s.assetSelectionAnchor;
        if (
          anchor &&
          (anchor.kind === "asset" ? !visibleIds.has(anchor.key) : !folderSet.has(anchor.key))
        ) {
          patch.assetSelectionAnchor = null;
        }
        return patch;
      }),
    removeFromAssetSelection: (assetIds) =>
      set((s) => {
        const next = new Set([...s.selectedAssetIds].filter((id) => !assetIds.has(id)));
        return next.size === s.selectedAssetIds.size ? {} : { selectedAssetIds: next };
      }),
    rewriteSelectedFolderPaths: (rewrite) =>
      set((s) => {
        let changed = false;
        const next = new Set<string>();
        for (const path of s.selectedFolderPaths) {
          const to = rewrite(path);
          changed ||= to !== path;
          next.add(to);
        }
        return changed ? { selectedFolderPaths: next } : {};
      }),
    setAssetMarqueeActive: (assetMarqueeActive) => set({ assetMarqueeActive }),
    setAssetMenuTarget: (assetMenuTarget) => set({ assetMenuTarget }),
    setAssetsPanelHovered: (assetsPanelHovered) => set({ assetsPanelHovered }),
    setAssetsFolderNav: (assetsFolderNav) => set({ assetsFolderNav }),
    setAssetSort: (assetSort) => {
      persistAssetSort(assetSort);
      set({ assetSort });
    },
    setCatalogDrag: (catalogDrag) => set({ catalogDrag }),
  };
}
