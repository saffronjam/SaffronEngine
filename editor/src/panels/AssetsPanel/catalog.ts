import { client } from "../../control/client";
import { errorText, notifyError } from "../../lib/flash";
import { useEditorStore } from "../../state/store";
import type { AssetEntry } from "../../protocol";
import type { AssetSortMode } from "../../state/store";

/// Extensions that import as a catalog texture; everything else imports as a model.
const TEXTURE_EXTS = new Set(["png", "jpg", "jpeg", "hdr", "tga", "bmp"]);
const VEGETATION_EXTS = new Set(["splant", "sbiome", "svegmap"]);

/// Extensions offered in the file dialog, grouped per filter entry.
export const MODEL_EXTS = ["gltf", "glb", "obj", "smesh"];
export const IMAGE_EXTS = ["png", "jpg", "jpeg", "hdr", "tga", "bmp"];
export const NATIVE_VEGETATION_EXTS = ["splant", "sbiome", "svegmap"];

export function extensionOf(path: string): string {
  const dot = path.lastIndexOf(".");
  return dot >= 0 ? path.slice(dot + 1).toLowerCase() : "";
}

export function parentFolderPath(folder: string): string | null {
  const slash = folder.lastIndexOf("/");
  return slash >= 0 ? folder.slice(0, slash) : null;
}

export function isFolderDescendant(candidate: string, folder: string): boolean {
  return (
    candidate.length > folder.length &&
    candidate.startsWith(folder) &&
    candidate[folder.length] === "/"
  );
}

export function replaceFolderPrefix(value: string, from: string, to: string): string {
  if (value === from) {
    return to;
  }
  return isFolderDescendant(value, from) ? `${to}${value.slice(from.length)}` : value;
}

/// Resolve typed folder text against a parent, rejecting empty or blank-segment input.
export function normalizeFolderInput(input: string, parent: string | null): string | null {
  const trimmed = input.trim().replaceAll("\\", "/");
  if (!trimmed) {
    return null;
  }
  const segments = trimmed.split("/");
  if (segments.some((segment) => segment.trim().length === 0)) {
    return null;
  }
  const relative = segments.map((segment) => segment.trim()).join("/");
  return parent ? `${parent}/${relative}` : relative;
}

export function assetCountLabel(count: number): string {
  return `${count} asset${count === 1 ? "" : "s"}`;
}

export function deleteFolderBody(folder: string): string {
  const count = useEditorStore
    .getState()
    .assets.filter(
      (asset) => asset.folder === folder || isFolderDescendant(asset.folder ?? "", folder),
    ).length;
  return `Moves ${count} asset${count === 1 ? "" : "s"} to Root.`;
}

/// Folder navigation history with browser semantics: navigating truncates the forward tail;
/// back/forward skip entries whose folder no longer exists.
export interface FolderHistory {
  stack: (string | null)[];
  index: number;
}

/// The nearest history entry in `step` direction that still exists; -1 if none.
export function nearestHistoryIndex(
  history: FolderHistory,
  folders: string[],
  step: -1 | 1,
): number {
  for (let i = history.index + step; i >= 0 && i < history.stack.length; i += step) {
    const entry = history.stack[i];
    if (entry === null || folders.includes(entry)) {
      return i;
    }
  }
  return -1;
}

export type FolderItem =
  | { kind: "folder"; path: string; label: string; sortName: string }
  | { kind: "new"; path: string; label: string; sortName: string };

/// The immediate child folders of `currentFolder`, name-filtered and sorted, with the inline
/// creation tile sorted last.
export function sortedFolderItems(
  folders: string[],
  currentFolder: string | null,
  creatingFolder: boolean,
  filter = "",
): FolderItem[] {
  const parentPrefix = currentFolder ? `${currentFolder}/` : "";
  const items: FolderItem[] = folders.flatMap((folder) => {
    if (currentFolder) {
      if (!folder.startsWith(parentPrefix)) {
        return [];
      }
      const rest = folder.slice(parentPrefix.length);
      if (!rest || rest.includes("/")) {
        return [];
      }
      if (filter && !rest.toLowerCase().includes(filter)) {
        return [];
      }
      return [{ kind: "folder", path: folder, label: rest, sortName: rest }];
    }
    if (folder.includes("/")) {
      return [];
    }
    if (filter && !folder.toLowerCase().includes(filter)) {
      return [];
    }
    return [{ kind: "folder", path: folder, label: folder, sortName: folder }];
  });
  if (creatingFolder) {
    items.push({ kind: "new", path: "\uffff", label: "", sortName: "\uffff" });
  }
  return items.sort((a, b) =>
    a.sortName.localeCompare(b.sortName, undefined, { numeric: true, sensitivity: "base" }),
  );
}

const byName = (a: AssetEntry, b: AssetEntry): number =>
  a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });

/// Order assets for the grid. Ties on creation time fall back to name so the order is
/// deterministic across restarts. Folders sort separately, always by name.
export function sortAssets(assets: AssetEntry[], mode: AssetSortMode): AssetEntry[] {
  const sorted = [...assets];
  switch (mode) {
    case "name-asc":
      sorted.sort(byName);
      break;
    case "name-desc":
      sorted.sort((a, b) => byName(b, a));
      break;
    case "created-desc":
      sorted.sort((a, b) => b.createdAt - a.createdAt || byName(a, b));
      break;
    case "created-asc":
      sorted.sort((a, b) => a.createdAt - b.createdAt || byName(a, b));
      break;
  }
  return sorted;
}

async function moveImportedAsset(assetId: string, folder: string): Promise<void> {
  if (assetId !== "0") {
    await client.moveAsset(assetId, folder);
  }
}

/// Import one file by extension: images become catalog textures (no spawn), native vegetation
/// assets import in place, everything else imports as a model (which spawns and selects).
export async function importPath(path: string, folder: string | null): Promise<void> {
  try {
    if (TEXTURE_EXTS.has(extensionOf(path))) {
      const imported = await client.importTexture(path);
      if (folder) {
        await client.moveAsset(imported.texture, folder);
      }
    } else if (VEGETATION_EXTS.has(extensionOf(path))) {
      await client.importVegetationAsset(path, folder ?? undefined);
    } else {
      const imported = await client.importModel(path);
      if (folder) {
        await moveImportedAsset(imported.id, folder);
      }
    }
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Hit-test a physical-pixel drop position against the Assets panel's DOM rect. The drop event
/// reports physical pixels while `getBoundingClientRect` is CSS pixels.
export function isInsidePanel(position: { x: number; y: number }): boolean {
  const el = document.querySelector('[data-asset-panel="true"]');
  if (!el) {
    return false;
  }
  const rect = el.getBoundingClientRect();
  const scale = window.devicePixelRatio || 1;
  const cssX = position.x / scale;
  const cssY = position.y / scale;
  return cssX >= rect.left && cssX <= rect.right && cssY >= rect.top && cssY <= rect.bottom;
}
