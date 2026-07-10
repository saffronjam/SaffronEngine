# Phase 5 — Creative 3D LUT: `.cube` import, post-tonemap apply, bake

**Status:** NOT STARTED

Part of `plans/post-processing/` (bloom + color grading as one post subsystem). This is the fifth phase, built directly on Phase 3's folded grade — it depends on the per-view grade uniform buffer and the `set-color-grading` / `SetColorGradingParams` seam Phase 3 lands, and does **not** depend on the tonal-range work in Phase 4. It adds the industry-standard *creative look* stage that shipping engines put **after** the view transform: a display-referred `.cube` 3D LUT sampled tetrahedrally on the bounded `[0,1]` output and dialled in by intensity, imported into the asset catalog. It then adds the **bake** — a GPU pass that folds the view transform + creative LUT + a frozen grade into a single `33³` log2-shaper LUT for the exported `saffron-player` and for ingesting external full-tail `.cube` looks. This is the classic game color-LUT (Unity `Color Lookup`, Godot `adjustment_*`, UE color-grading LUT) done correctly: the grade stays scene-referred ALU (Phase 3), the *look* is a bounded display-space table.

## Goal

- **A creative `.cube` importer in the asset catalog.** Parse `LUT_3D_SIZE 17|33|65` (with optional `TITLE`/`DOMAIN_MIN`/`DOMAIN_MAX`) into a `CubeLut` asset backed by an `R16G16B16A16_SFLOAT` **3D** device image, sized `N×N×N`, alongside the existing `.smat`/texture importers.
- **One more op inside the existing tonemap compute pass.** `tonemap.slang` samples the creative LUT on the display color **after** `tonemapAndEncode`, tetrahedrally, and `lerp`s by intensity — no new render-graph pass, no second grade command, no "LUT enabled" branch (a size-2 identity LUT is always bound; intensity `0` is the neutral, one code path).
- **The LUT slot rides `SetColorGradingParams`.** Extend the Phase 3 DTO with `creativeLutAsset: Option<AssetId>` + `creativeLutIntensity: f32`; extend the grade uniform buffer and `RenderStatsDto` read-back; persist under the `colorGrading` block. **No new `set-lut` command** — the one `set-color-grading` handler is extended.
- **A GPU bake that reuses the same math.** A `bake-look` compute pass evaluates `grade() → tonemapAndEncode() → creative LUT` over a `33³` log2-shaper grid spanning EV `[-14, +11]`, reads it back, and writes a `.slut` look table the player and external-ingest paths consume — **the shared `tonemap_ops.slang` helpers, not a divergent Rust re-derivation of the tail**.
- **Editor: a LUT drop slot in `RenderPanel`.** An asset-reference drop target + intensity `SliderField` + a size/interp readout, on the same optimistic-fold + per-gesture `pushEdit` undo path as exposure/tonemap.

## NO-LEGACY checklist for this phase

- The creative-LUT slot is **new fields on `SetColorGradingParams`** (from Phase 3), not a new `set-lut`/`set-look` command. Every reader (`commands_render.rs` handler, `render_settings.rs`, `RenderPanel.tsx`, the e2e case) speaks the one extended command; the `check.ts` `"color-grading"` fixture is extended, not duplicated.
- The LUT is **one op appended inside `tonemap.slang`'s `computeMain`**, after `tonemapAndEncode`, sharing the tonemap set. No parallel "color-correction" compute pass is added, and `add_bloom_pass`/`add_tonemap_pass` in `renderer.rs` are untouched in count.
- The bake reuses `grade()`, `tonemapAndEncode()`, and the new `sampleCubeTetrahedral()` helper **verbatim** — there is no CPU mirror of the grade/DRT math. A LUT baked from a look is bit-identical to the live ALU up to shaper quantization.
- Neutral is a single always-bound **identity** LUT (size-2 ramp) at intensity `0`, not a boolean that skips the sample. There is exactly one tonemap tail.
- The player's frozen-look path and the host's live-ALU path are selected by one `frozenLook: uint` field in the grade UBO — a view-mode flag of the same shape as the existing tonemap `mode`, **not** a compat shim kept beside a live path.

## 1 — `.cube` import into the asset catalog

**File `engine/crates/assets/src/cube.rs` (new), registered in the catalog importers.**

A `.cube` is a small text file: an optional `TITLE`, a `LUT_3D_SIZE N`, optional `DOMAIN_MIN`/`DOMAIN_MAX` (default `0 0 0` / `1 1 1`), then `N³` whitespace-separated `r g b` triples ordered **red-fastest**. Parse it into a size-tagged buffer, reject anything but `17|33|65`, and upload it to an `R16G16B16A16_SFLOAT` **3D** image via the catalog's existing staged-upload path (the same one the texture/`.smat` importers use for 2D images, extended to `vk::ImageType::TYPE_3D` + `vk::ImageViewType::TYPE_3D`).

```rust
/// A parsed creative look-up table imported from a `.cube` file.
pub struct CubeLut {
    pub size: u32,           // 17 | 33 | 65
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub rgb: Vec<[f32; 3]>,  // size^3 entries, red-fastest
}

#[derive(Debug, thiserror::Error)]
pub enum CubeError {
    #[error("unsupported LUT_3D_SIZE {0} (expected 17, 33 or 65)")]
    UnsupportedSize(u32),
    #[error("expected {expected} entries, found {found}")]
    EntryCount { expected: usize, found: usize },
    #[error("malformed line {line}: {reason}")]
    Malformed { line: usize, reason: &'static str },
}

pub fn parse_cube(text: &str) -> Result<CubeLut, CubeError> { /* … */ }
```

- Add `import_cube_lut(catalog, path) -> Result<AssetId, …>` next to the other importers; it parses, uploads the 3D image, and registers a `CubeLut` asset keyed by the catalog's `AssetId` (the existing `Uuid` newtype already used to reference `.smat` assets from `MaterialSet`).
- The device image is created `SAMPLED | TRANSFER_DST`, `TYPE_3D`, extent `N×N×N`, one mip, bound with `linear_sampler()` and clamp-to-edge (`descriptors.rs`, `linear_sampler`). The shader fetches corners with integer `Load`, so the sampler is nominal — clamp is what matters at the cube faces.
- Add the `.cube` extension to the catalog's importer dispatch and to the Asset Store's accepted look formats, so a dragged/imported `.cube` lands as a `CubeLut` asset the RenderPanel slot can reference.

> Decision to record when building: **the imported 3D image is `R16G16B16A16_SFLOAT`, not `R8G8B8A8_UNORM`.** A creative look is display-referred but graders author on 10-bit+ timelines; a half-float table removes banding on smooth skies/skin for a trivial VRAM cost (`33³ × 8 B ≈ 287 KB`) and keeps the bake output (also `16F`) round-trip-clean.

## 2 — The creative-LUT descriptor binding on the tonemap set

**File `engine/crates/rendering/src/descriptors.rs` — `create_tonemap_layout`.**

Phase 3 leaves the tonemap set at **binding 0** = storage image (`color`, `StorageImageRwCompute`, GENERAL) and **binding 1** = the grade UBO. Add **binding 2** = a `COMBINED_IMAGE_SAMPLER` for the creative LUT 3D image, mirroring how `create_fxaa_layout` pairs a sampled source with its storage target.

- Extend `create_tonemap_layout` with the binding-2 `compute_binding(2, COMBINED_IMAGE_SAMPLER)`; bump the `COMBINED_IMAGE_SAMPLER` term in `create_descriptor_pool`'s budget by the per-view tonemap-set count, exactly as the bloom phase bumps the `STORAGE_IMAGE` term.
- Keep a device-shared **identity** 3D LUT (a `2×2×2` neutral ramp, uploaded once in `Descriptors::new`) as the default bound at binding 2 whenever no creative LUT is assigned — so the descriptor is never dangling and the shader never branches on presence.

**File `engine/crates/rendering/src/view_target.rs` — `write_aa_sets` / the tonemap-set write, `Binding::sampled`.**

The per-view `tonemap_set` write gains binding 2 via `Binding::sampled(set, 2, sampler, lut_view)`, where `lut_view` is the resolved creative LUT view or the identity default. Because a LUT **swap** does not resize the view (no `generation` bump), add a small dirty flag so the renderer re-writes binding 2 on the next frame after `set_creative_lut`, reusing the same set-write machinery rather than reallocating.

- LUT targets are display-extent-agnostic (the LUT is `N³`, unrelated to `published_extent()`), so no transient-pool interaction — the LUT image lives in the asset catalog, not the frame pool.

## 3 — `tonemap.slang`: post-DRT tetrahedral LUT sample

**File `engine/assets/shaders/tonemap.slang` (`computeMain`) + `engine/assets/shaders/tonemap_ops.slang`.**

Today `computeMain` runs, after Phase 3, `grade(color.rgb, gradeUbo)` then `tonemapAndEncode(graded, push.mode)` and writes the display color. Phase 5 appends the look sample on the bounded `[0,1]` result and lerps by intensity:

```hlsl
float3 display = tonemapAndEncode(graded, push.mode);          // view transform (Phase 3 rename)
float3 look    = sampleCubeTetrahedral(creativeLut, saturate(display), grade.creativeLutSize);
display        = lerp(display, look, grade.creativeLutIntensity);
target[px]     = float4(display, a);
```

The tetrahedral helper lives in `tonemap_ops.slang` (already excluded from entry-point compile via `TONEMAP_OPS_STEM`) so it is shared identically by the live path **and** the bake shader. It takes the texture as a parameter (Slang passes `Texture3D` by argument), fetches the 4 corners of the tetrahedron the sample falls in, and blends — the Resolve/hardware-LUT interpolation, not trilinear:

```hlsl
float3 sampleCubeTetrahedral(Texture3D<float3> lut, float3 c, uint n) {
    float3 p = c * float(n - 1);
    int3   b = int3(floor(p));
    float3 f = p - float3(b);
    // pick the tetrahedron from the ordering of f.x, f.y, f.z; fetch 4 corners via Load
    // and return the barycentric-weighted blend (v000 + Δ along the tetra edges).
    …
}
```

- The LUT **size** rides the grade UBO (`creativeLutSize: u32`) so the shader scales `[0,1] → [0, n-1]` correctly for `17|33|65`.
- Extend the grade UBO layout (Phase 3's `#[repr(C)]` uniform struct) with `creativeLutIntensity: f32`, `creativeLutSize: u32`, and `frozenLook: u32`; keep the `bytemuck` `Pod`/`Zeroable` derive and the size assert.
- `overlay.rs` already renamed `TonemapMode`/`TonemapPush` semantics to "view transform" in Phase 3 — no push change here (the LUT parameters ride the UBO, not the push).

> Decision to record when building: **tetrahedral, applied post-DRT on `saturate(display)`.** Trilinear box interpolation skews hue along the cube diagonal (the classic LUT green/magenta shift); tetrahedral is what Resolve bakes against, so imported looks match their source. Applying it *after* the view transform is the industry split — the DRT is a fixed invertible image-formation step, the LUT is bounded display code-value look.

## 4 — `SetColorGradingParams` LUT slot + control + persistence

**File `engine/crates/protocol/src/dto.rs` — `SetColorGradingParams`, `RenderStatsDto`.**

Extend the Phase 3 params (not a new DTO) and the read-back:

```rust
// added to SetColorGradingParams (full derive stack + camelCase + ts(export), params: Default)
pub creative_lut_asset: Option<AssetId>,
pub creative_lut_intensity: f32,

// added to RenderStatsDto so the panel reads live state (asset, intensity, resolved size, interp label)
pub creative_lut: Option<CreativeLutStat>,
```

- `CreativeLutStat { asset: AssetId, intensity: f32, size: u32 }` is a new struct DTO — add it to `codegen.rs` (`ts_decls!` + `fragment_decls!`), `tests/inventory.rs`, and `tests/schema_fragments.rs`, per the tripwires. `set-color-grading` stays the one command; no `command.rs` `COMMANDS` row is added for the slot (only for `bake-look`, below).

**File `engine/crates/control/src/registry.rs` (`ControlRenderer`) + `engine/crates/host/src/control_renderer.rs`.**

Add trait methods; the **host** impl resolves the `AssetId` → the `CubeLut`'s 3D view + size via the asset catalog and hands the GPU handles to the renderer, so the control crate never touches rendering internals:

```rust
// registry.rs — ControlRenderer
fn set_creative_lut(&mut self, asset: Option<AssetId>, intensity: f32);
fn creative_lut(&self) -> Option<(AssetId, u32, f32)>;
```

- The host impl looks up the asset; on `Some`, it calls `renderer.set_creative_lut_texture(view, size, intensity)` (new setter storing the view + `creativeLutSize`/`creativeLutIntensity` UBO fields and flagging the tonemap-set rewrite). On `None`, it binds the identity default and zeroes intensity. Add the matching stub methods to `test_support.rs` and the second impl in `commands_asset.rs`.
- `commands_render.rs` extends the existing `set-color-grading` handler to read the two new params and call `set_creative_lut`; `render_stats_dto()` populates `creative_lut`. Validate `creativeLutIntensity ∈ [0,1]` and `Err(Error::command(..))` on an unknown/unresolvable asset id (mirror `set-tonemap`'s enum validation).

**File `engine/crates/rendering/src/render_settings.rs` — `RenderSettings`.**

Persist under the `colorGrading` block: `creativeLut: { asset: AssetId, intensity: f32 }` (the asset id — a `Uuid` — persists; the host re-resolves it to the texture on load). Add the `Option<T>` field, the `settings_to_json`/`parse_render_settings` keys, the `Some(..)` in `render_settings_to_json`, the apply arm in `apply_render_settings`, and update the three frozen-key tests (`block_has_the_frozen_keys`, `parse_then_serialize_round_trips`, `missing_and_malformed`).

**File `tools/check-control-schema/check.ts` — `paramsForFixture`.**

Extend the `"color-grading"` case to include `creativeLutIntensity: 0` (and `creativeLutAsset: null`) so the live-vs-schema contract test still round-trips.

## 5 — The bake: fold view transform + creative LUT + frozen grade into a `33³` log2-shaper LUT

**File `engine/assets/shaders/lut_bake.slang` (new compute) + `engine/crates/rendering/src/renderer.rs` (`bake_look_lut`) + `engine/crates/control/src/commands_render.rs` (`bake-look`).**

A shipped game freezes its look; the correct interchange for "the whole HDR→display tail as one table" is an OCIO-style **log2-shaper prelut + 3D LUT**. Because a bounded `.cube` cannot hold unbounded scene radiance directly, the shaper maps cube coordinate `t ∈ [0,1]` to scene-linear radiance across EV `[-14, +11]` anchored at 18% grey; the table then stores the display-referred result of the full tail at each shaper node.

The bake is a **GPU** pass that dispatches over a `33×33×33` storage 3D image and, per cell, runs the *same* shared helpers as the live path — guaranteeing bit-identity up to node quantization:

```hlsl
// lut_bake.slang — one invocation per cell (gid = int3 in [0,32]^3)
float3 t   = float3(gid) / 32.0;
float3 lin = 0.18 * exp2(lerp(EV_MIN, EV_MAX, t));            // log2 shaper: t -> scene-linear
float3 g   = grade(lin, gradeUbo);                            // Phase 3 grade, frozen UBO
float3 d   = tonemapAndEncode(g, gradeUbo.viewTransform);     // the view transform
float3 out = lerp(d, sampleCubeTetrahedral(creativeLut, saturate(d), gradeUbo.creativeLutSize),
                  gradeUbo.creativeLutIntensity);
bakedLut[gid] = out;
```

- `bake_look_lut()` acquires a `33³` `R16G16B16A16_SFLOAT` image, dispatches `lut_bake` with the current (frozen) grade UBO + bound creative LUT, then reads it back through a fenced staging buffer (`wait_gpu_idle`-safe like the thumbnail readback) and serializes a native `.slut` (`size:33`, `shaperEvMin:-14`, `shaperEvMax:+11`, `rgb16f`) into the project's baked-assets dir.
- Register a `bake-look` command in `command.rs` (the four edits: `COMMANDS` row, `COMMAND_FIXTURES` `("bake-look","bake-look")`, `DTO_TYPE_NAMES` for `BakeLookParams`/`BakeLookResult`, and `render_domain()`), its handler in `commands_render.rs`, and the `paramsForFixture` `"bake-look"` case. `BakeLookResult` returns the written LUT's `AssetId`/path so `sa bake-look` and the export step can consume it. `sa` needs no per-command code (generic, positional-folded).

**Player consumption — `saffron-player`'s tonemap tail.** The player never authors, so its `tonemap.slang` runs the **frozen-look** branch: `frozenLook = 1` in the grade UBO selects `coord = (log2(max(lin,eps)/0.18) - EV_MIN) / (EV_MAX - EV_MIN)` per channel, then `sampleCubeTetrahedral(bakedLut, coord, 33)` — a single dependent fetch replacing the ALU grade + DRT + creative LUT. The host runs `frozenLook = 0` (live ALU). Same shader file, two view-modes, one path per binary.

**External `.cube` ingest.** A full-tail log look authored elsewhere (Resolve, an OCIO bake) is imported through the *same* `.slut` runtime resource: its `DOMAIN_MIN/MAX` become the shaper bounds and its data the table. The player's frozen-look branch is provider-agnostic — it does not care whether the table came from `bake-look` or an external file. A display-domain external `.cube` (the common case) simply lands in the Section 1 creative slot instead.

> Decision to record when building: **the bake is a GPU pass over the shared helpers, not a CPU re-implementation of the tail.** Any Rust mirror of `grade()`/`tonemapAndEncode()` would drift from the shader on the next operator tweak; baking on the GPU with `tonemap_ops.slang` keeps the frozen player look identical to the live editor look by construction.

## 6 — Editor: LUT drop slot + intensity + size/interp readout

**File `editor/src/panels/RenderPanel.tsx` + `editor/src/control/client.ts`.**

Under the Phase 3 "COLOR GRADING" section (the `Separator` + uppercase-`Label` pattern), add a **Creative Look** row group:

- An asset-reference **drop slot** (reusing the same asset-reference/drop affordance the Inspector's material slot uses) that writes `creativeLutAsset` and a "Clear" affordance that writes `null`; both funnel through the existing optimistic fold + `pushEdit(..., 'scene')` undo, capturing the prior asset/intensity at drag/drop start.
- A `SliderField` for `creativeLutIntensity` (`0..1`), on `makeCoalescer` + `onDragStart`/`onDragEnd` like exposure.
- A read-only **size/interp** line resolved from `RenderStatsDto.creativeLut` (e.g. `33³ · Tetrahedral`), with the asset name resolved through the editor's asset store.
- Extend the existing `setColorGrading(...)` wrapper params with `creativeLutAsset`/`creativeLutIntensity` (no new client method for the slot); add a `bakeLook()` wrapper calling `call("bake-look", …)`. Run `bun run check` so `SetColorGradingParams` gains the fields and `CommandName` gains `bake-look`.

The dedicated color-wheel/tone-curve Post panel is Phase 6; this phase keeps the LUT controls in `RenderPanel` and Phase 6 migrates them (deleting the rows in the same change).

## Known interactions (note, don't over-engineer)

- **HDR output.** The creative LUT is bounded display-space `[0,1]`, correct for the SDR view this planset targets. A future HDR display view would apply the look in the HDR display domain (or skip it); the color-space decision keeps SDR-only here, so it is a docs note, not code.
- **Bake determinism.** `bake_look_lut` reads the current grade UBO and fences the readback; a `33³` shaper introduces the expected minor quantization versus the live ALU (the same trade every Resolve/Unity LUT bake makes). The live host path stays ALU, so the editor never sees the quantization.
- **LUT swap without resize.** A new `set-creative-lut` asset changes binding 2 without a `generation` bump; the per-view dirty flag (Section 2) re-writes just that binding next frame — no view-target reallocation.

## Out of scope (later phases)

- **Color wheels / tone curves** for the grade — Phase 6 (`GradingWheel.tsx`, `ToneCurve.tsx`) and the dedicated Post panel.
- **Per-volume / per-shot look blending** (post-process-volume style priority + blend weight) — a single global creative look this phase.
- **OCIO-driven looks.** When OCIO lands it *replaces* this switch: the creative slot becomes an OCIO look and `bake-look` becomes an OCIO bake (`GpuShaderDesc` 3D-LUT). Left as a clean seam — the shaper+tetrahedral machinery is exactly what an OCIO baker emits — not built beside it.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Import a `17³` and a `33³` `.cube` look into the catalog, boot an emissive scene headless (`just run-engine-headless`) with a validation-clean log, and confirm the graded frame shifts under the imported look and reverts at intensity `0`. Drive it over the control plane: `sa set-color-grading` with the LUT `AssetId` and `intensity 0.8`, then `sa bake-look`; boot `saffron-player` on the baked `.slut` and confirm its frame matches the host tail. `bun run check` in `editor/` (regenerates `@saffron/protocol` — `SetColorGradingParams` gains `creativeLutAsset`/`creativeLutIntensity`, `RenderStatsDto` gains `creativeLut`, and `CommandName` gains `bake-look`; never hand-edit `sa-types.ts`), and `just e2e` with a case that imports a `.cube`, applies it via `set-color-grading`, and asserts the `render-stats` read-back size/intensity. Update the `docs/content/` color-grading explanation page (add the creative-LUT and bake sections — post-DRT display-space look, tetrahedral, the log2-shaper bake, and `.cdl`/`.ccc` vs `.cube`/`.slut` interchange) and its hub `_index.md` row in the same change.
