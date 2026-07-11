# Phase 4 — Per-range grade, channel mixer, split-toning

**Status:** COMPLETED

Part of `plans/post-processing/` (scene-linear color grade folded into the tonemap pass). This is the
fourth phase; it builds directly on the Phase 3 grade stage. Phase 3 landed the *global* grade —
exposure, white-balance CAT, contrast-around-pivot, saturation, and one global ASC-CDL (SOP, surfaced
as Lift/Gamma/Gain) — driven by a per-view **grade uniform buffer** bound to the tonemap descriptor
set. This phase adds the three tonal ranges every colorist reaches for: **Shadows / Midtones /
Highlights** each running the CDL+Sat+Contrast trio behind a smooth luma mask (UE `ShadowsMax` /
`HighlightsMin` model), a **3×3 channel mixer**, and **split-toning** (a shadow tint + a highlight tint
blended by luma with a balance bias). It changes no descriptors and adds no command — it grows the one
grade UBO and extends the one `set-color-grading` DTO. It does not touch the view transform or the
creative LUT (Phase 5).

## Why the UBO was chosen in Phase 3 pays off here

Per-range grading is exactly why Phase 3 put the grade on a uniform buffer instead of the tonemap push.
Three CDL blocks (9 floats each) + a 3×3 mixer + two split tints + the range knobs blow well past the
128-byte push-constant floor, so a push-based grade would have forced a descriptor-layout rewrite this
phase. Because Phase 3 already bound a `GradeUniforms` UBO to the tonemap set
(`engine/crates/rendering/src/view_target.rs`, `tonemap_set`; `descriptors.rs`, `create_tonemap_layout`),
this phase **grows the struct in place** — no new binding, no `view_target.rs` set-allocation change, no
push-size assert to chase. That is the load-bearing structural decision this phase leans on.

## Goal

- **Three masked correction ranges inside the same grade.** `grade()` (in the shared `tonemap_ops`
  module) applies the global ops (Phase 3), then blends a Shadows, a Midtones, and a Highlights
  CDL+Sat+Contrast trio by smooth luma weights derived from `ShadowsMax` (~0.09) and `HighlightsMin`
  (~0.5), matching UE's Shadows/Midtones/Highlights semantics.
- **A 3×3 channel mixer.** One row-major matrix applied after the ranges (`out = mixer · rgb`), identity
  by default.
- **Split-toning.** A shadow tint + a highlight tint blended by luma, biased by a balance knob, applied
  last before `tonemapAndEncode`.
- **One grade UBO, one command, one persisted block.** The new state extends the existing
  `GradeUniforms` struct, the existing `SetColorGradingParams` DTO, and the existing `colorGrading`
  persistence block — **no** `set-tonal-ranges` / second grade buffer / parallel settings block is added.

## NO-LEGACY checklist for this phase

- No new command. The per-range/mixer/split state rides the existing `set-color-grading` (`command.rs`
  `COMMANDS`/`COMMAND_FIXTURES`/`DTO_TYPE_NAMES`/`render_domain()` are **unchanged** — the params/result
  type names are the same, only their fields grow). A `set-tonal-ranges` command duplicating the grade
  surface is forbidden.
- No second grade buffer. `GradeUniforms` (`renderer.rs`) is extended in place; the tonemap descriptor
  set layout and the per-view `tonemap_set` allocation do not change.
- No re-implemented CDL/sat/contrast. The per-range trio calls the Phase 3 leaf helpers
  (`applyCdl`, `applySaturation`, `applyContrast`) in `tonemap_ops.slang`; it does not copy their math.
  The global CDL stays the global block — the ranges are additive masked corrections, not a fork of it.
- No second settings block. The `colorGrading` block in `render_settings.rs` gains keys; the three
  frozen-key tests are updated to include them, not a parallel block.

## 1 — Grade module: masks, per-range trio, mixer, split-tone

**File `engine/assets/shaders/tonemap_ops.slang`.**

`tonemap_ops` is the resource-free module (excluded from entry-point compile via `TONEMAP_OPS_STEM` in
`engine/xtask/src/shaders.rs`) imported by `tonemap.slang` **and** the thumbnail/preview shaders, so the
grade encodes identically everywhere — this phase adds only pure functions there, **no xtask change**.
Phase 3 added `grade(GradeParams g, float3 c)` calling `applyContrast`/`applySaturation`/`applyCdl`
leaves; extend the `GradeParams` mirror with the new fields and insert the three ops between the global
CDL and the return.

Range weights follow the UE model — shadows fall to zero by `ShadowsMax`, highlights rise from
`HighlightsMin`, midtones are the remainder:

```hlsl
float3 rangeWeights(float l, float shadowsMax, float highlightsMin) {
    float ws = 1.0 - smoothstep(0.0, shadowsMax, l);
    float wh = smoothstep(highlightsMin, 1.0, l);
    return float3(ws, saturate(1.0 - ws - wh), wh); // shadows, midtones, highlights
}

float3 gradeTrio(float3 c, GradeBlock b) {          // reuses the Phase 3 leaves
    c = applyCdl(c, b.slope, b.offset, b.power);
    c = applySaturation(c, b.sat);
    c = applyContrast(c, b.contrast, GRADE_PIVOT);
    return c;
}

float3 splitTone(float3 c, float3 shadowTint, float3 highlightTint, float balance) {
    float t = saturate(lumaRec709(c) + balance);    // balance biases the neutral pivot
    return c * (lerp(shadowTint, highlightTint, t) * 2.0); // 0.5-neutral tints
}
```

The tail of `grade()` after the global ops:

```hlsl
float3 w = rangeWeights(lumaRec709(c), g.shadowsMax, g.highlightsMin);
c = lerp(c, gradeTrio(c, g.shadows),    w.x);
c = lerp(c, gradeTrio(c, g.midtones),   w.y);
c = lerp(c, gradeTrio(c, g.highlights), w.z);
c = mul(g.channelMixer, c);                          // 3x3 row-major
c = splitTone(c, g.splitShadow, g.splitHighlight, g.splitBalance);
```

- `lumaRec709` and `GRADE_PIVOT` already exist from Phase 3 (saturation + contrast-around-pivot use
  them); reuse them, do not add a second luma constant.
- `tonemap.slang` `computeMain` is unchanged — it still calls `grade(...)` then `tonemapAndEncode(...)`;
  the new ops live entirely inside `grade()`.

> Decision to record when building: the ranges are **weighted lerps toward the trio result**, not a
> hard per-pixel range switch — `smoothstep` masks with a small overlap so a pixel near `ShadowsMax`
> receives a blend of the shadow and midtone corrections. One coherent smooth model (UE), never a hard
> `if luma < ShadowsMax` branch that bands on gradients.

## 2 — Grow the grade UBO layout + upload

**File `engine/crates/rendering/src/renderer.rs`.**

Extend the Phase 3 `GradeUniforms` `#[repr(C)]` Pod with a std140-padded per-range block and the new
scalars/matrix. Each range packs its saturation and contrast into the CDL triplet's `.w` lanes so the
block stays 16-byte aligned:

```rust
/// One masked correction range: ASC-CDL SOP + saturation + contrast (std140-padded).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradeBlock {
    slope: [f32; 4],  // xyz slope, w saturation
    offset: [f32; 4], // xyz offset, w contrast
    power: [f32; 4],  // xyz power,  w pad
}
```

Appended to `GradeUniforms` (after the Phase 3 global fields):

```rust
shadows: GradeBlock,
midtones: GradeBlock,
highlights: GradeBlock,
channel_mixer: [[f32; 4]; 3], // 3 row-major rows, each vec4-padded
split_shadow: [f32; 4],       // xyz tint, w unused
split_highlight: [f32; 4],    // xyz tint, w unused
range_knobs: [f32; 4],        // x shadowsMax, y highlightsMin, z split balance
```

- Add matching renderer state fields next to the Phase 3 grade fields (a `ColorGrading` struct is
  cleanest — extend it rather than scattering scalars) with `pub` setters/getters mirroring
  `set_exposure`/`exposure_ev`; the per-frame writer that fills `GradeUniforms` and uploads it to the
  per-view UBO gains the new lanes. Defaults are identity: slope `1,1,1`, offset `0,0,0`, power `1,1,1`,
  sat/contrast `1.0`, mixer identity, split tints `0.5,0.5,0.5`, balance `0.0`, `shadowsMax 0.09`,
  `highlightsMin 0.5`.
- The tonemap descriptor set, `create_tonemap_layout`, and the `view_target.rs` `tonemap_set`
  allocation are **untouched** — only the bytes written to the existing UBO grow (confirm the UBO
  allocation is sized from `size_of::<GradeUniforms>()` so it tracks automatically).
- Mirror the new fields into `RenderStatsFull` where Phase 3 mirrored the global grade, so the panel
  reads live state from `render-stats`.

## 3 — Protocol: extend `SetColorGradingParams` + readback

**File `engine/crates/protocol/src/dto.rs`.**

Add two nested DTO structs and extend the params/result + `RenderStatsDto`. Nested DTOs carry the full
derive stack and `#[ts(export)]`; params-facing structs also derive `Default`.

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GradeRangeDto {
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub saturation: f32,
    pub contrast: f32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SplitToneDto {
    pub shadow: [f32; 3],
    pub highlight: [f32; 3],
    pub balance: f32,
}
```

New `Option` fields on `SetColorGradingParams` (partial-patch, matching the Phase 3 convention so a
single-field editor write does not reset the rest):

```rust
pub shadows: Option<GradeRangeDto>,
pub midtones: Option<GradeRangeDto>,
pub highlights: Option<GradeRangeDto>,
pub shadows_max: Option<f32>,
pub highlights_min: Option<f32>,
pub channel_mixer: Option<[f32; 9]>, // row-major
pub split_tone: Option<SplitToneDto>,
```

- Add the same fields (non-`Option`, current live values) to `SetColorGradingResult` **and**
  `RenderStatsDto` so the panel reflects state.
- **`codegen.rs`:** add `decl_entry!(GradeRangeDto)`, `decl_entry!(SplitToneDto)` to `ts_decls()` and
  `frag_entry!(...)` for both to `fragment_decls()`. **`tests/inventory.rs`:** add both to the
  `inventory!` list; **`tests/schema_fragments.rs`:** add a `check!` line for each.
- **`command.rs` is unchanged.** `set-color-grading` already occupies its `COMMANDS` row, its
  `COMMAND_FIXTURES` tuple (`"set-color-grading" → "color-grading"`), its `DTO_TYPE_NAMES`
  (`SetColorGradingParams`/`SetColorGradingResult`), and `render_domain()`. The frozen wire order does
  not move.
- **`tools/check-control-schema/check.ts` is unchanged** — the `"color-grading"` `paramsForFixture`
  case from Phase 3 stays valid because every new field is optional.

## 4 — Control: apply the new fields; surface them in `render-stats`

**File `engine/crates/control/src/commands_render.rs`.** The `set-color-grading` handler applies each
present `Option` field through the `ControlRenderer` seam and echoes the live values into
`SetColorGradingResult`; `render_stats_dto()` populates the new `RenderStatsDto` fields from the
renderer getters.

**File `engine/crates/control/src/registry.rs`.** The `ControlRenderer` grade pair from Phase 3
(`set_color_grading(&mut self, params: &SetColorGradingParams)` and the readback getter feeding
`render-stats`) already carries the extensible DTOs, so this phase is a **field-only** change: the
setter now also copies the per-range/mixer/split `Some(_)` fields into renderer state, and the readback
DTO gains the mirror fields. No new trait method.

- **`engine/crates/host/src/control_renderer.rs`:** the real impl copies the new fields onto
  `saffron_rendering::Renderer` via its setters and reads them back for the DTO.
- **`engine/crates/control/src/test_support.rs`** and the second impl in
  **`engine/crates/control/src/commands_asset.rs`** gain the same field handling (store-and-echo in the
  stub) so both `ControlRenderer` impls compile.

## 5 — Persist the per-range block

**File `engine/crates/rendering/src/render_settings.rs`.** Extend the existing `colorGrading` block in
`RenderSettings` + `settings_to_json` + `parse_render_settings` with the new keys
(`shadows`/`midtones`/`highlights` each `{ slope, offset, power, saturation, contrast }`, plus
`shadowsMax`, `highlightsMin`, `channelMixer`, `splitTone { shadow, highlight, balance }`); add the
`Some(_)` reads to `render_settings_to_json` and the apply arms to `apply_render_settings`. Update the
three frozen-key unit tests (`block_has_the_frozen_keys`, `parse_then_serialize_round_trips`,
`missing_and_malformed`) to include the new keys — one persisted `colorGrading` block, no parallel
block.

## 6 — RenderPanel: per-range sections, channel mixer, split-tone

**File `editor/src/panels/RenderPanel.tsx`.** These rows land in `RenderPanel` alongside the Phase 3
grade rows (Phase 6 migrates all of it to the dedicated Post panel with color wheels — NO-LEGACY cutover
there). Section with the existing `Separator` + uppercase muted `Label` pattern (the "Debug" divider);
no Tabs primitive exists yet.

- **SHADOWS / MIDTONES / HIGHLIGHTS** sections — each a `VectorEditor` for `slope`, `offset`, `power`
  (RGB triplets; react-colorful is a square not a wheel, so triplets are the correct widget until the
  Phase 6 trackball) plus `NumberDrag` for `saturation` and `contrast`. Above them, two `SliderField`
  rows for `shadowsMax` (0..1) and `highlightsMin` (0..1).
- **CHANNEL MIXER** — a `Select` output selector (Red/Green/Blue, local UI state) plus three `NumberDrag`
  rows (min −2, max 2) editing the selected output row's three coefficients of the `channelMixer[9]`.
- **SPLIT TONE** — two `ColorField`s (shadow tint, highlight tint) and a `NumberDrag` `balance` (−1..1).
- Every edit funnels through `makeCoalescer` for scrub streams and records one `pushEdit(..., "scene")`
  inverse per gesture (capture the prior value at `onDragStart`, record at `onDragEnd`), exactly like the
  Phase 3 grade rows; the optimistic fold reads the echoed `SetColorGradingResult`, and the reconcile
  poll refreshes from `RenderStatsDto`. Labels via `humanizeFieldName`; semantic Tailwind tokens only.

**File `editor/src/control/client.ts`.** `setColorGrading` already forwards a `SetColorGradingParams`
object; after `bun run check` regenerates the union, its typed param widens to accept the new optional
fields — pass them through, no structural change.

## Known interactions (note, don't over-engineer)

- **`sa` CLI + nested blocks.** The top-level scalar knobs fold as plain flags
  (`sa set-color-grading --shadows-max 0.09 --highlights-min 0.5`); the per-range CDL blocks and
  split-tone travel as JSON object flag values forwarded by the generic client (no per-command `sa`
  code). The editor and the `tests/e2e` driver — where JSON objects are native — are the primary drivers
  for the object fields; the CLI scalars are enough to eyeball a range knob from a shell.
- **Range masks read pre-tonemap scene-linear luma.** Weights come from unbounded scene radiance, so a
  very bright highlight sits firmly in the highlight mask before the view transform rolls it off — this
  is the correct scene-referred behavior and matches UE. No action.

## Out of scope (later phases)

Color wheels / trackballs and the tone-curve widget are Phase 6 (this phase uses `VectorEditor`
triplets). The creative post-tonemap `.cube` LUT and the frozen-grade bake are Phase 5. ACEScg working
space stays a Future note (the operator input matrices in `tonemap_ops.slang` are Rec.709-derived).

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning
this change raises. Boot an emissive/high-contrast scene headless (`just run-engine-headless`) with a
validation-clean log and confirm an independent **shadow tint** and **highlight tint** (split-tone) plus
a highlights-only CDL shift are visible and thumbnails still match the viewport. `bun run check` in
`editor/` regenerates `@saffron/protocol` (`GradeRangeDto`/`SplitToneDto` exported; `SetColorGradingParams`
gains the optional fields — never hand-edit `sa-types.ts`); `just e2e` with an added case asserting a
`set-color-grading` write of a full `highlights` block + `channelMixer` + `splitTone` **round-trips**
through `render-stats`. Update the `docs/content/` post-processing `color-grading` page (tonal ranges /
`ShadowsMax`·`HighlightsMin`, the channel mixer, split-toning) and its hub `_index.md` row in the same
change.
