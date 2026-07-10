# Phase 3 — Color grade core, folded into the tonemap pass

**Status:** NOT STARTED

Part of `plans/post-processing/` (an energy-conserving bloom pyramid + a scene-referred color grade on the display-extent `color` target). This is the grade-core phase; like the bloom core it **stands alone and ships independently** — it folds into the tonemap compute pass that already exists, so it needs none of the bloom passes. It adds a scene-linear grade (white balance, contrast-around-pivot, saturation, global ASC-CDL) that runs immediately before `tonemapAndEncode`, driven by a **per-view grade uniform buffer** bound to the tonemap descriptor set, and relabels the tonemap operator selector as a **view/display transform**. Phases 4 (per-range CDL, channel mixer, split-tone) and 5 (creative `.cube` LUT) extend the same stage and the same UBO; nothing here blocks them, and the UBO transport is chosen up front precisely so Phase 4 needs no push-size rewrite.

## Goal

The tonemap compute pass gains a scene-linear grade stage in front of the display transform. In `tonemap.slang` the per-pixel body becomes: exposure (the existing `TonemapPush::exposure` multiply, kept as grade op #1) → `grade(c, params)` → `tonemapAndEncode(c, mode)` → store. The `grade()` helper is a **pure, resource-free function** in the shared `tonemap_ops.slang` module (the same module thumbnails and previews import, so their encode stays bit-identical), applying, in ASC-CDL order:

1. **white balance** — a Bradford chromatic-adaptation `float3x3` (computed CPU-side from Temp/Tint), a matrix multiply in the working primaries;
2. **contrast** — a gain around the 0.18 middle-grey pivot in the `log2` domain;
3. **saturation** — a lerp toward Rec.709 luma;
4. **global ASC-CDL SOP** — `out = (slope·x + offset)^power`.

The grade parameters ride a **per-view grade uniform buffer** on the tonemap descriptor set, written each frame from renderer state. Every knob is one control setting: a single `set-color-grading` command, read back through `RenderStatsDto`, persisted as the canonical ASC-CDL SOP+Sat block in the project `renderSettings`, and scriptable from `sa`. The RenderPanel gains grade rows (Temp/Tint/Contrast/Saturation `NumberDrag`, a `VectorEditor` triplet or three sliders for CDL surfaced as Lift/Gamma/Gain), and the operator `Select` is relabeled "View transform". The neutral default is a mathematical identity, so an ungraded project renders **bit-identical to today**.

## NO-LEGACY checklist for this phase

- The grade is folded into the **one** existing tonemap compute pass (`add_tonemap_pass`, `renderer.rs`) — there is **no** `add_grade_pass` and no second image write. `grade()` lives in `tonemap_ops.slang` next to `tonemapAndEncode`, not in a parallel grade module (a resource-free pure function needs no new `xtask` exclusion entry).
- Exposure stays grade op #1 as the existing `TonemapPush::exposure = exp2(ev)` multiply and the existing `set-exposure` command — it is **not** copied into the grade UBO. One exposure source, one command.
- Grade rides a **per-view UBO from the start**, never a push. `TonemapPush` stays its current 8 bytes; Phase 4's per-range CDL extends the *same* UBO layout — there is no push→UBO rewrite later, and no second transport.
- The operator selector stays the one `TonemapMode` wire pair (`as_str`/`from_name`, `overlay.rs`); only its editor label changes to "View transform". No parallel "look mode" enum is introduced.
- `SetColorGradingParams` is **the** grade command. ASC-CDL slope/offset/power + saturation is the single canonical param and persist form (round-trips as `.cdl`/`.ccc`). Lift/Gamma/Gain is an **editor-side reparametrization** of those same values on the way into the widgets, not a second wire field.

## 1 — the `grade()` helper in `tonemap_ops.slang`

**File `engine/assets/shaders/tonemap_ops.slang`.**

Add a pure `GradeParams` struct and a `grade()` function to the resource-free module (`module tonemap_ops`) that `tonemap.slang` and the thumbnail/preview shaders already import. Because it is a plain function over a value struct — no bindings, no entry point — it needs **no** change to the `xtask` module-exclusion list (`shaders.rs`, `TONEMAP_OPS_STEM`); only a *new* helper file with no entry points would.

```hlsl
struct GradeParams {
    float3x3 whiteBalance;   // Bradford CAT, precomputed CPU-side from Temp/Tint
    float3   slope;          // ASC-CDL S
    float3   offset;         // ASC-CDL O
    float3   power;          // ASC-CDL P
    float    contrast;       // gain around the log2 pivot
    float    pivot;          // 0.18 middle grey
    float    saturation;     // around Rec.709 luma
};

static const float3 kRec709Luma = float3(0.2126, 0.7152, 0.0722);

/// Scene-linear grade, applied AFTER exposure and BEFORE the view transform.
float3 grade(float3 c, GradeParams g) {
    c = mul(g.whiteBalance, c);                                   // white balance
    float3 lg = log2(max(c, 1e-5));                               // contrast in log2
    lg = (lg - log2(g.pivot)) * g.contrast + log2(g.pivot);
    c = exp2(lg);
    float luma = dot(c, kRec709Luma);                            // saturation
    c = lerp(float3(luma, luma, luma), c, g.saturation);
    c = pow(max(g.slope * c + g.offset, 0.0), g.power);           // ASC-CDL SOP
    return c;
}
```

- The helper is pure over `GradeParams`; the caller unpacks the UBO into a `GradeParams` value and passes it in, so `tonemap_ops.slang` stays binding-free and thumbnails/previews that never bind the grade UBO are unaffected.
- White balance is a **matrix multiply**, not a per-channel scale — the shader does not know about Temp/Tint. The Bradford CAT `float3x3` is built on the CPU (§3) so the shader cost is one `mul`.
- Contrast runs in `log2` around the 0.18 pivot for perceptual evenness (matching Unreal's scene-referred order); saturation is a Rec.709 luma lerp; CDL is the canonical `(S·x+O)^P` with a `max(...,0)` guard before the `pow`.

> Decision to record when building: **white balance transport.** Temp/Tint is turned into a full Bradford chromatic-adaptation `float3x3` on the CPU (Planckian-locus target white → Bradford cone response → adapt from the working-space D65 white → back to Rec.709), uploaded in the UBO — the modern, correct form. The cheap per-channel RGB multiply is the labeled fallback and is not built; there is one white-balance path.

## 2 — `tonemap.slang` binds the grade UBO and calls `grade()`

**File `engine/assets/shaders/tonemap.slang`.**

The tonemap compute entry (`computeMain`) already reads the storage image at `binding(0,0)` (GENERAL, `RWTexture2D` rgba16f) and applies `TonemapPush { exposure, mode }`. Add the grade UBO at `binding(1,0)` and thread the grade between exposure and encode:

```hlsl
[[vk::binding(1, 0)]] ConstantBuffer<GradeParams> gradeParams;

// inside computeMain, per pixel:
float3 c = color.rgb * push.exposure;          // 1 — exposure (existing push)
c = grade(c, gradeParams);                      // 2..4 — scene-linear grade
float3 mapped = tonemapAndEncode(c, push.mode); // view/display transform
target[px] = float4(mapped, color.a);
```

- Exposure remains the existing `push.exposure` multiply (grade op #1); the grade covers white balance → CDL; `tonemapAndEncode` is unchanged and is now explicitly the **view transform**.
- `push.mode` and `TonemapPush` are untouched (still 8 bytes) — grade state does not ride the push.

## 3 — the grade uniform: renderer state, descriptor, per-frame upload

**File `engine/crates/rendering/src/overlay.rs`.**

Add the CPU-side grade value type and its `#[repr(C)]` `std140` image alongside `TonemapPush` (`TonemapPush` stays as-is). Keep the `bytemuck` size assert on the uniform.

```rust
/// Scene-linear grade state (canonical ASC-CDL SOP+Sat + white balance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGrade {
    pub temperature: f32, // Kelvin; 6500 = neutral
    pub tint: f32,        // green(-)/magenta(+)
    pub contrast: f32,    // 1.0 neutral
    pub pivot: f32,       // 0.18
    pub saturation: f32,  // 1.0 neutral
    pub slope: [f32; 3],  // ASC-CDL S
    pub offset: [f32; 3], // ASC-CDL O
    pub power: [f32; 3],  // ASC-CDL P
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self {
            temperature: 6500.0, tint: 0.0, contrast: 1.0, pivot: 0.18, saturation: 1.0,
            slope: [1.0; 3], offset: [0.0; 3], power: [1.0; 3],
        }
    }
}

/// std140 image of `GradeParams` for the tonemap set. mat3 columns are vec4-padded.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GradeUniform {
    white_balance: [[f32; 4]; 3],
    slope: [f32; 4],
    offset: [f32; 4],
    power: [f32; 4],
    contrast: f32,
    pivot: f32,
    saturation: f32,
    _pad: f32,
}
```

- `GradeUniform::from(&ColorGrade)` computes the Bradford white-balance `Mat3` (glam) from Temp/Tint and lays out the std140 image (mat3 as three vec4 columns; slope/offset/power as `xyz + pad`). `ColorGrade::default()` is a **hand-written neutral identity** — deriving `Default` would give `pivot = 0`, `power = 0` (a black frame), so the identity is written by hand and is the passthrough that keeps an ungraded project bit-identical.

**File `engine/crates/rendering/src/descriptors.rs`.** Extend `create_tonemap_layout` (today a single `STORAGE_IMAGE` at binding 0) with a `UNIFORM_BUFFER_DYNAMIC` at binding 1 (via `compute_binding`), and bump the uniform-buffer term in the pool budget in `create_descriptor_pool` to cover one grade UBO per view.

**File `engine/crates/rendering/src/view_target.rs`.** The tonemap descriptor set (`tonemap_set`, allocated near l.464, written at l.1206) is per-view — add a per-view grade buffer next to it:

- Add `grade_ubo: Arc<Buffer>` to `ViewTarget`, sized `frames_in_flight * align_up(size_of::<GradeUniform>(), min_uniform_buffer_offset_alignment)`, `HOST_VISIBLE | HOST_COHERENT`, persistently mapped, allocated in `ViewTarget::new` and re-created on a `generation` bump.
- In the tonemap-set write, add a `Binding::uniform(set, binding, buffer, range)` variant (mirroring `Binding::sampled` l.1437 / `Binding::storage` l.1451) that binds binding 1 as `UNIFORM_BUFFER_DYNAMIC` over one `align_up(size_of::<GradeUniform>())` range. One set serves every frame-in-flight; the per-frame slice is selected by a dynamic offset at record time — no per-frame descriptor rewrite, no CPU/GPU race.

> Decision to record when building: **grade transport is a `UNIFORM_BUFFER_DYNAMIC`, not a per-frame set or a push.** A dynamic-offset UBO gives one persistent tonemap set across all frames-in-flight (the dispatch supplies `frame_index * aligned_size`), avoiding both descriptor-update-in-flight hazards and a Phase-4 push-size rewrite. Confirm the tonemap set takes the extra binding cleanly against its existing set-0/binding-0 storage-image layout (open question in the plan README) — it does: binding 1 is additive.

**File `engine/crates/rendering/src/renderer.rs`.**

- Add a `color_grade: ColorGrade` field (init `ColorGrade::default()`, near `exposure_ev` l.481 / `tonemap_mode` l.596) with `set_color_grading(&mut self, g: ColorGrade)` / `color_grading(&self) -> ColorGrade` (near `set_exposure` l.3733 / `set_tonemap_mode` l.1377).
- In `Renderer::render`, **before** `add_tonemap_pass` (l.5784), build `GradeUniform::from(&self.color_grade)` and write it into the current frame's slice of the view's `grade_ubo`.
- Extend `add_tonemap_pass` (l.7684) to bind the tonemap set with the frame's dynamic offset for binding 1 (`TonemapPush::new(self.exposure_ev, self.tonemap_mode)` at l.7696 is unchanged).
- Mirror the grade fields into `RenderStatsFull` (near `exposure_ev` l.241, populated l.3865) so the panel can read live state.

## 4 — protocol: `SetColorGradingParams`/`Result` + `RenderStatsDto`

**File `engine/crates/protocol/src/dto.rs`.**

Add the wire DTO pair with the full derive stack (`Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS`; `#[serde(rename_all = "camelCase")]`; `#[ts(export)]`; params also `#[serde(default)]` over a hand-written neutral `Default`), mirroring `SetExposureParams` (l.3187) / `SetTonemapParams` (l.1222). Params are **flat** so `sa` positional/named folding works.

```rust
#[serde(rename_all = "camelCase")]
pub struct SetColorGradingParams {
    pub temperature: f32, // Kelvin, 6500 neutral
    pub tint: f32,
    pub contrast: f32,
    pub pivot: f32,       // 0.18
    pub saturation: f32,
    pub slope: [f32; 3],  // ASC-CDL S (canonical)
    pub offset: [f32; 3], // ASC-CDL O
    pub power: [f32; 3],  // ASC-CDL P
}
// SetColorGradingResult mirrors the applied values (same flat fields).
```

- `SetColorGradingParams::default()` is the neutral identity (temperature 6500, contrast/saturation 1, pivot 0.18, slope/power 1, tint/offset 0), so `#[serde(default)]` means a partial `sa` call leaves unspecified fields neutral.
- Add read-back to `RenderStatsDto` (near `tonemap` l.407): a `color_grading: SetColorGradingParams` field. The panel reads live grade state from `render-stats` (not the set echo), and reusing the flat params shape means the panel reads exactly the shape it writes.

**File `engine/crates/protocol/src/command.rs`.** Four edits, mirroring `set-tonemap`/`set-exposure`, placed in the render-domain region so the frozen wire order is set:

- `COMMANDS` — a `CommandSpec { name: "set-color-grading", summary, params: "SetColorGradingParams", result: "SetColorGradingResult" }` row.
- `COMMAND_FIXTURES` — `("set-color-grading", "color-grading")`.
- `DTO_TYPE_NAMES` — `SetColorGradingParams`, `SetColorGradingResult`.
- `render_domain()` (`#[cfg(test)]`, l.1826) — add `"set-color-grading"`.

**File `engine/crates/protocol/src/codegen.rs`.** Add `decl_entry!(SetColorGradingParams)` + `decl_entry!(SetColorGradingResult)` to `ts_decls()` and `frag_entry!(...)` for both to `fragment_decls()`. **Files `tests/inventory.rs` / `tests/schema_fragments.rs`.** Add both DTOs to the `inventory!` list and a `check!(TypeName, "TypeName")` line per struct; `schema.rs` gains the positional-field-order expectation for `SetColorGradingParams`.

## 5 — control: `set-color-grading` + the `ControlRenderer` seam

**File `engine/crates/control/src/registry.rs`.** Add to the `ControlRenderer` trait (the control↔rendering boundary), grouped rather than nine scalar methods:

```rust
fn set_color_grading(&mut self, params: SetColorGradingParams);
fn color_grading(&self) -> SetColorGradingParams;
```

The control crate already depends on `saffron-protocol`, so the trait can carry the wire DTO directly; the rendering crate stays protocol-free and the host does the one DTO↔`ColorGrade` mapping.

**File `engine/crates/control/src/commands_render.rs`.** Register the handler in `register_render_commands` next to `set-tonemap` (l.800) / `set-exposure` (l.913):

```rust
reg.register::<SetColorGradingParams, SetColorGradingResult>(
    "set-color-grading",
    "Set the scene-linear color grade (white balance, contrast, saturation, ASC-CDL)",
    |ctx, params| {
        // validate: pivot > 0, contrast/saturation finite and >= 0, power components > 0
        ctx.renderer.set_color_grading(params);
        Ok(result_from(ctx.renderer.color_grading()))
    },
)?;
```

Validate the numeric ranges and return `Err(Error::command(..))` on a non-finite/out-of-range value (mirroring `set-tonemap`'s bad-enum rejection). Populate the new `RenderStatsDto.color_grading` field in `render_stats_dto()` (l.191) from `ctx.renderer.color_grading()`.

**File `engine/crates/host/src/control_renderer.rs`.** Implement the two trait methods, mapping `SetColorGradingParams` ↔ `saffron_rendering::ColorGrade` field-for-field (the one mapping site), delegating to `Renderer::set_color_grading` / `color_grading`. Add matching stubs to the test double in `engine/crates/control/src/test_support.rs` (l.262/430) and the second `ControlRenderer` impl in `commands_asset.rs` (l.790/794).

`sa` needs **no** per-command code — it forwards any name in `COMMANDS`, folding provided keys onto the DTO with the rest at their neutral serde default. `sa set-color-grading temperature 5000 contrast 1.2` sets those two and leaves the others neutral.

## 6 — persistence (`render_settings.rs`)

**File `engine/crates/rendering/src/render_settings.rs`.**

Persist the canonical ASC-CDL SOP+Sat block in the project `renderSettings` so grades survive save/load and round-trip as `.cdl`/`.ccc`:

- Add `color_grading: Option<ColorGradingSettings>` to `RenderSettings` (l.16-45) — `{ temperature, tint, contrast, pivot, saturation, slope, offset, power }`.
- Add the `colorGrading` key to `settings_to_json` (l.50) and `parse_render_settings` (l.71).
- Emit it in `Renderer::render_settings_to_json` (l.105) from `self.color_grading()` and apply it in `apply_render_settings` (l.127) via `set_color_grading` (next to the `set_exposure` l.135 / `set_tonemap_mode` l.157 arms).
- Extend the three frozen-key unit tests (`block_has_the_frozen_keys` l.188, `parse_then_serialize_round_trips` l.233, `missing_and_malformed` l.272) with the new key.

## 7 — editor: client wrapper + RenderPanel grade rows + "View transform" label

**File `tools/check-control-schema/check.ts`.** Add a `paramsForFixture()` case `"color-grading"` returning a real params object (e.g. `{ temperature: 5000, tint: 0, contrast: 1.2, pivot: 0.18, saturation: 1, slope: [1,1,1], offset: [0,0,0], power: [1,1,1] }`), then run `cargo run -p xtask -- gen-protocol` (regenerates `editor/src/protocol/sa-types.ts` + `schemas/control/*`) and re-export the new type names from `editor/src/protocol/index.ts`.

**File `editor/src/control/client.ts`.** Add the hand-authored typed wrapper next to `setExposure` (l.848) / `setTonemap` (l.812):

```ts
setColorGrading: (p: SetColorGradingParams) => call("set-color-grading", p),
```

**File `editor/src/panels/RenderPanel.tsx`.** Add a grade section using the existing `Separator` + uppercase-`Label` pattern (the "Debug" divider), reading `renderStats.colorGrading` and writing via `client.setColorGrading` with the optimistic-fold + per-gesture undo already used for exposure:

- Rows: Temperature, Tint, Contrast, Saturation as `NumberDrag`; the ASC-CDL triplet as a `VectorEditor` per S/O/P **surfaced as Lift/Gamma/Gain** (Gain↔Slope, Lift↔Offset, Gamma↔Power — a display-only reparametrization; the wire stays CDL SOP), or three `SliderField`s.
- Funnel scrub streams through `makeCoalescer` (like `EnvironmentPanel`) so a drag is one preview per burst; capture the prior grade at `onDragStart` and record a single `pushEdit(inverse, 'scene')` at `onDragEnd` (mirroring `onExposureDragStart`/`onExposureDragEnd`). Label fields via `humanizeFieldName`; semantic Tailwind tokens only.
- Rename the tonemap `Select` label (`TONEMAP_OPTIONS` group) from the operator wording to **"View transform"** — the one label change that reframes the operator as a display transform.

## Known interactions (note, don't over-engineer)

- **Bloom ordering (Phases 1–2).** Bloom composites into `color` before the tonemap pass and reads pre-exposure radiance; exposure remaining grade op #1 inside the pass is deliberate and documented on the README. This phase does not change that — the grade runs after exposure, after any bloom composite, still scene-linear.
- **Thumbnails/previews.** They import `tonemap_ops.slang` but never bind the grade UBO, so adding `grade()` to the module does not touch their output (they don't call it, or call it with the neutral identity). "Thumbnails match" is verified as: with a neutral grade the viewport and every shared-encode surface are bit-identical to before.

## Out of scope (later phases)

Per-range Shadows/Midtones/Highlights CDL, the 3×3 channel mixer, and split-toning are **Phase 4** — they extend this same `grade()` stage and the same grade UBO (which is why the UBO transport is chosen now). The creative `.cube` 3D-LUT applied post-tonemap on `[0,1]` display values, plus the bake path, are **Phase 5**. The dedicated Post panel with color-wheel/tone-curve widgets is **Phase 6**; until then grade rows live in `RenderPanel`.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot an emissive/lit fixture headless (`just run-engine-headless`) with a validation-clean log and confirm a warmer, more contrasty **graded** frame (`sa set-color-grading temperature 5000 contrast 1.2`) while a neutral grade renders bit-identical to today and thumbnails are unchanged. Because a wire type changed, run `bun run check` in `editor/` (regenerates `@saffron/protocol` via `xtask gen-protocol` — never hand-edit `sa-types.ts`; `CommandName` must gain `set-color-grading`), and add/extend a `tests/e2e` case driving `set-color-grading` over the control plane and asserting the echoed + `render-stats` grade values, then `just e2e`. Update the `docs/content/` post-processing **Color grading** page (the scene-referred-grade-then-view-transform split, the ASC-CDL SOP+Sat order, the operator-as-view-transform reframing, `.cdl`/`.ccc` interchange) and its hub `_index.md` row in the same change.
