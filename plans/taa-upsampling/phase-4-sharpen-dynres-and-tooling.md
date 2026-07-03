# Phase 4 — Sharpen, dynamic resolution, and tooling

**Status:** COMPLETED

Part of the `plans/taa-upsampling/` feature (temporal upsampling / TAAU). The finishing phase: the
RCAS contrast-adaptive sharpen — already folded into the resolve by `plans/modern-taa-core/`
phase 4 — is made **upscale-aware** and runs at **display extent** after the reconstruction; the
existing `BudgetStep::Scale` → `pending_render_scale` frame-budget hook is wired to drive the
**input** extent toward a frame-time budget (dynamic resolution); a single `get-upscale` /
`set-upscale` control-command pair exposes the ratio + dynamic-resolution surface over every protocol
seam (per the keep-current `sa` rule); a light editor control drives it; and the `docs/` TAA /
render-quality pages gain the TAAU + dynamic-resolution story (per the keep-docs-current rule).
Depends on `phase-3-robust-reconstruction.md`, which left the resolve reconstructing a robust
display-extent image over input-extent inputs; this phase sharpens, drives, and exposes it.

## Goal

- **One sharpen, upscale-aware.** The `plans/modern-taa-core/` phase-4 RCAS branch in
  `computeMain` (keyed on `TaaParams::sharpness > 0`) is *the* sharpen. This phase spends it at the
  **display** grid the reconstruction now writes, taps the **display-grid** `+` neighbours of the
  reconstructed output, and modulates the lobe by the Phase-2 upscale ratio so more upscaling recovers
  a touch more of the reconstruction softness — no second sharpen pass, no new sharpness knob.
- **Dynamic resolution on the one driver.** The `BudgetController` → `BudgetStep::Scale` →
  `pending_render_scale` hook that already exists in `Renderer` now drives the **input** extent
  (`scaled_render_extent()`), while the display-extent history / resolve output / tonemap / overlays
  hold constant. Measured frame time vs the budget adjusts `render_scale`; `apply_render_extent`
  rebuilds only the input-extent targets; the resolve reconstructs to the fixed display extent. A
  scale change must **not** hard-reset the display-extent history every frame.
- **One upscale-control command.** `get-upscale` / `set-upscale` (fixed `ratio` + dynamic-resolution
  `dynamic` toggle + `targetMs` budget) is the single wire surface for the upscale / dynamic-resolution
  state, threaded through every protocol seam and regenerated with `xtask gen-protocol`. It is a
  **separate concern** from the core `set-taa-params` (blend / sharpen tunables) — the two never merge.
- **Editor + docs.** A light React/Zustand control drives `set-upscale`; the TAA docs page gains a
  TAAU / upsampling section, `render-quality-tiers.md` gains the dynamic-resolution-under-TAAU story
  (retargeted onto `set-upscale`), and the hub `_index.md` row is updated.

## NO-LEGACY checklist for this phase

- Sharpen is **one** code path: the single `computeMain` RCAS branch from `plans/modern-taa-core/`
  phase 4, now dispatched at display extent with a ratio-modulated lobe. No `add_sharpen_pass`, no
  second PSO, no upscale-only duplicate of the sharpen. `sharpness == 0` is still the exact bypass.
- Dynamic resolution runs on **one** driver: the existing `BudgetController` /
  `BudgetStep::Scale` / `pending_render_scale` loop. No second budget loop, no parallel "TAAU
  auto-res" controller, no feature flag choosing old-blit-upscale vs TAAU (Phase 1 already deleted the
  terminal LINEAR stretch — nothing to keep beside it).
- `set-upscale` is the **single** upscale-control command. The settable `renderScale`, `autoQuality`,
  and `targetFps` fields are **removed** from `SetPerfConfigParams` and rebuilt on `set-upscale`
  (`ratio` / `dynamic` / `targetMs`); every caller (editor, e2e) is migrated in the same change.
  `set-perf-config` keeps only the green/amber/red threshold config. There is exactly one wire path to
  the render scale, the dynamic-resolution enable, and the frame budget.

## Sharpen — `engine/assets/shaders/taa.slang` (+ `engine/crates/rendering/src/aa.rs`)

The `plans/modern-taa-core/` phase 4 already implemented the RCAS-style branch in `computeMain`:
a `+`-tap contrast-adaptive limiter over the current-frame neighbourhood, keyed on the `sharpness`
field packed into `TaaPush`, writing the **sharpened** color to `outColor` and the **unsharpened**
resolved color to `outHistory` (sharpen is a display-time filter, never part of the temporal state).
Do **not** re-specify that kernel — reference it. This phase changes only what upscaling demands, and
needs no Rust/descriptor/push-layout change beyond a `cargo run -p xtask -- shaders` recompile (the
upscale ratio already rides in `TaaPush` from Phase 2).

1. **Sharpen at display extent, on display-grid taps.** After Phase 1 the resolve dispatches over the
   **display** grid (`published_extent()`), so the sharpen already runs per output pixel there — but
   its `+` taps must be the **display-grid neighbours of the reconstructed `result`**, not
   input-extent `current` samples. In the reconstruction body (Phase 2/3) keep the four cardinal
   reconstructed-output values (`n`/`e`/`s`/`w`) in registers alongside the center `result`, and feed
   *those* to the RCAS limiter. Reason: at input extent < display extent a `current` fetch is a
   sub-display-pixel neighbour, so sharpening against it would fight the reconstruction; sharpening the
   reconstructed display neighbourhood recovers exactly the softness the resample introduced.

2. **Ratio-modulate the lobe (upscale-aware).** Read the Phase-2 upscale ratio from `TaaPush`
   (`display / input`, `>= 1`) and lift the RCAS lobe strength slightly as the ratio rises, clamped, so
   a heavy upscale recovers more detail without over-sharpening a native (ratio == 1) frame:

   ```hlsl
   // `sharpness` = TaaParams::sharpness (core, via set-taa-params); `ratio` = TaaPush upscale ratio.
   // n/e/s/w are the DISPLAY-grid reconstructed-output taps; `result` is the reconstructed center.
   float sharpness = /* core `sharpness` field, TaaPush */;
   if (sharpness > 0.0)
   {
       float upscale = saturate(sharpness + 0.15 * (ratio - 1.0));   // ratio 1 → sharpness; more upscale → firmer
       // ... the modern-taa-core RCAS limiter verbatim, but with `upscale` as the lerp knob:
       //     lobe = amp * (-1.0 / lerp(8.0, 5.0, upscale));
       //     result = max((b + lobe*(n+e+s+w)) / (1.0 + 4.0*lobe), 0.0);
   }
   ```

   The `0.15 * (ratio - 1.0)` boost is the only new arithmetic; at ratio == 1 the branch is
   bit-identical to the core sharpen, so a native-resolution frame (input extent == display extent)
   looks exactly as `plans/modern-taa-core/` phase 4 shipped it. The invariants stay: `+` taps only,
   self-limiting amp, non-negative HDR clamp, write the **unsharpened** `result` to `outHistory`.

3. **Update the top-of-file comment** to name the resolve chain as it now stands: dilated reproject →
   Catmull-Rom/Lanczos reconstruct to display extent → YCoCg variance clip → locks + reactive weight →
   luma/velocity-adaptive blend → **upscale-aware RCAS sharpen at display extent**. Describe what it
   does now; no change-journey text, no "used to sharpen at render extent".

> The `sharpness` param itself stays owned by the core `TaaParams` / `set-taa-params` command — this
> phase does **not** add an upscale-specific sharpness knob. `set-upscale` (below) is a separate
> concern (ratio / dynamic resolution). Keeping them apart is deliberate: sharpen tuning is a look
> choice, upscale ratio is a performance choice.

## Dynamic resolution — `engine/crates/rendering/src/{budget.rs, renderer.rs, view_target.rs}`

The driver already exists end-to-end: `BudgetController::update` returns `BudgetStep::Scale(scale)`
below the tier floor, the telemetry hook stores it in `pending_render_scale`, and
`render_scene_offscreen` applies it via `set_render_scale` → `apply_render_extent` at the safe frame
boundary. This phase does **not** add a second loop — it makes that one drive the *input* extent under
TAAU and keeps the display-extent history alive across a scale change.

4. **Confirm the hook drives input extent, not the whole view.** After Phase 1, `set_render_scale` /
   `apply_render_extent` recreate only the **input-extent** class (`scaled_render_extent()`: scene
   color, depth, motion, the G-buffer / SSGI / ReSTIR chain), while the **display-extent** class
   (`published_extent()`: `history[2]`, the resolve output, tonemap, overlays, the present source)
   is untouched. Verify `apply_render_extent` no longer calls `build_aa_targets` for the
   *display-extent* history on a scale-only change — it rebuilds the input-extent AA inputs (motion /
   input scratch) but leaves `history[2]` and the resolve output at their fixed display size. If Phase
   1 left `build_aa_targets` rebuilding everything, split it here so a scale change rebuilds only the
   input-extent members.

5. **Do not hard-reset history on a scale change (the load-bearing behavior).** Today
   `apply_render_extent` invalidates the temporal reprojection (`history_valid = false`, SSGI/ReSTIR
   history reset) because the single-extent history *was* the thing being resized. Under TAAU the
   display-extent history is **not** resized by a scale change, so it must **stay valid**: the resolve
   already resamples the (now differently-sized) input `current` into the fixed display grid with the
   Phase-2 kernel every frame, and motion vectors reproject in resolution-independent UV space, so an
   input-extent change is exactly the case the resolve tolerates. Concretely, on a render-scale-only
   change: rebuild the input-extent targets, **keep `history_valid == true`**, and do **not**
   `flip`/clear `history[2]`. The SSGI / ReSTIR reservoirs (input-extent) still reset — their history
   *is* resized — but the display-extent TAA history rides through. A one-frame trace of extra
   reconstruction softness after a step is acceptable; a per-step history flush (a visible flicker at
   every budget step) is not, and is the bug this step exists to prevent.

6. **Ladder + budget note.** The existing `SCALE_LADDER` (`0.5 … 1.0`) now expresses genuine
   input:display ratios that TAAU reconstructs, not blit-stretch factors. No ladder change is required
   this phase; note in `budget.rs` that a lower floor (e.g. `0.33`) is now defensible because the
   temporal upsampler reconstructs far better than the deleted LINEAR blit — but leave the rungs as-is
   unless the visual check demands it (do not widen scope speculatively). The budget target itself is
   set through `set-upscale`'s `targetMs` (below), which writes `PerfConfig::target_fps`
   (`budget_ms = 1000 / target_fps`), the same value `BudgetController::update` already reads.

## Control command — `saffron-protocol` and `saffron-control`

Follow the `set-aa` / `set-perf-config` precedent end-to-end. Register **two** commands: `get-upscale`
(read) and `set-upscale` (partial update, every field `Option<T>`). `get-upscale` auto-classifies
read-only via the `get-` prefix in `is_read_only_command` (no allow-list edit); `set-upscale` mutates
and therefore repaints — both correct.

### DTOs — `engine/crates/protocol/src/dto.rs`

7. Add the DTOs near the `PerfConfigDto` / `SetPerfConfigParams` block, camelCase, `#[ts(export)]`,
   deriving `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS` (`Default` on the shared
   `UpscaleDto` and on `SetUpscaleParams` for convenience):

   ```rust
   #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct UpscaleDto {
       /// Fixed input:display render scale in (0, 1]; 1.0 = native (no upscaling).
       pub ratio: f32,
       /// Dynamic resolution: the frame-budget controller drives `ratio` toward `target_ms`.
       pub dynamic: bool,
       /// The per-frame budget (ms) the dynamic driver holds to (= 1000 / target_fps).
       pub target_ms: f32,
       /// Current input extent (scene / depth / motion render size) in device pixels.
       pub input_width: u32,
       pub input_height: u32,
       /// Fixed display extent the resolve reconstructs to (the present size), in device pixels.
       pub display_width: u32,
       pub display_height: u32,
   }

   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct GetUpscaleResult { pub upscale: UpscaleDto }

   #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct SetUpscaleParams {
       #[serde(skip_serializing_if = "Option::is_none")]
       pub ratio: Option<f32>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub dynamic: Option<bool>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub target_ms: Option<f32>,
   }

   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct SetUpscaleResult { pub upscale: UpscaleDto }
   ```

8. **NO-COMPAT cutover of `SetPerfConfigParams`.** Delete the `render_scale`, `auto_quality`, and
   `target_fps` `Option` fields from `SetPerfConfigParams` — they are now owned by `set-upscale`.
   `SetPerfConfigParams` keeps only the threshold config (`green_budget_frac`, `green_median_mul`,
   `amber_median_mul`, `frozen_ms`, `vram_warn_frac`, `vram_crit_frac`). Leave `PerfConfigDto` (the
   *read* result) reporting `target_fps` / `budget_ms` / `auto_quality` as read-only telemetry — the
   perf overlay still shows the budget; only the settable path moves.

### Codegen + inventory + command table — `codegen.rs`, `command.rs`

9. `codegen.rs` — add `UpscaleDto`, `GetUpscaleResult`, `SetUpscaleParams`, `SetUpscaleResult` to
   **both** `ts_decls()` (`decl_entry!(Name)`) and `struct_fragments()` (`frag_entry!(Name)`), near the
   `PerfConfigDto` / `SetPerfConfigParams` entries.

10. `command.rs` — four edits:
    - Add all four type names to `DTO_TYPE_NAMES` (the build-time `dtos_are_declared` test fails on any
      missing name).
    - Add two `CommandSpec` rows to `COMMANDS`, placed just after the `set-perf-config` row (same
      render domain): `get-upscale` with `params: "EmptyParams", result: "GetUpscaleResult"`, and
      `set-upscale` with `params: "SetUpscaleParams", result: "SetUpscaleResult"`. Bump the module
      header's command count (`160 typed commands` → `162`).
    - Add each command to `COMMAND_FIXTURES` exactly once: `("get-upscale", "empty")` and
      `("set-upscale", "upscale")`, adding an `upscale` fixture whose JSON validates against
      `SetUpscaleParams` (e.g. `{ "ratio": 0.67, "dynamic": true, "targetMs": 16.7 }`). Because
      `SetPerfConfigParams` changed shape, **re-check the existing `perf-config-30` fixture** — if it
      set `renderScale` / `autoQuality` / `targetFps`, drop those keys so it still validates.

### Handler + renderer seam + live impl + stub

11. `commands_render.rs`, `register_render_commands` — register both handlers beside `set-perf-config`,
    with an `upscale_dto` helper (mirroring `perf_config_dto`) that reads the current state off
    `ControlRenderer`. `set-upscale` is a partial merge, not a replace:

    ```rust
    fn upscale_dto(r: &dyn ControlRenderer) -> UpscaleDto {
        let cfg = r.perf_config();
        let (iw, ih) = r.input_extent();
        let (dw, dh) = r.display_extent();
        UpscaleDto {
            ratio: r.render_scale(),
            dynamic: cfg.auto_quality,
            target_ms: cfg.budget_ms(),
            input_width: iw, input_height: ih,
            display_width: dw, display_height: dh,
        }
    }
    // ...
    reg.register::<EmptyParams, GetUpscaleResult>(
        "get-upscale",
        "get-upscale — TAAU ratio, dynamic-resolution state, and input/display extents",
        |ctx, _p| Ok(GetUpscaleResult { upscale: upscale_dto(ctx.renderer) }),
    );
    reg.register::<SetUpscaleParams, SetUpscaleResult>(
        "set-upscale",
        "set-upscale {ratio,dynamic,targetMs} — TAAU input:display scale + dynamic resolution",
        |ctx, p| {
            if let Some(r) = p.ratio { ctx.renderer.set_render_scale(r); }
            let mut cfg = ctx.renderer.perf_config();
            if let Some(d) = p.dynamic { cfg.auto_quality = d; }
            if let Some(ms) = p.target_ms { cfg.target_fps = if ms > 0.0 { 1000.0 / ms } else { 0.0 }; }
            ctx.renderer.set_perf_config(cfg);
            Ok(SetUpscaleResult { upscale: upscale_dto(ctx.renderer) })
        },
    );
    ```

    Remove the `render_scale` / `auto_quality` / `target_fps` branches from the existing
    `set-perf-config` handler (they moved here). Bump the module `//!` header and the
    `register_render_commands` doc comment from `30 render-domain commands` to `32`.

12. `registry.rs`, `trait ControlRenderer` — the trait already exposes `render_scale()` /
    `set_render_scale()` and `perf_config()` / `set_perf_config()`. Add only the two extent getters the
    result needs (DTO-free primitives so the trait keeps no `saffron-rendering` dependency):

    ```rust
    /// The active view's current input (scene-render) extent in device pixels.
    fn input_extent(&self) -> (u32, u32);
    /// The active view's fixed display (present) extent in device pixels.
    fn display_extent(&self) -> (u32, u32);
    ```

13. `test_support.rs`, `StubRenderer` — implement `input_extent` / `display_extent` over stored fields
    (e.g. a stored input `(w, h)` derived from the display size × `render_scale`, and a stored display
    `(w, h)`), so the `set-upscale` → `get-upscale` round-trip is testable without a GPU. The stub's
    existing `render_scale` / `set_render_scale` and `perf_config` / `set_perf_config` already hold the
    rest of the state.

14. `control_renderer.rs`, `impl ControlRenderer for HostControlRenderer<'_>` — implement
    `input_extent` from the active view's `scaled_render_extent()` and `display_extent` from its
    `published_extent()` (add the `Renderer` accessors if Phase 1 did not already surface them). The
    `render_scale` / `set_render_scale` / `perf_config` / `set_perf_config` impls already delegate to
    the real `Renderer`.

### Regenerate — `xtask gen-protocol`

15. `cargo run -p xtask -- gen-protocol` (toolbox; aliased `bun run gen:protocol`) regenerates the
    committed artifacts — `editor/src/protocol/sa-types.ts`, `schemas/control/openrpc.generated.json`,
    `schemas/control/command-manifest.generated.json`, `schemas/control/sa.generated.luau`. The
    byte-identical tests and `registry_covers_the_protocol_manifest` (handler ↔ manifest set-equality)
    guard the result. **Never hand-edit `sa-types.ts`.**

### `sa` CLI usage

`sa get-upscale` reads the live ratio / dynamic state / extents; `sa set-upscale '{"ratio":0.67}'`
pins a 1.5× upscale; `sa set-upscale '{"dynamic":true,"targetMs":16.7}'` hands the input extent to the
budget driver at a 60 fps target. Both reach the registered commands through `sa`'s
`external_subcommand` — no bespoke CLI code. This phase **is** the `sa` deliverable for the set.

## Editor — `editor/`

16. Light quality control (name the seam; keep it lighter than the engine detail):
    - `editor/src/control/client.ts` — a typed pair `getUpscale()` / `setUpscale(params)` following the
      existing `setAa` / `setRenderQuality` wrappers (`call("get-upscale", {})` /
      `call("set-upscale", params)`).
    - `editor/src/state/store.ts` — an `upscale: UpscaleDto | null` slice + `setUpscale` setter,
      mirroring the existing `perfConfig` / `setPerfConfig` slice; seed it on connect where
      `getPerfConfig` is seeded.
    - `editor/src/panels/RenderPanel.tsx` — a "Resolution" control group beside the AA and Quality
      selects: a ratio slider (or Performance/Balanced/Quality/Native presets → `ratio`), a
      dynamic-resolution toggle (`dynamic`), and — **migrated here** — the existing target-frame-rate
      control, which now calls `setUpscale({ targetMs })` instead of `setPerfConfig({ targetFps })`
      (the `targetFps` setter no longer exists). Show the live input→display extents from
      `getUpscale` as a label.

## Docs — `docs/`

17. Extend `docs/content/explanations/screen-space-and-post/taa.md` (already rewritten to the modern
    spine by `plans/modern-taa-core/` phase 4) with a **Temporal upsampling (TAAU)** section: the
    resolve reconstructs an input-extent scene into the fixed display extent (jitter phase count scales
    `ceil(8·n²)`, Lanczos resample + accumulated weight, locks + reactive mask), and the RCAS sharpen
    runs **at display extent** with a lobe that firms up as the upscale ratio rises. Note the
    pre-tonemap linear-HDR accumulation invariant still holds and that native (ratio == 1) degenerates
    to the core resolve. Add the new symbols to the "In the code" table:
    `renderer.rs · apply_render_extent`; `view_target.rs · scaled_render_extent, published_extent`;
    and the `get-upscale` / `set-upscale` command (`commands_render.rs`).

18. Update `docs/content/explanations/screen-space-and-post/render-quality-tiers.md` — retarget the
    **Auto-quality (frame-budget controller)** section onto the TAAU + `set-upscale` reality: below the
    tier floor the controller drops the **input** extent and the temporal upsampler reconstructs to the
    fixed display extent (not a LINEAR present blit — that path is gone as of Phase 1), and a scale
    change keeps the display-extent TAA history valid. Replace every `set-perf-config --renderScale` /
    `set-perf-config {autoQuality}` / `set-perf-config {targetFps}` reference with `set-upscale`
    (`ratio` / `dynamic` / `targetMs`). Add an "In the code" row for the upscale command
    (`control/src/commands_render.rs · get-upscale, set-upscale`).

19. Update the hub row in `docs/content/explanations/screen-space-and-post/_index.md` for `taa` to name
    the upsampler (e.g. `Halton jitter + reconstruct-to-display + variance clip + locks/reactive +
    RCAS`), and add the `get-upscale`/`set-upscale` command to the CLI/"In the code" table in
    `docs/content/explanations/anti-aliasing/aa-modes.md` (its `commands_render.rs` row), documenting
    that TAAU reconstructs sub-native input to a sharp native display image and that `set-upscale` is a
    partial update.

Keep prose in the house voice (run the `humanizer` pass), one concept per page, front-matter `title`
equal to the body `# H1`, and code pointers as `What | File | Symbols` (symbols, not line numbers).

## Edge cases & risks

- **History flicker on a scale step (the top risk).** If a render-scale change flushes the
  display-extent history (step 5), every budget step under dynamic resolution flickers. The
  display-extent history must ride through an input-extent change; only the input-extent SSGI/ReSTIR
  reservoirs reset. Confirm in the visual check by watching a sustained over-budget scene step down.
- **Sharpen taps at the wrong grid.** The RCAS `+` taps must be the **display-grid reconstructed**
  neighbours, not input-extent `current` fetches — otherwise the sharpen fights the reconstruction and
  aliases. This is the one sharpen change that upscaling forces.
- **Bypass + native exactness.** At `sharpness == 0` the branch is skipped; at `ratio == 1` the
  upscale boost is zero — a native-resolution frame must be bit-identical to the
  `plans/modern-taa-core/` phase-4 look. The visual check depends on both.
- **Partial-update merge, not replace.** `set-upscale` overlays only the `Some` fields, so
  `{"ratio":0.67}` must not clear `dynamic` or reset `targetMs`. The handler reads current
  `perf_config` and overlays — a `From<SetUpscaleParams>` that fills missing fields with defaults would
  be a silent regression.
- **Caller migration is part of the cutover (NO-COMPAT).** Removing `renderScale` / `autoQuality` /
  `targetFps` from `SetPerfConfigParams` breaks every caller that set them: the editor `RenderPanel`
  target-FPS control (step 16) and the e2e tests that call `set-perf-config { targetFps }`
  (`tests/e2e/alarms.test.ts`, `tests/e2e/frame_history.test.ts`) must move to `set-upscale` in the
  same change, or the build / e2e fail. The `perf-config-30` fixture must also drop the removed keys.
- **`targetMs = 0` means uncapped.** Guard the `1000/target_ms` conversion so a zero/negative
  `targetMs` maps to `target_fps = 0` (uncapped), matching `BudgetController::update`'s
  `budget_ms <= 0` hold.
- **Wire types changed → regenerate.** This phase adds DTOs and commands and reshapes
  `SetPerfConfigParams`; `gen-protocol` must run or the byte-identical protocol tests and
  `tools/check-control-schema` fail.

## Ordering / dependencies

Depends on `phase-3-robust-reconstruction.md` (a robust display-extent reconstruction exists to
sharpen and drive). This is the **final phase of the `plans/taa-upsampling/` set** — after it, TAAU is
complete: decoupled extents (Phase 1), resolution-aware reconstruction (Phase 2), robustness (Phase
3), and the sharpen + dynamic-resolution driver + tooling here. Mark the set `COMPLETED` in the README
once this phase's gate is green.

## Verification

Run the milestone gate and confirm each item:

1. **Build + shaders + protocol + lint clean.** `just engine` (recompiles `taa.slang` via
   `xtask shaders`), then `cargo run -p xtask -- gen-protocol`, then `just prepare-for-commit`
   (`cargo fmt`, clippy `-D warnings`, oxlint). The protocol byte-identical tests, `DTO_TYPE_NAMES`
   coverage test, `registry_covers_the_protocol_manifest`, and `tools/check-control-schema` stay green.

2. **`just e2e` stays green**, with the migrated `targetFps` tests now driving `set-upscale`, and the
   new commands round-trip: `get-upscale` returns the current ratio/dynamic/extents,
   `set-upscale { ratio: 0.67 }` echoes the merged state, and a follow-up `get-upscale` reflects it
   (session persistence); a field left out keeps its prior value (partial-update proof).

3. **Command works over `sa`.** In a `just run-engine` (or headless) session: `sa get-upscale` reports
   the state; `sa set-upscale '{"ratio":0.67}'` applies and echoes it; `sa set-upscale
   '{"dynamic":true,"targetMs":16.7}'` enables the driver at a 60 fps target; a follow-up `sa
   get-upscale` confirms it held.

4. **Headless smoke, validation-clean, at sub-native scale.** `just run-engine-headless 8` with a
   pinned `sa set-upscale '{"ratio":0.5}'` (input extent < display extent) boots with a clean
   validation log — the reconstruction + display-extent sharpen must not read out of bounds or emit
   NaN/negative HDR. Repeat with `'{"dynamic":true,"targetMs":8}'` (a tight budget) so the budget hook
   actually steps the input extent down over the run, and confirm the log stays validation-clean across
   the target reallocations.

5. **Visual check** (`just run-engine`; no pixel-diff harness exists, do not invent one):
   - At `ratio 0.5–0.67`, the sub-native input reconstructs to a sharp display image — thin features
     stable, no ghosting on motion, sharpness recovered vs an unsharpened reconstruction.
   - `ratio 1.0` matches the `plans/modern-taa-core/` phase-4 look exactly (upscale boost is zero).
   - With `dynamic` on and a tight `targetMs`, a heavy scene drops the input extent and the frame time
     settles at the budget; the display image stays native-sized and the TAA history does **not**
     flicker at each scale step (confirms step 5's no-history-flush behavior).
   - The grid/gizmo overlays (drawn after TAA at display extent) are unchanged at every ratio.

6. **Docs build.** `cd docs && hugo` builds clean; the TAA page's TAAU section, the retargeted
   `render-quality-tiers.md` auto-quality section, and the hub / aa-modes rows read correctly and name
   only `set-upscale` for the render-scale / dynamic-resolution surface (no lingering
   `set-perf-config --renderScale`).

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ shaders) + `cargo run -p xtask --
  gen-protocol` + `just prepare-for-commit`; `just e2e`; `cd docs && hugo` build.
- **NO-LEGACY:** one sharpen path (the core RCAS branch, now upscale-aware at display extent); one
  dynamic-resolution driver (the existing `BudgetController` / `pending_render_scale` hook); one
  upscale-control command (`set-upscale`, with `renderScale`/`autoQuality`/`targetFps` removed from
  `set-perf-config` and every caller migrated). This phase closes the set — no pre-upscale remnant
  survives in shader, Rust, editor, or docs.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only
  by default).
