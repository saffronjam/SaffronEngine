+++
title = 'Assets panel & thumbnails'
weight = 8
+++

# Assets panel & thumbnails

The Assets panel is a folder tree plus a tile grid over the project's [asset catalog](../../scene-and-ecs/asset-catalog-in-scene/). Each tile shows a thumbnail and an editable name, and acts as a drag source for the inspector's [pickers](../asset-pickers-and-drag-drop/). Virtual folders group catalog entries without changing the imported files on disk.

Thumbnails are PNGs fetched over the control socket and cached as blob URLs, because the engine and the webview share no GPU context. The panel itself is a React component reading `store.assets`.

## Navigation

The panel splits into a resizable folder tree on the left and the tile grid on the right, under a shared toolbar. The tree is built client-side from the flat `assetFolders` path list: a pinned Root row plus one expandable row per virtual folder. Clicking a row navigates the grid there; navigating any other way (a folder tile, a breadcrumb, back/forward) expands the tree down to the current folder so the selection is never hidden in a collapsed branch.

The toolbar holds back/forward buttons over a history stack with browser semantics — navigating truncates the forward tail, and back/forward skip entries whose folder has since been deleted — and clickable breadcrumbs, one segment per level of the current path. Renaming a folder rewrites the affected history entries, so back never lands on a stale name.

## The tile grid

`AssetsPanel` lays the current folder out with a CSS grid (`repeat(auto-fill, minmax(72px, 1fr))`), so tiles reflow to the panel width. Each `AssetTile` is a 72px tile: a square thumbnail on top, an in-place name button beneath. Subfolders show as folder tiles. An empty catalog shows an import and drag-and-drop prompt instead. The list comes from the reconcile poll's `list-assets` refresh, re-fetched eagerly after imports, moves, renames, or deletes.

Each tile tracks a three-state fetch (`loading` / `ready` / `none`): while the `get-thumbnail` promise is outstanding it shows a `Loader2` spinner over a dimmed type icon, a resolve swaps in the image, and a reject (an unsupported type or a failed render) settles to the bare type icon — a missing thumbnail is not an error. A warm cache starts `ready`, so re-opening a folder never flashes the spinner. The `AssetPicker` swatch and the asset viewer's 512 preview use the same distinction.

Right-clicking the asset background opens commands for **New Folder** and **Import**. Right-clicking a folder — as a grid tile or a tree row — opens **New Folder**, **Rename**, and **Delete**; Delete removes the virtual folder and moves its assets back to Root. Right-clicking an asset opens **View** and **Delete**; Delete asks the engine for `asset-usages` first, shows an in-place confirmation with affected slots, then calls `delete-asset`. The Delete key on a focused tile or tree row (clicking one focuses it) starts the same delete flow as its context menu item; the key is the `assets.delete` binding (default Delete), rebindable in [Editor Settings](../editor-settings/).

Asset selection is local editor state. A click selects one tile, Ctrl-click toggles a tile, Shift-click adds the range from the last clicked tile through the current tile, and dragging on empty panel space draws a marquee that selects intersecting asset tiles. Clicking empty space clears the selection. Dragging any selected tile writes the selected asset ids into the asset drag payload, so a folder drop moves the whole selection.

## Detail panel

Selecting a single asset slides a detail overlay in from the right edge of the grid (`AssetMetadataPanel`). It shows the filename, location, type, on-disk size, vertex and triangle counts (meshes), and the file's modified time. The values come from `probe-asset`, which resolves the asset, reads `file_size`/`last_write_time`, and — for a mesh — the `.smesh` header's vertex/index counts (no full mesh load). The panel re-fetches when the selection changes and slides out when the selection is not exactly one asset.

## Thumbnails over the socket

A thumbnail travels as data rather than a registered descriptor, since no GPU context is shared. `get-thumbnail` renders the asset to a small offscreen and reads it back as a **base64 PNG** in the JSON result; the client decodes it to a `Blob` and an object URL:

- a **texture** asset reads its own decoded image back;
- a **mesh** asset renders a [3D preview](../mesh-thumbnails/);
- anything else, or a failed render, falls back to a Lucide type icon in the webview.

The readback is **right-sized to the requested `size`**, not the source resolution. A texture larger than the tile is downscaled on the GPU first — a chained linear halving (mip-style, so a 4k source is not undersampled by a single tap) down to a target that fits `size`×`size` while preserving aspect — and only that small image is copied back and PNG-encoded. So a 128 px thumbnail of a 4096×2048 HDR reads back ~128×64 pixels, not 8.4M, whatever the source. The reply's `width`/`height` report the actual PNG dimensions. `view-asset` (512) takes the same path through the same `size` parameter.

A thumbnail of an **HDR asset** is tonemapped, not clamped. An HDR's radiance runs well past 1.0, so the plain [0,1]×255 clamp used for already-display-range captures blew the preview out to white; the texture branch instead Reinhard-maps + gamma-encodes the small downscaled image (≈16k pixels, cheap on the CPU) so a sky preview shows sky-and-horizon detail. Mesh and material previews render to a display-range offscreen already, so they keep the clamp.

Each `get-thumbnail` call is a GPU→CPU readback plus a PNG encode, so it must not run per frame or per tile. A module-scope frontend cache keyed by asset id holds `{ blob URL, the px size it was fetched at }`, and concurrent requests for the same asset share one in-flight promise. A cached URL is reused synchronously whenever it is at least as large as the requested size, so folder navigation does not ask the backend for thumbnails it already decoded in the current webview session:

```ts
export async function getThumbnailUrl(assetId: string, size: number): Promise<string> {
  const cached = thumbnailCache.get(assetId);
  if (cached && cached.size >= size) return cached.url;     // hit
  const inflight = thumbnailInflight.get(assetId);
  if (inflight) return inflight;                            // dedupe
  // miss → get-thumbnail, decode base64 PNG, store the object URL
}
```

When the catalog changes, on a project or scene load, every cached blob URL is stale. `invalidateThumbnails` revokes them all, and the lazy cache re-fetches on demand.

### The engine's disk cache

The webview cache is per-session — wiped on every project load — so on its own a project with ~100 textures pays a full round of generation at every startup. Underneath it the **engine keeps a persistent PNG cache** that survives restarts, because the engine is the right owner: it knows the source files and when they change.

- **Location:** `<projectRoot>/cache/thumbnails/`, next to the project's `assets/` and outside it, so the catalog scan and project save/load never pick it up. One PNG per entry.
- **Key:** the asset uuid, the requested pixel size, and a *stamp* of the source file — its size and mtime, folded with a cache-format version into the filename `<uuid>-<size>-<stamp>.png`. A hit is a single `exists()` after stating the source; a stale entry (edited source, or a version bump) simply never matches its old stamp again, so there is no explicit invalidation step for edits. Bumping `THUMBNAIL_CACHE_VERSION` invalidates every entry at once when generation behaviour changes.
- **Flow:** `get-thumbnail` reads the cached PNG bytes straight into the base64 reply on a hit — no GPU work, no encode — and reports the width/height read from the PNG header. On a miss it generates as above and writes the PNG before replying (best-effort: a failed write logs and still replies). So the second start of a project is disk reads, with no multi-second HDR spike.

The stamp keys textures on their imported file and meshes on the `.smesh`; materials stamp on the resolved material state, so editing a parent material reflows every instance's key (see [asset commands](../../tooling-and-control/asset-commands/) for the `thumbnail-cache` command and the delete/orphan cleanup).

### Off the frame loop

A cold cache-miss still has to decode the source, upload it, and render — for a 4k HDR that is ~1 s of work that, run inside the per-frame control drain, would freeze the viewport. So generation runs on a **worker thread**. On a miss the engine enqueues a job and replies `pending: true` immediately; the worker decodes + uploads + renders + readbacks + writes the PNG to the disk cache, and `getThumbnailUrl` re-requests with backoff (keeping the loading spinner) until the retry is a plain disk-cache hit. Pending replies return in microseconds, so the editor's serialized control I/O never queues behind a generation. The worker owns its own Vulkan command pool and shares the graphics queue with the frame loop under a mutex; finished GPU textures/meshes are handed back to the main thread and folded into the in-session caches, so a later draw of the same asset reuses them. A persistent generation failure replies with an error, which settles the tile to the type icon rather than retrying forever.

## Rename in place

Double-clicking a tile's name turns it into an input bound to a draft string. Enter or blur commits with `rename-asset`; Escape cancels. The engine returns the new `{id, name}`, and the catalog refresh reflects it. Names are UTF-8, so non-Latin names round-trip through the project file; rendering them needs a broader font, a known follow-up.

```ts
void client.renameAsset(entry.id, next).then(() => refreshAssets());
```

## Import and drag-drop

The **Import** button opens the native file dialog (`rfd` XDG portal); the panel is also an **OS file-drop** target via the webview drag-drop event. Both route by extension: images go to `import-texture` (catalog-only, no spawn), everything else to `import-model`, matching the engine's `importToCatalog`. The OS file-drop is hit-tested against the panel's rect, scaled by `devicePixelRatio` because the drop position is in physical pixels. Dropping a model on the *viewport* therefore does not trigger a catalog import here.

Each tile is also an HTML5 drag *source* on a distinct channel, `application/x-se-asset`, separate from the OS file drop. It carries `{id, type}` so an inspector [picker](../asset-pickers-and-drag-drop/) can type-gate the drop. Folder tiles, tree rows, breadcrumb segments, and the current folder background accept the same payload and call `move-asset`.

## Asset tabs

Double-clicking a tile opens a closeable titlebar tab for that asset (`view-asset`, the same readback path as `get-thumbnail`). The pinned `Scene` tab owns the native viewport and cannot be closed. Asset tabs show a type icon and can be reordered by drag-drop.

## In the code

| What | File | Symbols |
|---|---|---|
| Tile grid + history + import + drop | `editor/src/panels/AssetsPanel.tsx` | `AssetsPanel`, `AssetPanelBody`, `Breadcrumbs`, `FolderTile`, `FolderNameInput`, `importPath`, `isInsidePanel` |
| Folder tree sidebar | `editor/src/panels/AssetFolderTree.tsx` | `AssetFolderTree`, `buildFolderTree`, `folderAncestorPaths` |
| Detail panel | `editor/src/components/AssetMetadataPanel.tsx` | `AssetMetadataPanel` |
| Tile + rename + drag source | `editor/src/components/AssetTile.tsx` | `AssetTile`, `RenameInput`, `ASSET_DND_MIME` |
| Thumbnail blob-URL cache | `editor/src/state/store.ts` | `getCachedThumbnailUrl`, `getThumbnailUrl`, `invalidateThumbnails`, `thumbnailCache` |
| Asset tabs | `editor/src/app/WindowTitlebar.tsx` | `TitlebarTab`, `ViewTab`, `openAssetTab` |
| Preview content | `editor/src/components/AssetViewer.tsx` | `AssetPreview`, `viewAsset` |
| Readback + metadata (engine) | `engine/crates/control/src/commands_asset.rs` | `get-thumbnail`, `view-asset`, `list-assets`, `probe-asset`, `rename-asset`, `move-asset`, `delete-asset`, `delete-asset-folder` |

## Related

- [Mesh thumbnails](../mesh-thumbnails/) — the 3D preview render behind a mesh tile
- [Asset pickers](../asset-pickers-and-drag-drop/) — the drop targets these tiles feed
- [Asset catalog in the scene](../../scene-and-ecs/asset-catalog-in-scene/) — the catalog this grid views
- [Asset commands](../../tooling-and-control/asset-commands/) — import/list/rename + the thumbnail readback
