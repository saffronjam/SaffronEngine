import { useCallback, useState, type Dispatch, type SetStateAction } from "react";
import { client } from "../../control/client";
import { errorText, notify } from "../../lib/flash";
import { folderAncestorPaths, folderLabel } from "../AssetFolderTree";
import {
  isFolderDescendant,
  normalizeFolderInput,
  parentFolderPath,
  replaceFolderPrefix,
  type FolderHistory,
} from "./catalog";
import type { FolderActionTarget } from "./AssetPanelBody";

/// An in-progress folder creation: grid-initiated shows the inline tile in the current folder,
/// tree-initiated shows an inline row under `parent` in the tree.
export interface CreatingFolder {
  parent: string | null;
  origin: "grid" | "tree";
}

/// Inline folder creation, folder/asset rename, and the drag-move of assets and folders. A folder
/// path IS its identity, so every move rewrites the navigation history and the selected paths
/// through the same prefix rewrite.
export function useFolderEditing({
  folders,
  refreshAssets,
  navigateTo,
  setHistory,
  rewriteSelectedFolderPaths,
}: {
  folders: string[];
  refreshAssets: () => Promise<void>;
  navigateTo: (folder: string | null) => void;
  setHistory: Dispatch<SetStateAction<FolderHistory>>;
  rewriteSelectedFolderPaths: (rewrite: (path: string) => string) => void;
}) {
  const [creatingFolder, setCreatingFolder] = useState<CreatingFolder | null>(null);
  const [creatingFolderName, setCreatingFolderName] = useState("");
  const [renamingFolder, setRenamingFolder] = useState<FolderActionTarget | null>(null);
  const [renamingAsset, setRenamingAsset] = useState<string | null>(null);
  const [folderError, setFolderError] = useState<string | null>(null);

  // Grid-initiated creation navigates to the parent so its inline tile is in view; tree-initiated
  // creation stays put and edits inline under the clicked row.
  const startCreateFolder = useCallback(
    (parent: string | null, origin: "grid" | "tree"): void => {
      if (origin === "grid") {
        navigateTo(parent);
      }
      setFolderError(null);
      setCreatingFolderName("");
      setCreatingFolder({ parent, origin });
    },
    [navigateTo],
  );

  const startRenameFolder = useCallback((folder: string, origin: "grid" | "tree"): void => {
    setFolderError(null);
    setRenamingFolder({ path: folder, origin });
  }, []);

  const endRenameAsset = useCallback((): void => {
    setRenamingAsset(null);
  }, []);

  const commitNewFolder = useCallback(
    async (name: string): Promise<void> => {
      const folder = normalizeFolderInput(name, creatingFolder?.parent ?? null);
      if (!folder) {
        setCreatingFolder(null);
        setCreatingFolderName("");
        setFolderError(null);
        return;
      }
      if (folders.includes(folder)) {
        setFolderError("Folder already exists");
        return;
      }
      try {
        for (const path of folderAncestorPaths(folder)) {
          if (!folders.includes(path)) {
            await client.createAssetFolder(path);
          }
        }
        await refreshAssets();
        setCreatingFolder(null);
        setCreatingFolderName("");
        setFolderError(null);
      } catch {
        setFolderError("Could not create folder");
      }
    },
    [creatingFolder, folders, refreshAssets],
  );

  const cancelNewFolder = useCallback((): void => {
    setCreatingFolder(null);
    setCreatingFolderName("");
    setFolderError(null);
  }, []);

  const commitRenameFolder = useCallback(
    async (folder: string, name: string): Promise<void> => {
      const next = normalizeFolderInput(name, parentFolderPath(folder));
      if (!next || next === folder) {
        setRenamingFolder(null);
        setFolderError(null);
        return;
      }
      if (folders.includes(next)) {
        setFolderError("Folder already exists");
        return;
      }
      try {
        const ancestors = folderAncestorPaths(next).slice(0, -1);
        for (const path of ancestors) {
          if (!folders.includes(path)) {
            await client.createAssetFolder(path);
          }
        }
        await client.renameAssetFolder(folder, next);
        await refreshAssets();
        setHistory((current) => ({
          ...current,
          stack: current.stack.map((entry) =>
            entry === null ? null : replaceFolderPrefix(entry, folder, next),
          ),
        }));
        setRenamingFolder(null);
        setFolderError(null);
      } catch {
        setFolderError("Could not rename folder");
      }
    },
    [folders, refreshAssets, setHistory],
  );

  const cancelRenameFolder = useCallback((): void => {
    setRenamingFolder(null);
    setFolderError(null);
  }, []);

  const commitNewFolderName = useCallback(
    (name: string): void => void commitNewFolder(name),
    [commitNewFolder],
  );

  const commitRenameFolderName = useCallback(
    (folder: string, name: string): void => void commitRenameFolder(folder, name),
    [commitRenameFolder],
  );

  const clearFolderError = useCallback((): void => setFolderError(null), []);

  const moveAssetsToFolder = useCallback(
    async (assetIds: string[], folder: string | null): Promise<void> => {
      if (assetIds.length === 0) {
        return;
      }
      await Promise.all(assetIds.map((assetId) => client.moveAsset(assetId, folder)));
      await refreshAssets();
    },
    [refreshAssets],
  );

  // Re-parent dragged folders (keeping each label) under `parent`; a folder cannot be dropped into
  // itself or its own subtree, and a clashing name is skipped.
  const moveFoldersTo = useCallback(
    async (paths: string[], parent: string | null): Promise<void> => {
      const taken = new Set(folders);
      const moves: { from: string; to: string }[] = [];
      for (const folder of paths) {
        const to = parent ? `${parent}/${folderLabel(folder)}` : folderLabel(folder);
        if (to === folder || parent === folder || (parent && isFolderDescendant(parent, folder))) {
          continue;
        }
        if (taken.has(to)) {
          notify(`A folder named ${folderLabel(folder)} already exists there`);
          continue;
        }
        moves.push({ from: folder, to });
        taken.add(to);
      }
      if (moves.length === 0) {
        return;
      }
      for (const move of moves) {
        try {
          await client.renameAssetFolder(move.from, move.to);
        } catch (err) {
          notify(`Could not move ${folderLabel(move.from)}: ${errorText(err)}`);
        }
      }
      await refreshAssets();
      const rewrite = (path: string): string =>
        moves.reduce((acc, move) => replaceFolderPrefix(acc, move.from, move.to), path);
      setHistory((current) => ({
        ...current,
        stack: current.stack.map((entry) => (entry === null ? null : rewrite(entry))),
      }));
      rewriteSelectedFolderPaths(rewrite);
    },
    [folders, refreshAssets, rewriteSelectedFolderPaths, setHistory],
  );
  return {
    creatingFolder,
    creatingFolderName,
    setCreatingFolderName,
    renamingFolder,
    renamingAsset,
    setRenamingAsset,
    folderError,
    startCreateFolder,
    startRenameFolder,
    endRenameAsset,
    commitNewFolderName,
    cancelNewFolder,
    commitRenameFolderName,
    cancelRenameFolder,
    clearFolderError,
    moveAssetsToFolder,
    moveFoldersTo,
  };
}
