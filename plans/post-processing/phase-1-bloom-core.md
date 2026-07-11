# Phase 1 — Bloom core: energy-conserving mip-pyramid

**Status:** COMPLETED

Part of `plans/post-processing/` (bloom + color grading as one post-processing subsystem). This is the first, self-contained phase — it stands entirely alone (no dependency on the grading phases) and ships a full vertical slice: a thresholdless 13-tap Karis-averaged downsample + 9-tap tent upsample compute pyramid composited by an energy-conserving `lerp` into the scene-linear `color` target **before** the tonemap pass, plus its PSO, per-view descriptor sets, transient mip chain, render-graph passes, protocol DTO, control command, persistence, `sa` reachability, `RenderPanel` rows, and docs page. It does not touch the tonemap shader, does not add grading, and does not block any later phase.

## Goal

Emissive/HDR-bright pixels glow automatically in luminance proportion, with the glow rolled off gracefully by the existing view transform because bloom runs in unbounded scene-linear radiance ahead of it. Concretely:

- A new `bloom.slang` compute shader with one `computeMain` that branches on a push `pass` field: `0` = 13-tap Karis-averaged downsample, `1` = 9-tap tent upsample-and-add, `2` = energy-conserving composite into `color`. Auto-compiles to `bloom.spv` (no `xtask` edit).
- A `bloom` PSO in the cache, a per-view descriptor-set array, and a transient half-res mip chain sized off `published_extent()`.
- `Renderer::add_bloom_pass` inserted in `Renderer::render` strictly **between** the resolve / `ssgi-history` block (`renderer.rs` ends ~l.5777) and `self.add_tonemap_pass` (~l.5784) — still linear HDR, in-place on `color`.
- `set-bloom` as the one control command for bloom state (enabled / intensity / scatter / tint / threshold), reachable from `sa` with no per-command CLI code, persisted in the project `renderSettings` block, surfaced back through `RenderStatsDto`, and wired into `RenderPanel.tsx`.

## Design stance (grounded in current engine practice)

The engine already runs the mandatory tonemap as an in-place compute pass on the display-extent RGBA16F `color` storage image in `GENERAL` layout (`tonemap.slang` `computeMain`, declared `(color, RgUsage::StorageImageRwCompute)` at `renderer.rs` ~l.5784). Bloom is the same shape one step earlier: compute passes over the same `color` target, every barrier derived by the render graph from declared usage (`render_graph.rs` `usage_info` golden table l.269). Nothing about bloom is novel plumbing — the PSO cache, transient pool, per-view descriptor scaffolding, and compute-pass helper are all in place and are the exact attach points.

The technique is the Jimenez / Call-of-Duty:AW energy-conserving pyramid, not a bright-pass + Gaussian:

> **13-tap bilinear downsample (36 effective taps) → Karis luma average on the first downsample only → progressive 9-tap tent upsample scaled by one `filterRadius` UV → `lerp(hdr, bloom, intensity)` composite in scene-linear.** Threshold is OFF by default; the Karis average on the `color → mip0` step is the firefly / TAA-stability mechanism, and the energy-conserving relative-fraction composite (not additive) prevents brightness pile-up.

Bloom composites into `color` **before** the tonemap pass so it operates on unbounded scene radiance (emissives, speculars, sun) exactly as physically-plausible glow requires, and the chosen view transform (AgX's highlight desaturation especially) rolls off the bloomed highlights for free — the ordering itself is the quality feature.

## NO-LEGACY checklist for this phase

- `set-bloom` is **the** way to drive bloom — there is no second enable/intensity command and no bloom knob hidden on `set-tonemap` or `set-environment`. One `SetBloomParams` DTO, one handler, one `ControlRenderer` setter surface.
- Bloom state lives in exactly one place per layer: `Renderer` fields (`renderer.rs`), the `ControlRenderer` trait (`registry.rs`) implemented in the host + the test stub + the `commands_asset.rs` `ProjectHost` helper (all three impls gain the methods in the same change — no stub left un-migrated), and the persisted `RenderSettings` `Option<T>` block. No parallel copy on `EnvironmentDto`.
- The bloom pass name is added to the `final_post_pass_names` arming test in `overlay.rs` (l.285) in the same change so the ordered post-chain gate stays exact, not left stale.
- Every new DTO is registered in *all* tripwire lists (`codegen.rs` `ts_decls`/`fragment_decls`, `tests/inventory.rs`, `tests/schema_fragments.rs`) and the four `command.rs` sibling arrays in one change — the build refuses to compile otherwise, by design.

## 1 — The bloom compute shader

**File `engine/assets/shaders/bloom.slang` (new).**

One entry point `computeMain`, one push, three passes selected by `push.pass`. The downsample uses the exact 13-tap weight kernel (center quad `0.125`, inner ring `0.0625`, outer corners `0.03125`, summing to `1.0`) exploiting hardware bilinear via half-texel offsets; the Karis partial average (weight `1/(1+luma)` over the five overlapping 2×2 boxes, luma in an sRGB-ish perceptual space) is applied only when `push.karis != 0`, i.e. on the `color → mip0` step. The upsample is the progressive 9-tap 3×3 tent with sample offsets scaled by `push.filterRadius`. The composite reads `color` in place (storage load), reads the accumulated `mip0` bloom (sampled), and writes `lerp(hdr, bloom * push.tint, push.intensity)`.

```hlsl
// set 0: binding 0 = source (SampledTexture2D, linear sampler), binding 1 = target (RWTexture2D<float4>, rgba16f)
struct Push {
    float3 tint;         // bloom colour, composite pass only
    float  filterRadius; // scatter, UV units (~0.005)
    float  intensity;    // energy-conserving lerp weight, composite pass only
    float  threshold;    // soft-knee prefilter; 0.0 = off (default)
    uint   pass;         // 0 = downsample, 1 = upsample-add, 2 = composite
    uint   karis;        // 1 on the first downsample only
};
[vk::push_constant] Push push;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID) { /* branch on push.pass */ }
```

- New `bloom.slang` compiles automatically: the entry-point loop in `engine/xtask/src/shaders.rs` (l.213-257) emits `<stem>.spv` for every non-excluded `*.slang` with a `computeMain`. **No** `xtask` edit — it is a real entry-point shader, not a helper module, so it must *not* go in the exclusion list (l.224-232).
- Input clamped to `min 1e-4`, clamp-to-edge on upsample, border on downsample; format is `R16G16B16A16_SFLOAT` throughout, matching `OFFSCREEN_COLOR_FORMAT` (`pipelines.rs` l.34).

> Decision to record when building: **energy-conserving `lerp(hdr, bloom, intensity)`, never additive.** One coherent composite so `intensity` reads as a mix fraction (default `~0.05`, Jimenez uses `0.04`) that never piles brightness up on already-bright pixels — additive bloom is not offered as an alternate path.

> Decision to record when building: **Karis average on the first downsample only.** It is the firefly / TAA-stability win at the one place single-texel HDR fireflies exist (`color → mip0`); it is *not* applied on deeper downsamples (they are already smooth) and is gated by `push.karis`, not a separate shader.

## 2 — Descriptor layout and pool budget

**File `engine/crates/rendering/src/descriptors.rs`.**

Add `create_bloom_layout` shaped like `create_fxaa_layout` (l.1268): a `COMBINED_IMAGE_SAMPLER` at binding 0 (the linear-sampled source) plus a `STORAGE_IMAGE` at binding 1 (the RW target), both `COMPUTE`-stage via `compute_binding` (l.1324). Store the layout + an accessor on `Descriptors`, and allocate per-view sets with `allocate_set` (l.648). The bloom source sampler is `linear_sampler()` (l.258).

```rust
pub(crate) const MAX_BLOOM_MIPS: usize = 7; // 6 at 1080p, 7 at 1440p+
```

- Bloom needs one set per pyramid pass (each binds a distinct source/target pair): `N` downsamples + `N-1` upsamples + `1` composite = up to `2 * MAX_BLOOM_MIPS` sets per view. Bump the `STORAGE_IMAGE` pool budget term (l.1352-1355, the `29 * views` term) and the `COMBINED_IMAGE_SAMPLER` term by `2 * MAX_BLOOM_MIPS * views` so the pool never undersizes — the same discipline the existing screen-space sets follow.

## 3 — The bloom PSO

**File `engine/crates/rendering/src/pipelines.rs`.**

Mirror `request_tonemap` (l.1010): add a `bloom: Option<Arc<Pipeline>>` cache field and a `request_bloom` that calls `build_compute("shaders/bloom.spv", bloom_set_layout, size_of::<BloomPush>())` (l.2516; entry point `computeMain`, single set, one `COMPUTE` push range). `load_shader_module` (l.2932) resolves `shaders/bloom.spv` from the runtime shader dir — no format or entry wiring beyond the call.

- One PSO covers all three passes; the `pass`/`karis` push fields select behaviour per dispatch, so there is a single pipeline in the cache, not three.

## 4 — Per-view sets and the transient mip chain

**File `engine/crates/rendering/src/view_target.rs`.**

Add a per-view `bloom_sets: Vec<vk::DescriptorSet>` to `ViewTarget` (l.63), allocated alongside the other screen-space sets near `tonemap_set` alloc (l.464) to `2 * MAX_BLOOM_MIPS`. The mip *images* come from the transient pool, not `ViewTarget`, so a view switch never aliases and no bloom image outlives a frame. Write the set image bindings each frame in `write_aa_sets` (l.948) via `Binding::sampled(set, 0, linear_sampler, src_view)` and `Binding::storage(set, 1, dst_view)` (l.1437 / l.1451), after the transient mips are acquired. Size the chain off `published_extent()` (l.1406) — bloom runs on the display-extent `color`, not the `scaled_render_extent()` scene target (l.1417). Sets rewrite on recreate via the `generation` bump (l.239).

**File `engine/crates/rendering/src/transient.rs`.**

Each pyramid level is a distinct keyed image: `acquire_image(frame, key, &ImageDesc)` (l.185) returns one `(vk::Image, vk::ImageView)` per key, grow-only and fence-safe (reset only after the frame fence signals, `begin_frame` l.96). Add a static key array so each level is stable across frames.

```rust
pub(crate) const BLOOM_MIP_KEYS: [&str; MAX_BLOOM_MIPS] =
    ["bloom-mip-0", "bloom-mip-1", "bloom-mip-2", "bloom-mip-3",
     "bloom-mip-4", "bloom-mip-5", "bloom-mip-6"];
```

- Level `0` is `published_extent() / 2`, each subsequent level halves again; the live level count is `floor(log2(min(w, h))) - 3`, clamped to `[1, MAX_BLOOM_MIPS]` (≈6 at 1080p, 7 at 1440p+). Each level's `ImageDesc` is `R16G16B16A16_SFLOAT`, `STORAGE | SAMPLED`, initial layout `UNDEFINED`.

> Decision to record when building: **one keyed image per level, not a single mip-view image.** `acquire_image` returns exactly one view per key, so a `mip_levels > 1` allocation would not expose per-level views; distinct keys (`bloom-mip-0`..) is the correct fit for the transient pool and keeps each level's barriers independent.

## 5 — The render-graph passes and renderer state

**File `engine/crates/rendering/src/renderer.rs`.**

Add `bloom` to `FramePipelines` (l.247-345), resolve it in the request block (l.4676-4716), and populate it at l.4718 — exactly as the tonemap PSO threads through. Add `Renderer::add_bloom_pass` mirroring `add_tonemap_pass` (l.7684) built on `add_compute_pass` (l.7854): acquire the transient mips, `graph.import_image` each (`import_image(handle, view, COLOR, UNDEFINED, None)`, `render_graph.rs` l.464), then emit the chain of compute dispatches, each declaring its usage so the graph derives every `GENERAL ↔ SHADER_READ_ONLY` transition.

```rust
// downsample i:  (mip[i-1] | color as src) SampledReadCompute → mip[i] StorageImageRwCompute
// upsample  j:   mip[j+1] SampledReadCompute → mip[j] StorageImageRwCompute (tent add)
// composite:     mip[0] SampledReadCompute + color StorageImageRwCompute (lerp in place)
graph.add_pass(RgPass::compute("bloom-downsample-0", …)
    .access(color,  RgUsage::SampledReadCompute)
    .access(mip[0], RgUsage::StorageImageRwCompute));
```

- Call `self.add_bloom_pass(...)` in `Renderer::render` **between** the resolve / `ssgi-history` copy block (the last linear-HDR reader of `color`, ends ~l.5777) and `self.add_tonemap_pass` (~l.5784). The exact arm order is `fxaa/taa/scene_resolve → [ssgi-history] → bloom → tonemap → depth-upscale → motion-visualize → lit-wireframe → grid+overlay`.
- Add bloom state fields near `exposure_ev` (l.481) and `tonemap_mode` (l.596): `bloom_enabled: bool`, `bloom_intensity: f32`, `bloom_scatter: f32`, `bloom_tint: [f32; 3]`, `bloom_threshold: f32`, with defaults (`enabled` off, intensity `0.05`, scatter `0.005`, tint white, threshold `0.0`). Add `pub` setters/getters mirroring `set_exposure` (l.3733) / `exposure_ev` (l.3738), and skip the whole pass when `!bloom_enabled`.
- Mirror the five fields into `RenderStatsFull` (next to `exposure_ev` l.241, populated ~l.3865) so the panel reads live state.

> Decision to record when building: **bloom reads pre-exposure `color`.** Exposure is grade op #1 *inside* the tonemap pass (`TonemapPush::new` applies `exp2(ev)`), so bloom composites into radiance before that multiply. Because the composite is an energy-conserving relative-fraction lerp this is correct and kept — no duplicate exposure multiply is injected into the bloom stage.

**File `engine/crates/rendering/src/overlay.rs`.**

Add the bloom pass name(s) to `final_post_pass_names` (l.285) so the unit-tested post-chain arm order stays exact. The `BloomPush` `#[repr(C)]` `Pod`/`Zeroable` struct lives here beside `TonemapPush` (l.86), with a `size_of == 32` assert.

## 6 — Protocol DTO and command table

**File `engine/crates/protocol/src/dto.rs`.**

Add the params/result pair with the full derive stack (params also `Default`), mirroring `SetExposureParams` (l.3187):

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetBloomParams {
    pub enabled: bool,
    pub intensity: f32,
    pub scatter: f32,
    pub tint: [f32; 3],
    pub threshold: f32,
}
```

`SetBloomResult` carries the same five fields (echoing applied state, no `Default`). Add flat read-back fields to `RenderStatsDto` next to `tonemap` (l.407): `bloom_enabled`, `bloom_intensity`, `bloom_scatter`, `bloom_tint`, `bloom_threshold` — the panel reads live values from `render-stats`, not the set-echo.

**File `engine/crates/protocol/src/command.rs`** — the four edits, placed in the render-domain region beside `set-tonemap` (l.181) / `set-exposure` (l.715); position sets the frozen wire order:

- `COMMANDS` row `CommandSpec { name: "set-bloom", summary: "…", params: "SetBloomParams", result: "SetBloomResult" }`.
- `COMMAND_FIXTURES` tuple `("set-bloom", "bloom")`.
- `DTO_TYPE_NAMES` entries `SetBloomParams`, `SetBloomResult`.
- `render_domain()` (l.1826) gains `"set-bloom"`.

**File `engine/crates/protocol/src/codegen.rs`** — add `decl_entry!(SetBloomParams)`, `decl_entry!(SetBloomResult)` to `ts_decls()` and `frag_entry!(...)` for both to `fragment_decls()`. **Tripwires:** add both to the `inventory!` list in `tests/inventory.rs` and a `check!(...)` line per struct in `tests/schema_fragments.rs`; `schema.rs` `positional_field_order` follows the field order automatically.

## 7 — Control seam

**File `engine/crates/control/src/registry.rs`.**

Extend the `ControlRenderer` trait with a primitive-typed setter (no dependency on rendering internals) plus getters used by the stats builder, mirroring `set_exposure` (l.197) / `exposure_ev` (l.195):

```rust
fn set_bloom(&mut self, enabled: bool, intensity: f32, scatter: f32, tint: [f32; 3], threshold: f32);
fn bloom_enabled(&self) -> bool;
fn bloom_intensity(&self) -> f32;
fn bloom_scatter(&self) -> f32;
fn bloom_tint(&self) -> [f32; 3];
fn bloom_threshold(&self) -> f32;
```

**File `engine/crates/host/src/control_renderer.rs`.** Implement the trait methods delegating to `saffron_rendering::Renderer` (beside `set_exposure` l.285). The same methods are added to the test stub in `engine/crates/control/src/test_support.rs` (l.262/430) and any second impl in `commands_asset.rs` (l.790/794) — all three impls in the same change.

**File `engine/crates/control/src/commands_render.rs`.** Register the handler beside `set-tonemap` (l.800), validating ranges (`intensity >= 0.0`, `scatter` in `[0, 1]`, `threshold >= 0.0`) and returning `Err(Error::command(...))` on bad input:

```rust
reg.register::<SetBloomParams, SetBloomResult>("set-bloom", "Set bloom parameters", |ctx, p| {
    if !(p.intensity >= 0.0 && (0.0..=1.0).contains(&p.scatter) && p.threshold >= 0.0) {
        return Err(Error::command("bloom parameters out of range"));
    }
    ctx.renderer.set_bloom(p.enabled, p.intensity, p.scatter, p.tint, p.threshold);
    Ok(SetBloomResult {
        enabled: ctx.renderer.bloom_enabled(),
        intensity: ctx.renderer.bloom_intensity(),
        scatter: ctx.renderer.bloom_scatter(),
        tint: ctx.renderer.bloom_tint(),
        threshold: ctx.renderer.bloom_threshold(),
    })
});
```

Populate the new `RenderStatsDto` bloom fields in `render_stats_dto()` (l.191, next to `tonemap` l.223). The `sa` CLI needs **no** per-command code: it forwards any name in `saffron_protocol::COMMANDS`, folding positionals via `positional_field_order`.

## 8 — Persistence

**File `engine/crates/rendering/src/render_settings.rs`.**

Add `Option<T>` fields to `RenderSettings` (l.16-45): `bloom_enabled: Option<bool>`, `bloom_intensity`, `bloom_scatter`, `bloom_tint: Option<[f32; 3]>`, `bloom_threshold`. Add matching keys in `settings_to_json` (l.50) and `parse_render_settings` (l.71), a `Some(getter)` per field in `Renderer::render_settings_to_json` (l.105), and an apply arm in `apply_render_settings` (l.127) calling `set_bloom(...)`. Update the three frozen-key unit tests (`block_has_the_frozen_keys`, `parse_then_serialize_round_trips`, `missing_and_malformed`, l.188/233/272) so the persisted `renderSettings` block round-trips the new keys.

## 9 — Editor and codegen

- **Regenerate:** run `cargo run -p xtask -- gen-protocol` (or `bun run gen:protocol` from `editor/`). This rewrites `editor/src/protocol/sa-types.ts` (`CommandParamsMap`/`CommandResultMap` gain `set-bloom`) and `schemas/control/{openrpc,command-manifest}.generated.json` + `sa.generated.luau`. **Never hand-edit** `sa-types.ts`.
- **File `tools/check-control-schema/check.ts`** — add a `paramsForFixture()` case `"bloom"` returning a params object (e.g. `{ enabled: true, intensity: 0.08, scatter: 0.005, tint: [1, 1, 1], threshold: 0 }`), beside `"tonemap"` (l.247). The live-vs-schema contract test is part of the gate and throws on a missing fixture case.
- **File `editor/src/protocol/index.ts`** — re-export the new generated type names from the shim.
- **File `editor/src/control/client.ts`** — add a hand-authored typed wrapper `setBloom(params) => call("set-bloom", params)` beside `setExposure` (l.848); the generic `call<C extends CommandName>` compile-checks the name against the regenerated union.
- **File `editor/src/panels/RenderPanel.tsx`** — add bloom rows in the render config surface: a `Switch` (enable), `NumberDrag`/`SliderField` for intensity and scatter, and a `ColorField` for tint, sectioned with the existing `Separator` + uppercase muted `Label` pattern (the "Debug" divider). Follow the exposure write path exactly — read live state from the `renderStats` slice, funnel scrub streams through `makeCoalescer`, fold the echoed result optimistically, and record one `pushEdit(..., "scene")` undo per gesture via the `onDragStart`/`onDragEnd` bracket. Labels via `humanizeFieldName`, semantic Tailwind tokens only.

## Out of scope (later phases)

Lens-dirt masking, anamorphic streaks, and the per-mip art-directed tint/size stack are Phase 2 (`phase-2-bloom-lens-dirt-anamorphic.md`) — they share this phase's pre/post compositing seam. FFT-convolution bloom (authorable `.exr` PSF kernel) is deliberately Future, unscheduled: it swaps only the blur stage behind the shared composite, so this phase leaves that seam clean but does not build the FFT compute infra. Color grading is Phases 3-5 and folds into the tonemap pass, not here.

## Known interactions (note, don't over-engineer)

- **Threshold is a labeled non-physical escape hatch.** Default `0.0` (off) so emissive/HDR-bright pixels bloom automatically; the soft-knee prefilter on the `color → mip0` downsample only activates when `threshold > 0.0`. It is documented as non-physical, not the recommended path — do not build a full bright-pass stage around it.
- **Full-res mip0 variant.** Some pipelines keep the pyramid's first level at full `published_extent` for extra TAA stability. This phase keeps the standard half-res-first chain; the Karis average already carries the firefly/TAA win, so full-res mip0 is a later scatter-quality refinement, not a phase-1 requirement.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot an emissive test scene headless (`just run-engine-headless`) with a validation-clean log and confirm visible glow around bright/emissive surfaces; check `sa set-bloom intensity 0.08` changes the frame. Because a wire type changed, run `bun run check` in `editor/` (regenerates `@saffron/protocol` — `CommandName` must gain `set-bloom`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that toggles bloom over the control plane and asserts the `render-stats` echo plus a validation-clean log. Add the `docs/content/` post-processing hub `_index.md` bloom row and the new `Bloom` concept page (the energy-conserving mip pyramid, thresholdless/Karis rationale, lerp-vs-additive, scatter/intensity/tint, the pre-tonemap scene-linear ordering, and the `What | File | Symbols` table: `bloom.slang`, `add_bloom_pass`, `SetBloomParams`; FFT convolution noted as Future) in the same change.
