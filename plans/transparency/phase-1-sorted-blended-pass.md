# Phase 1 — Sorted back-to-front translucent pass

**Status:** COMPLETED

**As built.** `alpha_clip: bool` was replaced end-to-end by `saffron_core::BlendMode { Opaque,
Masked, Blend }` (a `Copy` enum with `as_wire`/`from_wire` over `opaque`/`masked`/`translucent`),
threaded through the scene components, `.smat`/component serde, protocol schema + component TS, and
material resolution. `gpu_types::Material` gained `blend`, `PsoKey` gained a `blend` axis, and
`build_mesh_pipeline_with_module` branches to a straight-alpha `SRC_ALPHA`/`ONE_MINUS_SRC_ALPHA`
"over" attachment with `depth_write_enable(false)` on the blend permutation. `SceneDrawList` gained
`transparent_batches`, recorded by `renderer.rs` in a new `scene-translucent` scope after
`scene-submissions`, reading the shared depth.

**Routing is per-submesh, not per-item** (the load-bearing correction). Blend mode belongs to a
material *slot*, and a glTF model like `AlphaBlendModeTest` imports as one shared `MaterialSet` whose
slots span all three modes, with each mesh picking a slot by `material_slot`. A per-item PSO taken
from slot 0 drew every panel opaque; masked survived only because it is a shader feature bit, not a
PSO. So `DrawBatch` gained a `submeshes: Vec<u32>` subset, and `submit_draw_list` resolves the PSO
*per submesh* from its own `BlendMode`, grouping a mesh's submeshes into batches by PSO: opaque +
(at 1×) masked collapse to one opaque batch, masked-under-MSAA to the A2C batch, and translucent to
the blend batch routed to `transparent_batches` with its depth key. All groups share one
submesh-major instance block, so the split adds no instance data. A blend-carrying bucket never
merges (each keeps its own depth key). The `set-material` control command gained a validated `blend`
field. Verified validation-clean on the GPU against the real `AlphaBlendModeTest` +
`CompareAlphaCoverage` models (their translucent submeshes route to the sorted blend batch), by the
`submit_draw_list_splits_submeshes_by_blend_mode` unit test, and via `tests/e2e/alpha_blend.test.ts`.

Part of `plans/transparency/`. This is the core, self-contained phase: it makes glTF `BLEND` actually
blend. At the end of it, `AlphaBlendModeTest`'s Blend panel and `CompareAlphaCoverage`'s `fur_blend`
patch render translucently instead of opaque. Coverage-AA for masked foliage is Phase 2 and does not
block this.

## Goal

A `translucent` material is drawn in a dedicated **translucent** graphics pass that runs after the
forward `"scene"` pass, with a **blend-enabled, depth-test-on / depth-write-off** PSO, its draws
**sorted back-to-front** by view-space depth. Opaque + masked geometry stays on the existing opaque
path (depth-prepass + `"scene"`), and translucent geometry is excluded from the depth pre-pass.

## NO-LEGACY checklist for this phase

- The masked `alpha_clip: bool` is **gone** tree-wide, replaced by `BlendMode { Opaque, Masked, Blend }`.
  Every reader (`render_material.rs`, `draw_list.rs`, `instancing.rs`, the graph-lowered path, tests) is
  migrated in the same change — no bool kept "for the mask path".
- The blend-mode test in `render_material.rs` is rewritten to assert the enum mapping
  (`opaque→Opaque`, `masked→Masked`, `translucent→Blend`), not `("translucent", false)`.
- No second "transparent material" struct or duplicate PSO-request function: the translucent pass reuses
  `SubmeshMaterial` / `DrawBatch` and one `request_mesh_pipeline` extended with a blend axis.

## 1 — `BlendMode` enum replaces the masked bool

**File `engine/crates/rendering/src/draw_list.rs`.**

Introduce the raster blend mode and replace `SubmeshMaterial::alpha_clip`:

```rust
/// How a submesh resolves its alpha (the glTF alpha mode), on the raster side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Alpha ignored; fully opaque. Depth-prepass + opaque scene pass.
    #[default]
    Opaque,
    /// Alpha-tested cutout: discard below `alpha_cutoff`. Opaque pass; Phase 2 adds coverage-AA.
    Masked,
    /// Alpha-blended translucency: sorted, blended, no depth write. The translucent pass.
    Blend,
}
```

- Replace the `alpha_clip: bool` field with `blend_mode: BlendMode`; keep `alpha_cutoff: f32`.
- Update `SubmeshMaterial::defaults()` → `blend_mode: BlendMode::Opaque`.

**File `engine/crates/assets/src/render_material.rs`.** In `build_submesh_material` (and the
graph-lowered path), replace the `alpha_clip = material.blend == "masked"` derivation with:

```rust
blend_mode: match material.blend.as_str() {
    "masked" => BlendMode::Masked,
    "translucent" => BlendMode::Blend,
    _ => BlendMode::Opaque,
},
```

Rewrite the module test accordingly. (The `.smat` `blend` string stays the authoring source of truth;
this is only the raster-side lowering.)

## 2 — Blend axis on the PSO key + a blended mesh PSO

**File `engine/crates/rendering/src/pipelines.rs`.**

- Add a `blend: bool` (translucent) discriminant to `PsoKey` (alongside `unlit`/`skinned`/`wireframe`/
  `sample_count`). It selects a distinct cached pipeline; masked is *not* a separate PSO here — masking
  is a runtime discard on the opaque PSO (Phase 2 adds the coverage variant).
- In `build_mesh_pipeline_with_module`, branch the color-blend + depth state on the key's blend flag:
  - **Opaque (today):** `blend_enable(false)`, `depth_write_enable(true)`.
  - **Blend (new):** attachment = the existing `alpha_blend_attachment()` (straight-alpha over:
    `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA`, alpha `ONE` / `ONE_MINUS_SRC_ALPHA`), `depth_test_enable(true)`,
    **`depth_write_enable(false)`**, `depth_compare_op(LESS_OR_EQUAL)`. Keep the dynamic cull mode so
    two-sided translucents (both test models are `doubleSided`) cull `NONE`.
- Extend `request_mesh_pipeline` to take the blend flag (or read it from the item — see §3) and thread
  it into the key. Blending in the HDR `R16G16B16A16_SFLOAT` linear scene color is correct (pre-tonemap,
  linear `over`); no format change.

The übershader shader is unchanged: `fragmentMain` already returns `float4(color, surf.opacity)`, which
is exactly the straight-alpha source the attachment expects.

> Decision to record when building: straight vs premultiplied alpha. `alpha_blend_attachment()` is
> straight-alpha (`SRC_ALPHA`,`ONE_MINUS_SRC_ALPHA`) and the shader emits straight color+opacity, so they
> match as-is. If Phase 2's coverage path or emissive-heavy translucents want premultiplied later, switch
> the source factor to `ONE` and premultiply in-shader — one coherent choice, not both.

## 3 — Split the draw list: opaque vs translucent

**File `engine/crates/rendering/src/instancing.rs`.**

- Add `camera_pos: Vec3` (world eye) to `DrawListInputs` for the depth sort; the caller derives it from
  the inverse view (see §5). `view_proj` alone is insufficient for a stable back-to-front key.
- In `submit_draw_list`, partition work by `blend_mode`. Opaque + masked submeshes bucket exactly as
  today (by `(pipeline, mesh)`, instanced, first-seen order). Translucent submeshes go to a **separate**
  list on `SceneDrawList` (add `pub transparent_batches: Vec<DrawBatch>`), built with the blend PSO
  (`request_mesh_pipeline(..., blend = true)`).
- **Sorting granularity.** Per-object back-to-front is the baseline: sort translucent draws by
  view-space depth of the instance/submesh center (`dot(center - camera_pos, forward)`, or transform the
  center by the view matrix and sort on `z`), descending. Because sorting fights instanced merging,
  translucent draws are emitted **one instance per batch** (no cross-depth instancing) — foliage is not
  the translucent case that needs instance throughput, and correctness wins here. Keep masked/opaque
  instancing untouched.
- **Per-submesh routing.** A mesh may mix opaque and translucent submeshes (glTF materials are
  per-primitive). Route per submesh: an opaque submesh of an otherwise-translucent mesh stays in the
  opaque buckets, and vice-versa. The test models are single-material meshes, but the design handles the
  general case — do not assume whole-mesh blend mode.
- `resolve_material` keeps setting `FEATURE_ALPHACLIP` for `BlendMode::Masked` (rename the local from
  `alpha_clip`), and packs `alpha_cutoff` into `pbr.w` as today. Translucent needs no feature bit.

**File `engine/crates/rendering/src/draw_list.rs`.** Add `transparent_batches` to `SceneDrawList` and
`shallow_clone`; the sort key can be computed at build time (store nothing extra on `DrawBatch`, or add a
`sort_depth: f32` if the record pass re-sorts).

## 4 — Record the translucent draws

**File `engine/crates/rendering/src/scene_pass.rs`.** Add `record_transparent_draw_list`, a sibling of
`record_scene_draw_list`: bind the same set 0/1/2/3/(4/5) descriptor sets, then replay
`transparent_batches` in sorted order via `record_batch_submeshes` (reuse the per-submesh dynamic cull).
No depth-prepass sibling — translucents never write depth.

## 5 — Add the translucent pass to the graph

**File `engine/crates/rendering/src/renderer.rs`.**

- Where `DrawListInputs` is built for `submit_draw_list`, also pass `camera_pos` (from the camera / the
  inverse view matrix already available to the pass).
- Add a `RgPass::graphics("translucent", extent)` **after** the `"scene"` pass, before the post/tonemap
  chain reads scene color:
  - `.color(color_att)` on the **same** HDR scene-color target with **load** (not clear) — it composites
    over the lit opaque result.
  - `.depth_attachment(depth_att)` declared **read-only** (depth-test, no write): reuse the resolved
    scene depth so translucents occlude correctly against opaque geometry. Confirm the render graph can
    declare a depth attachment as read-only (test enabled, write disabled) — if `depth_attachment` only
    models read-write, add a read-only depth declaration (`RgUsage::DepthRead`) so the graph derives the
    right `DEPTH_STENCIL_READ_ONLY_OPTIMAL` layout + barrier. This is the one render-graph capability
    this phase may need to add.
  - Body: one scope `"translucent"` calling `record_transparent_draw_list`.
- **Depth pre-pass exclusion.** The depth-prepass (`RgPass::graphics("depth-prepass")`) and the
  `scene-opaque` record already draw only the opaque/masked buckets once §3 routes translucents out of
  `batches`, so no explicit change beyond the split — verify translucents are absent from `batches`.
- **Ordering vs post.** The translucent pass must run after `"scene"` and after motion vectors, and
  before tonemap. Place it in the graph build accordingly; sky (`"sky"`) already draws before `"scene"`
  so the background is present for blending.

## Known interactions (note, don't over-engineer)

- **TAA ghosting.** Translucents write neither depth nor motion vectors, so TAA can smear them under
  motion. Acceptable for Phase 1 (this is the standard first-cut behaviour); the modern fix (responsive
  AA / a translucency velocity contribution) is out of scope here — note it in the docs page.
- **Contact shadows / SSAO / SSR** sample the opaque depth/G-buffer; translucents correctly do not
  contribute to them (they run before the translucent pass). No action.
- **Thumbnails / asset preview** (`thumbnail_render.rs`) reuse the mesh path; a translucent thumbnail
  will now blend — confirm the preview view builds a `translucent` pass too, or accept opaque previews
  and note it. Prefer wiring it through the same helper so there is one path.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning
this change raises. Boot both models headless (`just run-engine-headless`) with a validation-clean log,
and add/extend a `tests/e2e` case asserting a `translucent`-material entity renders (and that the draw
list reports a non-empty transparent batch list via `render-stats` if exposed). Update the
`docs/content/` rendering explanation page (alpha modes / transparency) and its hub `_index.md` row in
the same change.
