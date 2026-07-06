+++
title = 'Mesh thumbnails'
weight = 11
+++

# Mesh thumbnails

A mesh thumbnail is a small rendered 3/4 preview of an asset, drawn from the mesh itself rather than a
generic icon. The mesh is rendered once into a tiny offscreen image with a camera auto-framed to its
bounds, read back as a base64 PNG, and shown in the webview as an `<img>`. The render is engine-side;
the transport is the control socket.

## How it works

The preview camera is placed from the mesh's bounding box so any mesh fills the frame regardless of
size. `mesh_bounds` finds the center and a bounding radius, then `framed_view_proj` backs the camera
off by the distance that fits that radius in the field of view:

```rust
let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
let mut radius = (mesh.bounds_max - mesh.bounds_min).length() * 0.5;
if radius <= 0.0001 { radius = 1.0; }

let fovy = 45.0_f32.to_radians();
let distance = radius / (fovy * 0.5).tan() * 1.3;
let eye = center + Vec3::new(1.0, 0.7, 1.0).normalize() * distance;
```

The eye offset `(1, 0.7, 1)` gives the canonical 3/4 view. The `1.3` factor leaves a margin so the
mesh does not touch the edges, and the degenerate-radius guard keeps a flat or point mesh from putting
the camera on top of it. Near and far planes are derived from the framing distance for a tight depth
range, and the projection's Y is flipped to match the viewport convention so the thumbnail comes out
upright.

## A one-shot render

The thumbnail pipeline is deliberately bare: vertex position, normal, and uv in; a two-matrix push
constant (MVP and a normal matrix); no descriptor sets, no lighting, no materials. The mesh shows in a
flat color. The render is multisampled at the highest count the device supports (up to 8x) — at
thumbnail sizes geometry edges alias hard without it, and a one-shot tiny render makes the extra
samples free in practice. The pass draws into a transient MSAA target and resolves into the 1x image
that gets read back; the sample count is independent of the viewport's [AA mode](../../anti-aliasing/aa-modes/).
`render_to_texture` records a one-time-submit command buffer through dynamic rendering — clear, then
the closure paints the shared backdrop gradient (below) and binds the pipeline, pushes the matrices,
and draws each submesh:

```rust
raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.handle());
raw.cmd_push_constants(cmd, pipeline.layout(), vk::ShaderStageFlags::VERTEX, 0,
    bytemuck::bytes_of(&push));
raw.cmd_bind_vertex_buffers(cmd, 0, &[mesh.vertex_buffer()], &[0]);
raw.cmd_bind_index_buffer(cmd, mesh.index_buffer(), 0, vk::IndexType::UINT32);
for submesh in &mesh.submeshes {
    raw.cmd_draw_indexed(cmd, submesh.index_count, 1,
        submesh.first_index, submesh.vertex_offset, 0);
}
```

The Vulkan calls go through `ash`'s raw device seam; the render is synchronous — submit, then wait the
fence. Thumbnails are built lazily and once, between frames on the
[control-drain step, off the present path](../../tooling-and-control/asset-commands/), so the wait is
acceptable. The pipeline is built on first use and cached (`ensure_thumbnail_pipeline`), so the second
thumbnail reuses it. A texture asset skips the render and copies its decoded image straight back.

## Model thumbnails are textured

A bare mesh shows in a flat color, but a [`.smodel`](../../geometry-and-assets/smodel-container/) tile
shows the model as it looks — its mesh shaded with the embedded materials. `render_model_thumbnail`
frames the mesh exactly as above, then draws each submesh through the **material-preview pipeline**
(`preview.slang`, the same studio-lit shader the [material preview](../../materials-and-pipelines/native-materials/)
uses) with that submesh's material from the table, indexed by `Submesh.material_slot`:

```rust
let pushes: Vec<(Submesh, PreviewPush)> = mesh.submeshes.iter().map(|submesh| {
    let idx = (submesh.material_slot as usize).min(submesh_materials.len() - 1);
    (*submesh, preview_push(&submesh_materials[idx], view_proj))
}).collect();
// in the record closure: bind the bindless set, then per submesh push + draw_indexed
```

The materials and their textures live as chunks of the container, so the thumbnail worker has no
`AssetServer`: the main thread resolves them at enqueue — the mesh chunk into bytes, each material
into a `MaterialAsset`, and each referenced texture's chunk into bytes — and the
worker decodes from memory, uploads, and builds the `SubmeshMaterial` table. `THUMBNAIL_CACHE_VERSION`
prefixes every cache filename (`v<VERSION>-<contentHash>-<size>.png`), so bumping that one number
retires the whole on-disk cache — every kind, not just materials — and the tiles regenerate.

## A shared backdrop and matched color

Every 3D-render tile — a bare mesh, a textured model, a material or texture sphere, an HDRI chrome
ball — draws over one shared studio backdrop, so the grid reads as a set rather than four different
near-black squares. The backdrop is a fullscreen-triangle vertical gradient (`thumbnail_bg.slang`,
lighter at the top), recorded as the first draw of each tile through a depth-disabled pipeline
(`build_bg_pipeline`) so it paints under the object without touching the depth buffer the object needs.

The tiles also match the viewport's color response. The offscreen target is a non-sRGB `UNORM` image,
so — exactly like the scene [tonemap pass](../../screen-space-and-post/tonemap-and-exposure/) — the
display transfer must be applied in the shader, not by the hardware. The preview shaders share that
step: `tonemap_ops.slang` is a resource-free module holding the tonemap operators plus a
`tonemapAndEncode` that folds in the sRGB gamma, imported by both `tonemap.slang` and the thumbnail
shaders so there is one encode. The tiles run it as **Khronos PBR Neutral** — the operator tuned to
keep material color true for asset previews — over a fixed studio key light, so a thumbnail reads like
the same asset in the live [material preview](../../materials-and-pipelines/native-materials/), only
cheaper.

## Across the socket as a PNG

There is no shared GPU context with the webview. The rendered image is read back into a host-visible
staging buffer, encoded to a PNG in memory, and returned as base64 in the `get-thumbnail` result
(`{format, width, height, base64}`, the dimensions truthful to the PNG). The [Assets
panel](../assets-panel-and-thumbnails/) asks for 128px; the View modal re-renders at 512px through
`view-asset`. A mesh renders straight into the requested size, so it needs no downscale before
readback — that step only kicks in for an oversized [texture](../assets-panel-and-thumbnails/) asset.

The render is not synchronous on the request. A cold miss is generated on the engine's thumbnail
[worker thread](../assets-panel-and-thumbnails/) (the loaded mesh is handed back to the main thread's
cache afterward): `get-thumbnail` replies `pending` and the result lands in the persistent disk cache,
so the editor's retry — and every later start — is a plain cache read, never a frame-loop stall.

The webview decodes the base64 to a `Blob`, makes an object URL, and caches it by asset id. The
readback runs once per asset, not once per tile or per frame. That
[blob-URL cache](../assets-panel-and-thumbnails/) is where the size-reuse and de-dup live.

## In the code

| What | File | Symbols |
|---|---|---|
| The render | `engine/crates/rendering/src/thumbnail_render.rs` | `render_mesh_thumbnail` |
| Textured model render | `engine/crates/rendering/src/thumbnail_render.rs` · `engine/crates/assets/src/thumbnail.rs` | `render_model_thumbnail`, the `ThumbnailContent::Model` arm in `generate_thumbnail` |
| Auto-framing | `engine/crates/rendering/src/thumbnail_render.rs` | `mesh_bounds`, `framed_view_proj`, the `(1, 0.7, 1)` eye |
| The minimal pipeline | `engine/crates/rendering/src/thumbnail_render.rs` | `ensure_thumbnail_pipeline`, `render_to_texture` |
| Shared backdrop gradient | `engine/assets/shaders/thumbnail_bg.slang` · `engine/crates/rendering/src/thumbnail_render.rs` | `build_bg_pipeline`, `draw_backdrop`, `THUMBNAIL_BG_CLEAR` |
| Shared tonemap + sRGB encode | `engine/assets/shaders/tonemap_ops.slang` · `preview.slang` · `hdri_ball.slang` · `thumbnail.slang` | `tonemapAndEncode` (PBR Neutral) |
| MSAA + resolve | `engine/crates/rendering/src/thumbnail_render.rs` | `ThumbnailTargets::sample_count`, the resolve attachment |
| Readback → base64 PNG (engine) | `engine/crates/control/src/commands_asset.rs` | `get-thumbnail`, `view-asset` |
| Decode + blob-URL cache (client) | `editor/src/state/store.ts` | `getThumbnailUrl`, `base64ToBlob`, `thumbnailCache` |

## Related

- [Assets panel & thumbnails](../assets-panel-and-thumbnails/) — where the preview is shown + cached
- [Asset commands](../../tooling-and-control/asset-commands/) — the `get-thumbnail`/`view-asset` readback
- [Mesh and GPU upload](../../geometry-and-assets/) — the `GpuMesh` bounds the framing reads
