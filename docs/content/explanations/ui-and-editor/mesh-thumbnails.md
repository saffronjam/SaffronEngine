+++
title = 'Mesh thumbnails'
weight = 11
+++

# Mesh thumbnails

Mesh thumbnails are rendered asset previews, not static type icons. They use the same [forward+ scene graph](../../lighting-and-brdf/clustered-forward/) as the interactive asset preview, then cross the control socket as base64-encoded PNG data.

The thumbnail system also handles models, materials, texture maps, and HDRIs. Each asset kind becomes a small furnished scene on an offscreen renderer view.

## Preview subjects

`request_thumbnail` classifies the requested asset into a `PreviewRenderKind`. The control layer maps that kind to a `PreviewSubject` when the host drains the render queue:

| Asset kind | Preview subject |
|---|---|
| Mesh | The mesh with one default material slot |
| Model | The instantiated model forest with its materials and node transforms |
| Material | The material on the built-in sphere, including displacement |
| Texture | The texture on a sphere through a material matching its semantic role |
| HDRI | A chrome ball lit by and reflecting the HDRI |

Animation assets have no thumbnail subject and fall back to their type icon in the Assets panel.

`build_preview_scene_for_thumbnail` creates a throwaway scene with no floor. Meshes, models, materials, and ordinary textures use a procedural environment with a key light. An HDRI supplies image-based lighting to its chrome ball, while the thumbnail view keeps the fixed studio gradient as the visible background.

## Framing

The preview builder computes the world-space axis-aligned bounds of every renderable node in the subject forest. It derives a center and bounding-sphere radius, using a radius of 1 for degenerate or unresolved bounds.

```text
distance = radius / tan(verticalFov / 2) * margin
eye = center + normalize(1, 0.7, 1) * distance
```

The eye vector produces the three-quarter view. Subject-specific margins frame a smooth HDRI ball most tightly, leave displacement headroom for material and texture spheres, and give arbitrary mesh bounds more room. Near and far planes expand from the resulting distance and radius.

## Main-graph render

Queued jobs render on the engine's render thread through `ViewId::Thumbnail`. The view owns separate targets and temporal state, so the Scene and asset-preview views retain their accumulated histories. A separate preview IBL state also prevents a thumbnail environment bake from replacing the project's lighting state.

```mermaid
flowchart LR
    A[PreviewRenderJob] --> B[Build throwaway scene]
    B --> C[Select Thumbnail view]
    C --> D[Render 8 convergence frames]
    D --> E[Read post-processed offscreen]
    E --> F[Encode PNG]
    F --> G[Write content cache]
    G --> H[Restore prior view without reset]
```

Eight frames let temporal effects settle before readback. The render disables the editor grid and camera models but otherwise uses the scene pipeline, materials, post-processing, and the requested square extent. Restoring the previous active view uses `restore_active_view_no_reset`, which avoids clearing its temporal resources.

`encode_active_offscreen_png` waits for a safe readback, converts the post-processed framebuffer to RGB, and encodes it with the Rust `image` crate's PNG encoder. The reply reports dimensions read from the encoded image rather than echoing the request.

## Pending requests

`get-thumbnail` defaults to 128 pixels and `view-asset` defaults to 512. A disk-cache hit returns PNG data immediately. A miss inserts a deduplicated `PreviewRenderJob` and returns a pending response with empty image data.

The host renders at most two queued previews in one update tick because each job executes eight convergence frames. After the PNG reaches the cache, the editor's next request becomes a cache hit. Rendering and readback stay on the render thread; the pending control response keeps the initial request from blocking on that work.

The editor retries a pending response after 60 milliseconds, doubles the delay after each miss, and caps it at 1 second. A rejected request settles the tile to its asset-type icon without an error toast.

## Content-addressed cache

Cache files live in the app-level thumbnail directory and use this shape:

```text
v<THUMBNAIL_CACHE_VERSION>-<contentHash>-<size>.png
```

Mesh, texture, and model entries use their catalog content hash. A model hash covers mesh chunks, node transforms, material state, and referenced texture bytes. Materials use a live hash of resolved parameters, texture identifiers, shader, and blend mode, so changing a parent material also changes an instance's key.

The cache is shared across projects and asset identifiers that resolve to the same content. Writes cap it at 1 GiB; crossing the cap removes oldest files until usage reaches 80 percent. `thumbnail-cache` reports entry count and bytes or clears the directory.

Changing `THUMBNAIL_CACHE_VERSION` changes the filename prefix for every asset kind. Old files then age out through the same size-cap eviction.

## Browser cache

`AssetTile` requests a 128-pixel image and displays it in a 72-pixel square. `getThumbnailUrl` converts the base64 payload to a `Blob`, creates an object URL, and caches the URL with its fetched size. A cached image satisfies any request for the same asset at an equal or smaller size.

The in-flight map lets concurrent consumers share one retry loop per asset. Replacing a cached image revokes its prior object URL. Project or catalog replacement calls `invalidateThumbnails`, which revokes every client URL; the content-addressed disk cache remains available to the next request.

## In the code

| What | File | Symbols |
|---|---|---|
| Request classification and disk cache | `engine/crates/assets/src/thumbnail.rs` | `request_thumbnail`, `PreviewRenderKind`, `PreviewRenderJob`, `THUMBNAIL_CACHE_VERSION`, `write_thumbnail_cache` |
| Preview scene and framing | `engine/crates/control/src/commands_asset.rs` | `PreviewSubject`, `build_preview_scene_for_thumbnail`, `compute_preview_bounds`, `frame_preview_camera` |
| Queue drain and convergence | `engine/crates/host/src/layer.rs` | `HostLayer::drive_preview_render_queue`, `render_preview_scene_to_png` |
| Offscreen view isolation | `engine/crates/rendering/src/renderer.rs` | `ViewId::Thumbnail`, `scene_ibl`, `encode_active_offscreen_png`, `restore_active_view_no_reset` |
| PNG conversion | `engine/crates/rendering/src/thumbnail.rs` | `convert_to_rgb`, `encode_to_png`, `ThumbnailPng` |
| Thumbnail commands | `engine/crates/control/src/commands_asset.rs` | `thumbnail_result`, `get-thumbnail`, `view-asset`, `thumbnail-cache` |
| Blob URL cache and retries | `editor/src/state/store.ts` | `getThumbnailUrl`, `getCachedThumbnailUrl`, `base64ToBlob`, `invalidateThumbnails` |
| Grid tile display | `editor/src/components/AssetTile.tsx` | `AssetTile`, `THUMBNAIL_FETCH_SIZE`, `ThumbnailLoading` |

## Related

- [Assets panel and thumbnails](../assets-panel-and-thumbnails/) — explains grid loading, selection, and asset details.
- [Asset editor](../asset-editor/) — uses the interactive version of the preview scene.
- [Material graph live preview](../material-graph-live-preview/) — updates material assets shown by the preview path.
- [Asset commands](../../tooling-and-control/asset-commands/) — documents thumbnail and larger-view requests.
- [Tonemap and exposure](../../screen-space-and-post/tonemap-and-exposure/) — explains the display transform applied before PNG readback.
