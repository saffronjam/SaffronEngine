# Phase 2 — Bloom polish: lens dirt, anamorphic streaks, per-mip tint

**Status:** COMPLETED

Part of `plans/post-processing/` (an energy-conserving bloom pyramid + a scene-referred grade on the display-extent `color` target). This is the second phase, layered directly on Phase 1's core pyramid. It adds three art-direction controls over the *same* pre/post compositing — a lens-dirt mask multiplied into the accumulated bloom, horizontally-squeezed anamorphic streaks added over the radial result, and an optional per-mip tint applied during upsample — without introducing a second bloom command, a second push struct, or a second persistence block. It does **not** touch the color grade (Phases 3–5), and it explicitly leaves the blur stage swappable so FFT convolution bloom can replace it later (Future) with no compositing rewrite.

## Goal

The bloom composite becomes art-directable while staying energy-conserving:

- The accumulated pyramid is multiplied by a **lens-dirt mask** texture before the `lerp(hdr, bloom, intensity)` composite — an `intensity` (0 = no dirt, mask fully mixed at 1) and a `tint`, with the mask clamped `≤ 1.0` so dirt only *attenuates* glow, never adds energy.
- An optional **anamorphic streak** buffer — a 2× horizontally-squeezed blur of the bright pyramid, cool-tinted (Bart Wronski) — is added over the radial bloom before compositing, behind an `enabled` toggle with `ratio` / `tint` / `intensity` knobs.
- An optional **per-mip tint** stack multiplies each mip's contribution during the progressive tent upsample (UE #1–#5 warm-core / cool-halo), selected per upsample pass on the CPU so the shader carries one tint at a time.
- All three ride the **one** `set-bloom` command (`SetBloomParams` gains the fields), the **one** `RenderStatsDto` bloom read-back, the **one** `renderSettings` bloom block, and the **one** `RenderPanel` bloom section. No `set-bloom-dirt`, no `set-anamorphic`, no `AnamorphicPush`.

## NO-LEGACY checklist for this phase

- **One command, extended — not forked.** The new controls are additional `Option<T>` fields on the existing `SetBloomParams`/`RenderStatsDto` bloom block from Phase 1. There is **no** `set-bloom-dirt` / `set-anamorphic` command and **no** second params struct. The `set-bloom` handler in `commands_render.rs`, the `client.ts` `setBloom` wrapper, and the `paramsForFixture` `"bloom"` case all keep working because Phase 1's params are a merge/patch of `Option` fields.
- **One push struct.** The `BloomPush` introduced in Phase 1 (`engine/crates/rendering/src/overlay.rs`, next to `TonemapPush`) gains the dirt / mip-tint / anamorphic fields. No sibling `AnamorphicPush` or `DirtPush` — the streak passes reuse `BloomPush` and read the fields they need.
- **One descriptor layout.** `create_bloom_layout` (`engine/crates/rendering/src/descriptors.rs`) gains the dirt-mask sampler binding on the composite set; there is no second "bloom composite layout". The dirt binding is always valid because it defaults to the renderer's 1×1 white fallback (mask = 1 ⇒ identity), so an absent dirt texture is not a special code path.
- **One persistence block.** The `bloom` object in `render_settings.rs` gains keys in the *same* `RenderSettings` struct and the *same* three frozen-key tests — no second block, and the tests are updated to assert the extended key set, not the Phase 1 subset.

## 1 — Shader: dirt multiply, anamorphic streak, per-mip tint

**File `engine/assets/shaders/bloom.slang`.**

Phase 1 gives this shader a `downsample` entry (13-tap, Karis flag via push on mip0) and an `upsample` entry (9-tap tent, `filterRadius` UV). Phase 2 adds a `streak` entry (a horizontal-only blur for the anamorphic buffer) and extends the `upsample` and final composite to read the new push fields. All three entry points share the one `BloomPush`; a new entry auto-compiles to `bloom.spv` with no `xtask` edit (`engine/xtask/src/shaders.rs`, entry-point loop).

The composite step (the final upsample pass that reads `color` + mip1 + the streak buffer and writes `color`) multiplies the accumulated pyramid by the dirt mask, adds the tinted streak, then does the Phase 1 energy-conserving lerp:

```hlsl
// composite: `bloom` is the accumulated radial pyramid at this pixel
float3 dirt = dirtMask.SampleLevel(linearSampler, uv, 0).rgb;      // asset or 1x1 white
dirt = min(dirt, 1.0.xxx) * push.dirtTint;                          // mask <= 1.0: attenuate only
bloom *= lerp(1.0.xxx, dirt, push.dirtIntensity);
bloom += streak.SampleLevel(linearSampler, uv, 0).rgb;             // cool-tinted, pre-scaled
float3 outc = lerp(hdr, bloom, push.intensity);                    // Phase 1 composite, unchanged
```

The per-mip tint multiplies each upsampled contribution (identity `1,1,1` when off):

```hlsl
// upsample entry, progressive one step per mip
float3 up = tentUpsample(lowerMip, uv, push.scatter);
target[px] = float4(existing.rgb + up * push.mipTint, 1.0);
```

The `streak` entry is a separable horizontal blur whose tap offset is `push.scatter * push.anamorphicRatio` in U and `push.scatter` in V (ratio ≈ 2 squeezes the kernel horizontally into a streak), tinted by `push.anamorphicTint` and scaled by `push.anamorphicIntensity` into the buffer.

- Add the `streak` entry point and extend `upsample` + the composite path to read the new push fields.
- Bind the dirt mask and streak buffer as sampled inputs on the composite set (§2).

> Decision to record when building: **dirt is multiplicative and clamped, not additive.** A lens-dirt mask models occlusion/scatter *on the glow that already exists*, so it multiplies the accumulated bloom (`min(mask,1)`), never adds energy — this keeps the composite energy-conserving and matches Unreal's dirt-mask semantics. The streak, by contrast, *is* extra bloom energy and so is added over the radial result before the single `lerp` composite. Pick one coherent rule (multiply dirt, add streak), not a per-artist blend-mode switch.

## 2 — Dirt-mask binding + white fallback

**File `engine/crates/rendering/src/descriptors.rs`.**

`create_bloom_layout` from Phase 1 is `create_fxaa_layout`-shaped (a `COMBINED_IMAGE_SAMPLER` source at binding 0 + a `STORAGE_IMAGE` target at binding 1). The composite pass needs two more sampled inputs — the dirt mask and the streak buffer — so the layout gains binding 2 (dirt, `COMBINED_IMAGE_SAMPLER`) and binding 3 (streak, `COMBINED_IMAGE_SAMPLER`), both `compute_binding(slot, COMBINED_IMAGE_SAMPLER)`. The downsample/upsample passes ignore bindings 2–3 (bound to harmless views); only the composite entry samples them.

```rust
// create_bloom_layout: composite set
// 0: COMBINED_IMAGE_SAMPLER  source mip
// 1: STORAGE_IMAGE           `color` (rw composite)
// 2: COMBINED_IMAGE_SAMPLER  dirt mask (asset, or 1x1 white fallback)
// 3: COMBINED_IMAGE_SAMPLER  anamorphic streak buffer (transient)
```

- Bump the descriptor-pool `COMBINED_IMAGE_SAMPLER` budget for the two new per-view sampled bindings (the `29*views` / sampler terms in `create_descriptor_pool`), mirroring the Phase 1 `STORAGE_IMAGE` bump — an undersized pool silently fails allocation.

**File `engine/crates/rendering/src/view_target.rs`.**

The dirt mask and streak buffer are written into the per-view bloom composite set (`bloom_set` from Phase 1) in `write_aa_sets` via `Binding::sampled(set, 2, linear_sampler, dirt_view)` / `Binding::sampled(set, 3, linear_sampler, streak_view)`. The dirt view is resolved from the bloom state (an `AssetId` → texture view, §3), defaulting to the renderer's 1×1 white texture when no dirt asset is set, so the binding is always valid and mask = 1 is identity. The streak buffer is a transient allocation (§3), bound the same way Phase 1 binds the mip chain into `bloom_set`.

- Add the dirt view + streak view to the arguments `write_aa_sets` writes into `bloom_set`; both rewrite on a `generation` bump (resize) exactly like the mip bindings.
- Size the streak buffer off `published_extent()` (half the bloom mip0 chain res is enough for streaks; a quarter is fine).

> Decision to record when building: **absent dirt is the white fallback, not a shader branch.** Binding a 1×1 white texture (the same default the material system uses for a missing map) when no dirt asset is set keeps one composite code path — the shader always samples `dirtMask`, and `dirtIntensity = 0` or a white mask is the identity. No `#ifdef HAS_DIRT`.

## 3 — Renderer state, streak passes, per-mip tint push

**File `engine/crates/rendering/src/renderer.rs`.**

Extend the Phase 1 bloom state on `Renderer` with the new fields and their `pub` setters/getters (near the Phase 1 `bloom_enabled`/`bloom_intensity`/`bloom_scatter`/`bloom_tint` fields):

```rust
// bloom art-direction state (Phase 2), alongside the Phase 1 bloom fields
bloom_dirt_texture: Option<AssetId>,
bloom_dirt_intensity: f32,
bloom_dirt_tint: [f32; 3],
bloom_anamorphic_enabled: bool,
bloom_anamorphic_ratio: f32,      // horizontal squeeze, ~2.0
bloom_anamorphic_tint: [f32; 3],  // cool default ~[0.6, 0.8, 1.0]
bloom_anamorphic_intensity: f32,
bloom_mip_tint: Vec<[f32; 3]>,    // per-upsample-step, identity 1,1,1 when off
```

`add_bloom_pass` (Phase 1) grows two ways. First, when `bloom_anamorphic_enabled`, it acquires a streak buffer from the transient pool (`transient.acquire_image(frame, "bloom-streak-0", ..)` / `"bloom-streak-1"` for a horizontal ping-pong) and records `streak` compute passes reading the bright mip and writing the streak buffer, declared `(streak, StorageImageRwCompute)` / `(mip, SampledReadCompute)` so the graph derives the barriers. Second, each progressive upsample pass selects `bloom_mip_tint.get(level).copied().unwrap_or([1.0; 3])` into its `BloomPush.mip_tint`, and the final composite pass fills `dirt_*` / `anamorphic_*` from state. The dirt `AssetId` is resolved to a texture view here (the same catalog lookup materials use), passed to `write_aa_sets` so `bloom_set` binding 2 points at it.

- Mirror the new fields into `RenderStatsFull` for read-back (as Phase 1 mirrors `bloom_*`), populated where the Phase 1 bloom stats are populated.
- Streak passes run only when `bloom_anamorphic_enabled`; when off, streak binding 3 points at the black/white fallback and `anamorphic_intensity = 0`, so no streak energy is added.

**File `engine/crates/rendering/src/overlay.rs`.**

Extend the Phase 1 `BloomPush` with the dirt / mip-tint / anamorphic fields (`#[repr(C)]` `Pod`/`Zeroable`, keep the `bytemuck` size assert, 4-byte-aligned so no padding):

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BloomPush {
    pub intensity: f32,          // Phase 1
    pub scatter: f32,            // Phase 1 filterRadius UV
    pub karis: u32,              // Phase 1: 1 on mip0 downsample only
    pub dirt_intensity: f32,
    pub dirt_tint: [f32; 3],
    pub mip_tint: [f32; 3],
    pub anamorphic_intensity: f32,
    pub anamorphic_ratio: f32,
    pub anamorphic_tint: [f32; 3],
    pub _pad: f32,               // 64 bytes, well under the 128-byte push floor
}
```

- Extend `BloomPush::new` (or add a builder) so downsample/upsample/streak/composite each set the fields they use and leave the rest at their identity defaults.
- The Phase 1 bloom pass name(s) already in `final_post_pass_names` cover the arming test; add the `streak` pass name(s) so the order gate mirrors the new arms.

> Decision to record when building: **per-mip tint is one color per pass, selected on the CPU — not an array in the push.** The tent upsample is already progressive (one dispatch per mip), so `add_bloom_pass` indexes `bloom_mip_tint[level]` into `BloomPush.mip_tint` for that dispatch. This keeps the push tiny and needs no `[[float3; N]]` uniform; the DTO carries the stack, the renderer fans it out per pass.

## 4 — Protocol: extend `SetBloomParams` + `RenderStatsDto`

**File `engine/crates/protocol/src/dto.rs`.**

Add the fields to the existing `SetBloomParams` (patch of `Option`s from Phase 1) and to the bloom read-back on `RenderStatsDto` — no new struct, no new command:

```rust
// SetBloomParams (extended) — every field Option<T>, merge/patch semantics
pub dirt_texture: Option<AssetId>,
pub dirt_intensity: Option<f32>,
pub dirt_tint: Option<[f32; 3]>,
pub anamorphic: Option<AnamorphicParams>,   // { enabled, ratio, tint, intensity }
pub per_mip_tint: Option<Vec<[f32; 3]>>,
```

Because these are `Option` fields with `#[serde(default)]`, the Phase 1 `"bloom"` fixture (`{ intensity: 0.5 }`) still deserializes and the TS `SetBloomParams` fields stay optional — a partial `set-bloom` call touching only `dirtIntensity` works.

- `AnamorphicParams` (if introduced as a nested DTO) gets the full derive stack (`Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS`, `#[serde(rename_all="camelCase")]`, `#[ts(export)]`) and is added to `codegen.rs` (`ts_decls` + `fragment_decls`), `tests/inventory.rs` (`inventory!`), and `tests/schema_fragments.rs` (`check!`) — the DTO tripwires. `SetBloomParams` and `RenderStatsDto` are already in those lists from Phase 1; only a *new* nested struct needs new entries.
- **No** `command.rs` 4-edit dance: `set-bloom` already has its `COMMANDS` row, `COMMAND_FIXTURES` `("set-bloom","bloom")`, `DTO_TYPE_NAMES`, and `render_domain()` membership from Phase 1. Extending its params does not add a command.

## 5 — Control seam + persistence + contract test

**Files `engine/crates/control/src/registry.rs`, `engine/crates/host/src/control_renderer.rs`, `engine/crates/control/src/test_support.rs`, `engine/crates/control/src/commands_asset.rs`.**

The `ControlRenderer` trait (registry.rs) gains getters/setters for the new bloom state (e.g. `set_bloom_dirt(&mut, Option<AssetId>, f32, [f32;3])`, `set_bloom_anamorphic(&mut, bool, f32, [f32;3], f32)`, `set_bloom_mip_tint(&mut, Vec<[f32;3]>)` plus their getters), implemented in the host `control_renderer.rs` delegating to `saffron_rendering::Renderer`, and stubbed in `test_support.rs` and the second impl in `commands_asset.rs` — the same three-impl pattern Phase 1 established.

**File `engine/crates/control/src/commands_render.rs`.**

The Phase 1 `set-bloom` handler is extended to read the new `Option` fields off `SetBloomParams` and call the new trait setters (a texture set-then-echo, no enum to validate). `render_stats_dto()` populates the new bloom read-back fields from the trait getters so `RenderPanel` reflects live state.

**File `engine/crates/rendering/src/render_settings.rs`.**

The `bloom` object in `RenderSettings` gains `dirtTexture` / `dirtIntensity` / `dirtTint` / `anamorphic` / `perMipTint` keys: an `Option<T>` field each, a key in `settings_to_json` + `parse_render_settings`, a `Some(getter)` in `render_settings_to_json`, and an apply arm in `apply_render_settings`. The three frozen-key tests (`block_has_the_frozen_keys`, `parse_then_serialize_round_trips`, `missing_and_malformed`) are **rewritten to assert the extended bloom key set** — not the Phase 1 subset — so persistence round-trips the whole bloom block.

**File `tools/check-control-schema/check.ts`.**

The `paramsForFixture` `"bloom"` case already returns a valid params object from Phase 1; leave it, or extend it to exercise `anamorphic: { enabled: true, ratio: 2, tint: [...], intensity: 0.3 }` so the contract test covers the new nested DTO against the live schema.

- Run `cargo run -p xtask -- gen-protocol` (or `bun run gen:protocol`) to regenerate `editor/src/protocol/sa-types.ts` + `schemas/control/{openrpc,command-manifest}.generated.json` + `sa.generated.luau`; commit the regenerated artifacts. `sa set-bloom` needs no CLI code — the new nested/array params fold through the generic positional path (flags for named fields).

## 6 — Editor: dirt slot + anamorphic rows

**File `editor/src/control/client.ts`.**

The `setBloom` wrapper is unchanged in shape — it already forwards `call("set-bloom", params)` and the generated `SetBloomParams` now carries the new optional fields. Partial calls (`setBloom({ dirtIntensity })`) type-check against the regenerated `CommandParamsMap`.

**File `editor/src/panels/RenderPanel.tsx`.**

Extend the Phase 1 bloom section (the `Separator` + uppercase-`Label` pattern) with:

- a **dirt texture slot** — an asset-drop slot that opens the asset picker and sets `dirtTexture` to the chosen `AssetId` (reuse the material texture-slot pattern), plus a `NumberDrag` `dirtIntensity` (0..1) and a `ColorField` `dirtTint`;
- an **anamorphic** `Switch` (`enabled`) gating a `NumberDrag` `ratio` (1..4), a `SliderField` `intensity` (0..1), and a `ColorField` `tint`;
- (optional) a small per-mip tint row group behind a collapse, driving `perMipTint` — a `ColorField` per mip level.

All rows funnel scrub streams through `makeCoalescer`, fold the echoed `RenderStatsDto` optimistically, and record one undo entry per gesture via `pushEdit(prior, next, 'scene')` — the exact `onDragStart`/`onDragEnd` bracket the Phase 1 bloom rows use. Labels via `humanizeFieldName`; semantic Tailwind tokens only.

## Known interactions (note, don't over-engineer)

- **Bloom reads pre-exposure radiance (unchanged from Phase 1).** Exposure is grade op #1 *inside* the tonemap pass, so the dirt multiply and streak add operate on pre-exposure `color`. Because the composite is an energy-conserving relative-fraction `lerp`, this is acceptable and kept — dirt/anamorphic intensity is a fraction of the same radiance, so it tracks with the rest of the bloom. Documented, not duplicated into the bloom stage.
- **Streak buffer lives in the transient pool.** `acquire_image` returns one stable `(image, view)` per key across frames (grow-only, reset only post-fence), so the streak binding written into `bloom_set` is stable until a resize bumps `generation` and `write_aa_sets` rewrites it — the same lifetime as the mip chain. No per-frame descriptor churn.
- **Dirt is screen-space, not lens-aware.** The mask is sampled in screen UV (Unreal's default), so it does not track camera roll or FOV. That is the standard game approximation and is fine for this phase; a lens-model projection is out of scope.

## Out of scope (later phases / Future)

- **FFT convolution bloom** stays **Future (unscheduled)** — an authorable `.exr` PSF kernel (aperture diffraction / star / anamorphic for free) would swap *only* the blur stage behind this same pre/post compositing, but it needs an FFT compute infrastructure this planset does not build. Phase 2 leaves that seam clean: dirt multiply, streak add, and per-mip tint all live in the composite/upsample stage, so a future blur swap does not touch them. Note this in the docs Bloom page as the deferred upgrade.
- **The color grade** (white balance, contrast, saturation, CDL, per-range, LUT) is Phases 3–5 and does not overlap this phase — bloom composites into `color` *before* the tonemap/grade stage.
- **The dedicated Post panel** and the `GradingWheel` / `ToneCurve` widgets are Phase 6; Phase 2's controls live in `RenderPanel` and migrate there in the Phase 6 cutover.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot an emissive fixture headless (`just run-engine-headless`) with a dirt-mask texture assigned and anamorphic on — a validation-clean log, the bloom visibly masked by the dirt texture and carrying a cool horizontal streak, and `sa set-bloom` with `anamorphic` enabled / a `dirtTexture` set changing the frame. `bun run check` in `editor/` (regenerates `@saffron/protocol` via `xtask gen-protocol` — the new `SetBloomParams` fields and any `AnamorphicParams` type appear; `CommandName` still carries the one `set-bloom`, never hand-edit `sa-types.ts`), and `just e2e` with a `tests/e2e` case driving `set-bloom` with the dirt/anamorphic fields over the control plane and asserting the read-back on `render-stats`. Update the `docs/content/` post-processing **Bloom** explanation page (add the lens-dirt and anamorphic-streak sections, the per-mip tint note, and the FFT-convolution "Future" note) and its hub `_index.md` row in the same change.
