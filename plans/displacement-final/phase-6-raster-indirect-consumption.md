# Phase 6 — Raster consumption across all seven passes + height-mode routing default + delete the preview-sphere crutch

**Status:** NOT STARTED

## Goal

Make every rasterized geometry pass draw the Phase-4 tessellated buffer through an **indirect
draw** that reads the Phase-3 generated index stream and args/count buffers, so the surface the
rasterizer shows is byte-for-byte the surface the Phase-7 BLAS traces. Re-pin the import routing so
an ordinary height map defaults to **Parallax** — Displacement is a deliberate authored choice that
carries a factor budget — and delete the preview-sphere crutch so **preview == scene** on the
raster/baked path: an ordinary low-poly sphere carrying a Displacement material shows a true bulged
silhouette through the real tessellating path, with no bespoke dense stand-in.

## Build plan

### 1. One shared draw pair backs all seven passes — change it once

Every geometry pass records its per-batch draws through exactly two functions in
`crates/rendering/src/scene_pass.rs`:

- `bind_batch_vertices` (`scene_pass.rs:88`) — binds binding-0 vertices and the index buffer.
- `record_batch_submeshes` (`scene_pass.rs:29`) — issues one `cmd_draw_indexed` per submesh
  (`scene_pass.rs:72`), or a single whole-mesh `cmd_draw_indexed` for a submesh-less mesh
  (`scene_pass.rs:43`).

The seven consumers all call this pair:

| Pass | Function | Call site |
|---|---|---|
| Scene (opaque) | `record_scene_draw_list` → `record_batches` (`scene_pass.rs:333`) | `scene_pass.rs:348-349` |
| Directional/spot shadow | `record_shadow_depth` (`scene_pass.rs:358`) | `scene_pass.rs:393-394` |
| Depth prepass | `record_depth_prepass` (`scene_pass.rs:411`) | `scene_pass.rs:444-445` |
| Reactive coverage | `record_reactive_coverage` (`scene_pass.rs:454`) | `scene_pass.rs:487-488` |
| GBuffer | `record_gbuffer` (`scene_pass.rs:496`) | `scene_pass.rs:531-532` |
| Point-shadow cube | `record_point_shadow` (`scene_pass.rs:567`) | `scene_pass.rs:710-711` |
| Motion vectors | `render_motion` in `aa.rs` | `aa.rs:373` (binds its own streams first) |

Because the Phase-4 tessellated VB/IB is written **once** in the deform scope and read by all of
them, changing this shared pair reaches every pass consistently — a per-pass re-dice can never
desync depth vs shade or re-crack shadows. This is the whole reason the tessellation is a deform-scope
prepass into a shared buffer rather than a per-pass amplifier.

### 2. `DrawBatch` — carry the generated index + args/count handles

`DrawBatch` (`crates/rendering/src/draw_list.rs:180`) today carries `deformed: bool`
(`draw_list.rs:192`) and `deformed_vertex_offset: u32` (`draw_list.rs:195`) for the fixed-topology
skin/morph/displace path. After Phase 4, `HeightMode::Displacement` batches are **tessellated**
(variable topology), while skinned/morph batches stay fixed-topology. These are distinct draw shapes,
so distinguish them explicitly rather than overloading `deformed`:

- Add `tessellated: Option<TessDraw>` to `DrawBatch`. `TessDraw` carries the per-frame handles the
  indirect draw needs: the transient **generated index buffer** (`vk::Buffer`), the transient
  **vertex buffer** (`vk::Buffer` — the tessellated slice, distinct from the `Skinning` deformed
  ring), the **indirect-args buffer** + base offset (one `VkDrawIndexedIndirectCommand` per submesh),
  the **count buffer** + offset, and the CPU-side `max_draw_count` (the submesh count, the upper
  bound the `_count` draw clamps to). `deformed_vertex_offset` is repurposed as the tessellated-slice
  base for this batch when `tessellated` is set.
- When `tessellated` is `None`, the batch keeps the exact current fixed-count path (skinned/morph
  fixed-topology deform, or a static mesh). When `Some`, the batch draws indirectly (§3, §4).

`Instancing::submit_draw_list` (`crates/rendering/src/instancing.rs`) populates `tessellated` for a
Displacement bucket from the Phase-3/Phase-4 allocation (the transient VB/IB handles and the
args/count slice for that instance). Tessellated batches inherit the never-merge rule (a displace
bucket never merges, `instancing.rs`), so — like every deforming batch (`draw_list.rs:191-192`) — a
tessellated batch is **single-instance**. That collapses the submesh-major instance addressing to
`firstInstance = base_instance + s` per submesh `s`, which Phase 3/4 must seed into each
`VkDrawIndexedIndirectCommand` (see §5).

### 3. `bind_batch_vertices` — bind the generated index buffer

`bind_batch_vertices` (`scene_pass.rs:88`) hardcodes `batch.mesh.index_buffer()` at
`scene_pass.rs:102`. Branch on `batch.tessellated`:

- `Some(t)` → bind `t.vertex_buffer` at binding 0 and `t.index_buffer` as the index stream (the
  Phase-3 generated stream), both at offset 0, `vk::IndexType::UINT32` (the tessellator emits u32
  indices to match `triangle_geometry`'s `UINT32` contract in `rt.rs`, keeping the raster and RT
  index formats identical).
- `None` → the current match at `scene_pass.rs:94-97` (deformed ring or static stream) + the base
  `batch.mesh.index_buffer()`.

The vertex format is the shared 48-byte `Vertex`; the tessellated slice carries the same layout, so
the binding-0 attribute state is unchanged.

### 4. `record_batch_submeshes` — indirect draw + feature gating

Give `record_batch_submeshes` (`scene_pass.rs:29`) a tessellated branch that replaces the CPU-count
`cmd_draw_indexed` (`scene_pass.rs:43`, `:72`) with an indirect draw sourced from `TessDraw`. The
counts, `firstIndex`, `vertexOffset`, and `firstInstance` all come from the GPU-written
`VkDrawIndexedIndirectCommand`, so the CPU no longer computes them (the `deformed_vertex_offset +
submesh.vertex_offset` math at `scene_pass.rs:68` and the `firstInstance` math at `scene_pass.rs:69`
move into the Phase-3/4 seed).

**Feature-gated selection** — the three probes land in a `Capabilities` struct in
`crates/rendering/src/device.rs` in Phase 1 (mirroring the existing `mesh_shader_supported` probe).
Document them here because Phase 6 is their first load-bearing consumer:

- **`drawIndirectCount`** (`VkPhysicalDeviceVulkan12Features::drawIndirectCount`, core in Vulkan 1.2 /
  `VK_KHR_draw_indirect_count`) — enables `cmd_draw_indexed_indirect_count`, where the **GPU-written
  count buffer** decides how many of the seeded draws actually run, so an instance that tessellated to
  zero micro-tris in a submesh contributes no draw.
- **`multiDrawIndirect`** (`VkPhysicalDeviceFeatures::multiDrawIndirect`) — permits `drawCount > 1` in
  a single `cmd_draw_indexed_indirect` / `_count`, batching a whole batch's submeshes in one call.

The ladder, best to floor:

1. **`_count` + multi-draw** (both probes set): one
   `cmd_draw_indexed_indirect_count(cmd, args, args_offset, count_buf, count_offset, max_draw_count,
   stride)` per batch. GPU-decided draw count, one call per batch.
2. **multi-draw, no count** (`multiDrawIndirect` only): one
   `cmd_draw_indexed_indirect(cmd, args, args_offset, max_draw_count, stride)` per batch — the CPU
   supplies the submesh count as `drawCount`; per-draw index counts still come from the GPU. Empty
   submeshes emit a zero-`indexCount` command (a no-op draw) rather than being skipped.
3. **CPU loop** (neither probe): loop the submeshes, one `cmd_draw_indexed_indirect(cmd, args,
   args_offset + s*stride, 1, stride)` per submesh (`drawCount == 1` requires no feature, so this is
   valid on **every** implementation including llvmpipe). Counts are still GPU-sourced; only the draw
   dispatch is CPU-iterated.

`stride = size_of::<VkDrawIndexedIndirectCommand>()` (20 bytes). The args/count buffers were allocated
with the `INDIRECT_BUFFER` usage bit added to `Buffer::new` in Phase 1 and acquired from
`TransientResources` in Phase 3.

### 5. Interface contract on Phase 3/4 — seed the indirect commands correctly

Phase 6's correctness rests on the seeded `VkDrawIndexedIndirectCommand` reproducing what the current
CPU path computes, so state it as a hard requirement on the Phase-3 predict→args pass and the Phase-4
emit:

- **`firstInstance`** must equal `base_instance + s` for submesh `s` (single-instance tessellated
  batch), so the vertex shader's `instances[SV_VulkanInstanceID]` fetch keeps addressing the correct
  submesh-major instance row (the invariant documented at `scene_pass.rs:9-14`). This must hold in the
  depth/shadow/GBuffer/motion passes too, which read the same instance SSBO.
- **`vertexOffset`** must equal the tessellated-slice base so the generated indices (local to the
  emitted vertex block) resolve into the transient VB. Phase 4 emits indices relative to the slice
  base and seeds `vertexOffset` accordingly.
- **`indexCount` / `firstIndex`** come from the GPU-exact packing (Phase 3 prefix-sum offsets).

### 6. Graph barriers — declare `IndexInputRead` + `IndirectCommandRead` on the consumers

The Phase-4 tessellate pass declares `StorageWriteCompute` on the transient VB, IB, and args/count
buffers (in the deform scope in `renderer.rs::record_scene_graph`, the same scope that hosts
`RgPass::compute("displace")` today). Every consuming geometry pass already declares `VertexInputRead`
on the deformed/vertex resource; for the tessellated stream each also declares, on the transient IB
and args/count resources:

- **`RgUsage::IndexInputRead`** (stage `INDEX_INPUT`, access `INDEX_READ`) on the generated index
  buffer, and
- **`RgUsage::IndirectCommandRead`** (stage `DRAW_INDIRECT`, access `INDIRECT_COMMAND_READ`) on the
  args + count buffers.

Both variants are added to the `RgUsage` enum + golden `usage_info` table in Phase 1. With them
declared, `apply_access` / `derive_pass_barriers` derive the compute-write → index-input and
compute-write → indirect-command barriers automatically — no pass writes a barrier by hand, exactly as
the existing compute-write → `VertexInputRead` barrier for the deformed buffer is derived today. This
is purely additive at each pass's `.access(...)` chain in `renderer.rs`; the barrier core is unchanged.

### 7. The motion pass binds its own index stream (`aa.rs`)

The motion pass does **not** go through `bind_batch_vertices`. `render_motion` binds its own
`(cur, prev)` position streams and `batch.mesh.index_buffer()` directly (`aa.rs:369-371`) before
calling `record_batch_submeshes` (`aa.rs:373`). It must gain the same tessellated branch: for a
tessellated batch bind `t.vertex_buffer` as both current and previous position streams (Phase 5 emits
current + previous micro-vertex positions into the transient slices it double-buffers), and bind
`t.index_buffer` instead of `batch.mesh.index_buffer()` at `aa.rs:371`. `record_batch_submeshes` then
takes the shared indirect branch (§4). Because Phase 5 owns the previous-position production, this is
where its transient handles surface into the motion draw; the two land together.

### 8. Dynamic cull mode vs batched multi-draw (scene pass only)

Only `record_scene_draw_list` passes `Some(&batch.submesh_cull)` and issues `cmd_set_cull_mode` per
submesh via dynamic state (`scene_pass.rs:63-67`); the other six pass `None` and keep a baked cull
mode. A batched `_count` / multi-draw draws all of a batch's submeshes in one call, so it cannot
interleave a per-submesh `cmd_set_cull_mode`. Resolve it per pass:

- **Scene pass with per-submesh cull** (`cull_modes: Some`): loop the submeshes, `cmd_set_cull_mode`
  the submesh's mode, then a single `cmd_draw_indexed_indirect(cmd, args, args_offset + s*stride, 1,
  stride)` for that submesh (the floor form, drawCount 1). This preserves two-sided-material cull for
  displaced surfaces at one indirect call per submesh — acceptable, since the scene pass already loops
  per submesh today.
- **The other six passes** (`cull_modes: None`, baked cull): use the batched `_count` / multi-draw
  form (§4) — no per-submesh state to set.

This keeps the dynamic-cull correctness the scene pass depends on without forcing every pass into the
slower per-submesh loop.

### 9. Height-mode routing default — re-pin `detect_height_mode` to Parallax

`detect_height_mode` (`crates/assets/src/scan.rs`) currently routes a `displace`/`_disp` filename to
`HeightMode::Displacement`. Re-pin it so an imported height map **never auto-routes to Displacement**:

```
if has("bump") { HeightMode::Bump } else { HeightMode::Parallax }
```

Update the doc comment above it accordingly (it currently documents the `*_Displacement` →
Displacement route as the library authoring intent). Displacement now costs tessellation + a per-frame
BLAS, so it is a deliberate authored choice through the D1 Material-editor `heightMode` dropdown, not
an import inference — the flat / low-poly common case never silently pays for it. `bake_material_container`
(`crates/assets/src/import.rs`) already assigns `material.height_mode` from the height map's
`MaterialMap.height_mode` (the value `detect_height_mode` produced), so an ambientCG `*_Displacement`
map now imports in **Parallax** mode automatically; no other import-side change is required. The
`.smat` `heightMode` vocabulary and wire are untouched (this is a default-selection change, not a
format change).

### 10. Delete the preview-sphere crutch (preview == scene)

With real tessellation on arbitrary meshes, the dense stand-in and the forced-Displacement preview are
dead. Delete them in this change (NO-LEGACY — the replacement path lands here):

- **`preview_displacement_sphere`** (`crates/geometry/src/primitives.rs:132`, the 192×288 ~56k-vertex
  sphere) — delete the function.
- **`PREVIEW_DISPLACE_SPHERE_MESH_ID = Uuid(7)`** (`crates/assets/src/lib.rs:144`) and its references
  in the reserved-id tests (`lib.rs:384`, `:388`, `:395`, `:400`) — delete the constant and its test
  rows.
- **`load.rs:255-258`** — the seeding that builds `preview_displacement_sphere()` for id 7 — delete
  the branch.
- **`attach_preview_sphere`** (`crates/control/src/commands_asset.rs:3226`) — repoint its `Mesh`
  component from `PREVIEW_DISPLACE_SPHERE_MESH_ID` (`commands_asset.rs:3230`) to the ordinary
  `BUILTIN_SPHERE_MESH_ID` (`Uuid(5)`, `lib.rs:127`). The two call sites in `build_preview_scene`
  (`commands_asset.rs:3144`, `:3159`) are unchanged; the preview subject now rides a normal low-poly
  sphere and gets its silhouette from the real tessellating path — the same path a scene mesh uses.
- **`preview_material_for_texture`** (`commands_asset.rs:3046-3053`) — the `TextureRole::Height` branch
  forces `height_mode = HeightMode::Displacement` and a magic `height_scale = 0.08`. Replace with the
  map's real (re-pinned) mode: route through `detect_height_mode` on the texture's catalog name (the
  callers already resolve that name), which yields **Parallax** for an ordinary height map, and set
  `height_scale` from the one world-space amplitude convention pinned in Phase 4 (not the magic 0.08).
  A Displacement-authored material (`PreviewSubject::Material`) already carries `heightMode =
  Displacement` and now bulges on the ordinary sphere through tessellation; a bare Parallax height
  texture previews as parallax on the same sphere — in both cases the preview matches what the scene
  renders.

Import `BUILTIN_SPHERE_MESH_ID` where `PREVIEW_DISPLACE_SPHERE_MESH_ID` was imported
(`commands_asset.rs:19`).

### 11. Optional — engage the C2 mesh-shader front end as a raster consumer

`MeshletRaster` (`crates/rendering/src/meshlet_raster.rs`) already reads the shared deformed buffer as
its `vertexStream` via `MeshletPush.vertex_base`. As an **optional, non-load-bearing** extension it
could read the transient tessellated VB the same way for the opaque scene pass, so mesh-shader
hardware rasterizes the identical stream the index path draws. It stays raster-only (it can never be
the RT source — mesh output never enters a BLAS, the exact divergence this planset eliminates), stays
behind `mesh_shader_supported()` + the `SAFFRON_MESH_SHADER` opt-in, and consuming a variable index
count via `cmd_draw_mesh_tasks` needs task-shader amplification that is out of scope here. Leave it a
follow-up; the indirect index-draw path (§1-§8) is the authoritative raster consumer.

## Scope

`saffron-rendering` (`scene_pass.rs`, `aa.rs`, `draw_list.rs`, `instancing.rs`, and the per-pass
`.access` declarations in `renderer.rs`); `saffron-assets` (`scan.rs::detect_height_mode`,
`primitives.rs`, `lib.rs`, `load.rs`); `saffron-control` (`commands_asset.rs` preview builders). No
`.smat` / wire / protocol change (the mode vocabulary is unchanged).

## Depends on

- **[`phase-4-dice-displace-weld-emit.md`](phase-4-dice-displace-weld-emit.md)** — the tessellated
  VB/IB and the generated index stream this phase draws; the amplifying kernel that retires the 1:1
  displace path and the `deformed_cursor += vertex_count` reservation.
- **[`phase-5-temporal-motion-vectors.md`](phase-5-temporal-motion-vectors.md)** — the previous-frame
  transient position slices the motion pass (§7) binds.
- Phase 1 (the `IndexInputRead` / `IndirectCommandRead` `RgUsage` variants, the `INDIRECT_BUFFER`
  buffer-usage bit, and the `drawIndirectCount` / `multiDrawIndirect` probes) and Phase 3 (the
  args/count buffers and the seeded `VkDrawIndexedIndirectCommand`s) are transitive dependencies
  through Phase 4.

## Verification

- **Build/clippy:** `just engine` then `just prepare-for-commit` clean on the touched crates; the
  reserved-id tests in `lib.rs` pass after deleting `PREVIEW_DISPLACE_SPHERE_MESH_ID` (no dangling
  reference), and every asset re-seeds without id 7.
- **CPU/logic:** a unit test on the feature-gate ladder (given each probe combination, the selected
  draw form is the expected one), and a test that `detect_height_mode` returns `Parallax` for a
  `*_Displacement` / `*_disp` filename and `Bump` for a `*_bump` filename (Displacement is never
  auto-routed).
- **Needs a GPU with eyes** (the crack-testing this engine needs):
  - A **low-poly authored-Displacement scene mesh** shows a true bulged silhouette in the main
    viewport and in every shadow map (directional, spot, and the point-shadow cube faces), not a flat
    bump — confirming all seven passes consume the tessellated buffer.
  - **Preview == scene** for the raster/baked path: the same Displacement material on the ordinary
    builtin sphere in the previewer matches the same material in a scene (the dense-sphere +
    forced-`0.08` crutches are gone).
  - An **ordinary imported height map stays Parallax** — no tessellation dispatch, no BLAS build for
    it (confirm via `render-stats` / the tessellation-budget counters and a validation-clean log).
  - No cracks along shared edges or UV seams under camera motion (the watertightness Phases 2-4
    guarantee, observed here through the indirect-drawn silhouette in the shadow maps).
  - Run headless with `SAFFRON_EXIT_AFTER_FRAMES` on the NVIDIA card
    (`just run-engine-headless`) for a validation-clean log across the indirect draws in all seven
    passes.

## Risks

- **Dynamic cull vs batched multi-draw** (§8): batching a whole batch's submeshes into one `_count`
  draw is incompatible with the scene pass's per-submesh `cmd_set_cull_mode`. The mitigation (scene
  pass loops per submesh with single indirect draws; the other six batch) keeps correctness but leaves
  the scene pass at one indirect call per submesh — acceptable, and it already loops today.
- **Indirect-command seeding is load-bearing** (§5): if Phase 3/4 seed `firstInstance` or
  `vertexOffset` wrong, the vertex shader reads the wrong instance row or the indices miss the slice —
  a silent mis-draw, not a crash. The single-instance invariant of tessellated batches keeps the
  `firstInstance = base_instance + s` math simple, but it must be reproduced identically in all seven
  passes since they share the instance SSBO.
- **Feature-probe portability**: the floor form (single `cmd_draw_indexed_indirect`, `drawCount == 1`)
  is the only universally-guaranteed path; the `_count` and multi-draw fast forms depend on probes that
  are absent on some tiers. The ladder must never assume a probe — an unprobed device silently
  dropping to the floor is correct, just chattier. Real-GPU cross-vendor confirmation is deferred (no
  hardware GPU in the toolbox); the floor is architected portable but is not claimed proven on AMD/Intel.
- **Height-mode default regression**: re-pinning `detect_height_mode` before Phase 4 pins the shared
  world-space amplitude would regress preview fidelity (the forced-`0.08` crutch is deleted here). This
  phase depends on Phase 4, so the amplitude convention exists before the crutch is removed — do not
  land §9-§10 ahead of Phase 4.
- **Preview-surface coverage**: the previewer, the async thumbnail tiles, and the `preview-render` pane
  all route through `build_preview_scene` / `attach_preview_sphere`, so repointing the mesh in one
  place covers them all — but verify the thumbnail path (which renders through the same forward+ graph)
  also shows the tessellated silhouette, so a tile matches the interactive preview.
