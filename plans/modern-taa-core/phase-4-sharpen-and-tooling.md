# Phase 4 — Sharpen + control command + docs

**Status:** COMPLETED

Part of the `plans/modern-taa-core/` feature (modern native-resolution TAA). The finishing phase: an
optional RCAS-style contrast-adaptive sharpen folded into the resolve, a `get-taa-params` /
`set-taa-params` control command that inspects and tunes the now-runtime TAA parameters (per the
keep-current `sa` rule), and the `docs/` anti-aliasing/TAA pages brought up to the modern spine (per the
keep-docs-current rule). Depends on `phase-3-robust-blend.md`, which turned the tunables into runtime
state (`TaaParams` on the renderer, `Renderer::taa_params` / `set_taa_params`) and left the `sharpness`
field riding through the expanded `TaaPush`.

## Goal

- A configurable RCAS-style contrast-adaptive sharpen applied to the resolved color (`sharpness == 0`
  fully bypasses it), recovering the residual softness temporal accumulation and the Catmull-Rom
  reconstruction leave behind — the standard final step of a modern TAA/upsampling chain.
- A control-command pair exposing `TaaParams` over the wire so the editor and `sa` CLI can inspect and
  tune TAA at runtime. The read-vs-mutate split falls out for free: `is_read_only_command` in
  `registry.rs` classifies any `get-*` name read-only, so `get-taa-params` never forces a redraw and
  `set-taa-params` does (correct — a param change must repaint).
- The TAA docs page rewritten to the modern spine, its hub row updated, and the new command listed in the
  AA-modes reference (correcting the stale host-default comment the grounding flagged).

## NO-LEGACY checklist for this phase

- Sharpen is **one** code path: a branch inside `computeMain` keyed on `push` sharpness `> 0`. No second
  PSO, no separate post-resolve pass, no `add_sharpen_pass`. Strength 0 is the bypass, not a toggle for a
  parallel path.
- The command is registered **exactly once** per name (`get-taa-params`, `set-taa-params`) with one
  `COMMANDS` row and one `COMMAND_FIXTURES` entry each. No duplicate/parallel "taa config" command, and
  the `set-taa-params` partial-update path is the only way to mutate `TaaParams` over the wire.
- The docs no longer describe the fixed-`0.9`, bilinear, or RGB-min/max resolve anywhere — the TAA page,
  its hub row, and the AA-modes page describe only the shipped spine. The stale
  `set_aa(1, false, false)` "Default AA is 1× MSAA + TAA" comment is corrected wherever docs echo it.

## Sharpen — `engine/assets/shaders/taa.slang`

The `sharpness` value already reaches the shader through the expanded `TaaPush` (Phase 3 packs it into
`velocity_rejection_sharpness`). This phase spends it. No Rust, descriptor, or push-layout change is
needed — only the shader body and a `cargo run -p xtask -- shaders` recompile.

1. Apply an RCAS-style sharpen to the resolved `result` **after** the luma-weighted adaptive blend and
   **before** the dual write to `outColor` / `outHistory`. RCAS is the sharpen-only variant of FidelityFX
   CAS: a `+`-shaped tap pattern (up/down/left/right of the center) with a single sharpness knob and a
   noise-robust denominator, so it lifts local contrast without ringing on already-sharp edges.

   Sharpen the **resolved current-frame** color using the **current-frame** neighborhood taps, never the
   history — the sharpen must not feed itself frame-to-frame. The Phase-2/3 resolve already samples the
   3×3 `current` neighborhood for the variance-clip moments; reuse the four `+` taps (`n`/`e`/`s`/`w`)
   already in hand rather than taking new samples. Adapted to Slang:

   ```hlsl
   // `result` is the resolved linear-HDR color; b/n/e/s/w are the CURRENT-frame + taps.
   float sharpness = push.velocity_rejection_sharpness.y;   // Phase-3 push field
   if (sharpness > 0.0)
   {
       float3 b = result;                                   // sharpen around the resolved center
       // per-channel local min/max over the + pattern (noise-robust CAS range)
       float3 mn = min(min(n, e), min(s, w)); mn = min(mn, b);
       float3 mx = max(max(n, e), max(s, w)); mx = max(mx, b);
       // contrast-adaptive lobe: less sharpening where the neighborhood is near a clamp edge
       float3 amp = saturate(min(mn, max(0.0, 2.0 - mx)) / max(mx, 1e-5));
       // sharpness in [0,1] -> RCAS lobe strength (stronger denominator as it rises)
       float3 lobe = amp * (-1.0 / lerp(8.0, 5.0, saturate(sharpness)));
       float3 sharpened = (b + lobe * (n + e + s + w)) / (1.0 + 4.0 * lobe);
       result = max(sharpened, 0.0);                        // clamp non-negative HDR
   }
   ```

   > This is the RCAS-style adaptation, not a byte-for-byte FidelityFX port (the FSR RCAS internals are
   > not published verbatim; see the set README references). What is load-bearing: `+` taps only, a
   > contrast-adaptive lobe so flat/high-contrast regions self-limit, a non-negative clamp so the sharpen
   > can never push linear HDR below zero, and full bypass at `sharpness == 0`. Sharpen the **resolved**
   > color and write the sharpened value to `outColor`; the choice of whether the sharpened or unsharpened
   > color seeds `outHistory` is deliberate — write the **unsharpened** `result` to history so the sharpen
   > never accumulates across frames (sharpen is a display-time filter, not part of the temporal state).

2. Update the top-of-file comment to name the sharpen as the final resolve step (dilated reproject →
   Catmull-Rom → YCoCg variance clip → luma-weighted adaptive blend → optional RCAS sharpen). Describe
   what it does now; no change-journey text.

> Upsampling-ready: the sharpen runs per output pixel over the resolved output grid, so it already sits
> at the right place for the `taa-upsampling` set to run it at display extent — it reads only `result`
> and the current-frame `+` taps, never assumes input extent == output extent.

## Control command — `saffron-protocol`

Follow the `set-aa` precedent end-to-end (grounding lists every seam; `SetAaParams` / `SetAaResult` at
`dto.rs:1050`/`1058` is the worked example). Register **two** commands: `get-taa-params` (read) and
`set-taa-params` (partial update, every field `Option<T>`).

### DTOs — `engine/crates/protocol/src/dto.rs`

3. Add the DTOs beside the `SetAa*` block, camelCase, `#[ts(export)]`, deriving
   `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS` (add `Default` on `TaaParamsDto` for
   convenience). Mirror the `saffron_rendering::TaaParams` field set from Phase 3 exactly:

   ```rust
   #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct TaaParamsDto {
       pub feedback_min: f32,
       pub feedback_max: f32,
       pub velocity_rejection: f32,
       pub clip_gamma: f32,
       pub sharpness: f32,
   }

   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct GetTaaParamsResult {
       pub params: TaaParamsDto,
   }

   #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct SetTaaParamsParams {
       #[serde(skip_serializing_if = "Option::is_none")]
       pub feedback_min: Option<f32>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub feedback_max: Option<f32>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub velocity_rejection: Option<f32>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub clip_gamma: Option<f32>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub sharpness: Option<f32>,
   }

   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
   #[serde(rename_all = "camelCase")]
   #[ts(export)]
   pub struct SetTaaParamsResult {
       pub params: TaaParamsDto,   // the fully-resolved params after the merge (echo)
   }
   ```

### Codegen + inventory + command table

**Files `engine/crates/protocol/src/codegen.rs`, `engine/crates/protocol/src/command.rs`.**

4. `codegen.rs` — add each new DTO (`TaaParamsDto`, `GetTaaParamsResult`, `SetTaaParamsParams`,
   `SetTaaParamsResult`) to **both** `ts_decls()` (`decl_entry!(Name)`) and `struct_fragments()`
   (`frag_entry!(Name)`), near the `SetAaParams` / `SetAaResult` / `AaModeDto` entries.

5. `command.rs` — three edits, mirroring `set-aa`:
   - Add all four type names to `DTO_TYPE_NAMES` (the `dtos_are_declared`-style build-time test asserts
     every command param/result appears here — a missing name is a compile-time test failure).
   - Add two `CommandSpec` rows to `COMMANDS`, placed right after the `set-aa` row
     (`params: "SetAaParams", result: "SetAaResult"`): `get-taa-params` with
     `params: "EmptyParams", result: "GetTaaParamsResult"`, and `set-taa-params` with
     `params: "SetTaaParamsParams", result: "SetTaaParamsResult"`.
   - Add each command to `COMMAND_FIXTURES` exactly once (next to `("set-aa", "aa")`):
     `("get-taa-params", "empty")` and `("set-taa-params", "<label>")`, choosing/adding a fixture whose
     JSON matches `SetTaaParamsParams` (e.g. `{ "sharpness": 0.5 }`). Every command must resolve through
     `fixture_for` or `skip_for` — the manifest emitter's `unreachable!` invariant and the
     `registry_covers_the_protocol_manifest` test enforce it.

### Handler + renderer seam + live impl + stub

**Files `engine/crates/control/src/commands_render.rs`, `engine/crates/control/src/registry.rs`,
`engine/crates/control/src/test_support.rs`, `engine/crates/host/src/control_renderer.rs`.**

6. `commands_render.rs`, `register_render_commands` — register both handlers beside the existing `set-aa`
   registration (`reg.register::<SetAaParams, SetAaResult>("set-aa", …)`). Import the new DTOs from
   `saffron_protocol`.

   ```rust
   reg.register::<EmptyParams, GetTaaParamsResult>(
       "get-taa-params",
       "get-taa-params — current TAA blend/sharpen parameters",
       |ctx, _params| Ok(GetTaaParamsResult { params: ctx.renderer.taa_params() }),
   );
   reg.register::<SetTaaParamsParams, SetTaaParamsResult>(
       "set-taa-params",
       "set-taa-params {feedbackMin,feedbackMax,velocityRejection,clipGamma,sharpness} — tune TAA",
       |ctx, p| {
           // partial update: read current, overlay only the provided fields, write back.
           let mut cur = ctx.renderer.taa_params();
           if let Some(v) = p.feedback_min { cur.feedback_min = v; }
           if let Some(v) = p.feedback_max { cur.feedback_max = v; }
           if let Some(v) = p.velocity_rejection { cur.velocity_rejection = v; }
           if let Some(v) = p.clip_gamma { cur.clip_gamma = v; }
           if let Some(v) = p.sharpness { cur.sharpness = v; }
           ctx.renderer.set_taa_params(cur.clone());
           Ok(SetTaaParamsResult { params: cur })
       },
   );
   ```

   Bump the two "30 render-domain commands" doc comments in this file (the module `//!` header at the top
   and the `register_render_commands` doc line) to **32** — the count is asserted-by-eye documentation
   that must track reality once two commands are added.

7. `registry.rs`, `trait ControlRenderer` — add two methods beside `aa_mode` / `set_aa` (currently at
   `registry.rs:159`/`166`). The trait uses `TaaParamsDto` as its currency so the trait keeps no
   dependency on `saffron-rendering` (the same reason `aa_mode` returns `String`, not `Aa`):

   ```rust
   /// The current TAA blend/sharpen parameters (Phase-3 runtime state).
   fn taa_params(&self) -> saffron_protocol::dto::TaaParamsDto;
   /// Apply fully-resolved TAA parameters (no idle — it is a per-frame push value).
   fn set_taa_params(&mut self, params: saffron_protocol::dto::TaaParamsDto);
   ```

8. `test_support.rs`, `StubRenderer` (`impl ControlRenderer for StubRenderer` at `test_support.rs:197`) —
   implement the two methods over a stored `TaaParamsDto` field so the round-trip is testable without a
   GPU (store on set, return on get). Mirror how `aa_mode` / `set_aa` (`:369`/`:380`) hold their state.

9. `control_renderer.rs` (`impl ControlRenderer for HostControlRenderer<'_>` at `control_renderer.rs:55`,
   beside `aa_mode`/`set_aa` at `:228`/`:231`) — implement the two methods over the real `Renderer`,
   converting `saffron_rendering::TaaParams` ↔ `TaaParamsDto` and delegating to Phase 3's
   `Renderer::taa_params()` / `Renderer::set_taa_params(TaaParams)`. The DTO↔struct conversion is a plain
   field copy (identical field set); a small `From`/`Into` pair (defined here, not in `saffron-protocol`,
   to keep the protocol crate engine-free) is the tidy form.

### Regenerate + editor (optional UI)

10. `cargo run -p xtask -- gen-protocol` (from the toolbox; aliased `bun run gen:protocol`) regenerates the
    four committed artifacts — `editor/src/protocol/sa-types.ts`, `schemas/control/openrpc.generated.json`,
    `schemas/control/command-manifest.generated.json`, `schemas/control/sa.generated.luau`. The
    byte-identical tests and `registry_covers_the_protocol_manifest` (handler ↔ manifest set-equality)
    guard the result. **Never hand-edit `sa-types.ts`.**

11. Optional editor tune UI (polish, not required for keep-current — the `sa`-driveable command already
    satisfies the rule): a typed wrapper pair `getTaaParams` / `setTaaParams` in
    `editor/src/control/client.ts` (following `setAa`), and a small control group in
    `editor/src/panels/RenderPanel.tsx` shown only when the AA mode reads back as `taa` (sliders for
    `sharpness`, `feedbackMin/Max`, `velocityRejection`, `clipGamma`).

## Docs — `docs/`

12. Rewrite `docs/content/explanations/screen-space-and-post/taa.md` to the modern spine. Lead with the
    concept (why a temporal AA needs sub-pixel jitter to anti-alias a still image at all), then walk the
    resolve: Halton(2,3) jitter and velocity un-jitter (Phase 1), Catmull-Rom history reconstruction
    (Phase 2), YCoCg variance clipping — give the `μ = m1/N`, `σ = sqrt(|m2/N − μ²|)`, clip toward
    `μ ± γσ` via `clip_aabb` math, replacing the old min/max-clamp equation — luma-weighted
    `1/(1+luma)` adaptive feedback with velocity/shading-change rejection (replacing the fixed-weight
    lerp equation, Phase 3), dilated closest-depth velocity, and the optional RCAS sharpen (Phase 4).
    Note the pre-tonemap linear-HDR accumulation invariant and the reusable-for-upsampling spine. Update
    the "In the code" table to the new symbols: `taa.slang · computeMain`; `aa.rs · TaaParams, TaaPush,
    TAA_JITTER_PHASES, jitter_offset`; `view_target.rs · jitter, prev_jitter, advance_jitter`; and the
    `get-taa-params` / `set-taa-params` command (`commands_render.rs`).

13. Update the hub row in `docs/content/explanations/screen-space-and-post/_index.md` from
    `history reprojection + neighbourhood clamp + exponential blend` to the modern description, e.g.
    `Halton jitter + Catmull-Rom history + YCoCg variance clip + luma-adaptive blend + RCAS sharpen`, and
    fix its code-pointer cell to `taa.slang; aa.rs · TaaParams, TaaPush`.

14. Add the `get-taa-params` / `set-taa-params` command to the CLI/"In the code" table in
    `docs/content/explanations/anti-aliasing/aa-modes.md` (its `commands_render.rs` row), documenting the
    tunables and that `set-taa-params` is a partial update. **Correct the stale default:** the grounding
    notes `saffron-host` calls `renderer.set_aa(1, false, false)` (AA **off**) while an adjacent comment
    claims "Default AA is 1× MSAA + TAA". Wherever the docs echo that claim, state the real default (AA
    off at startup; a loaded project's saved `renderSettings.aa` mode overrides it via
    `apply_render_settings`).

Keep prose in the house voice (run the `humanizer` pass), one concept per page, front-matter `title`
equal to the body `# H1`, and code pointers as `What | File | Symbols` (symbols, not line numbers).

## Edge cases & risks

- **Sharpen must not accumulate.** Write the **unsharpened** resolved color to `outHistory` and only the
  sharpened color to `outColor`; feeding sharpened color back into history compounds the lobe every frame
  and rings. This is the single most important sharpen invariant.
- **HDR non-negativity.** RCAS can undershoot; clamp `result` to `>= 0` after the sharpen so the
  pre-tonemap linear-HDR buffer never carries negative radiance into `add_tonemap_pass`.
- **Bypass exactness.** At `sharpness == 0` the branch is skipped entirely, so the frame is bit-identical
  to the Phase-3 look — the visual check depends on this.
- **Overlays unaffected.** The grid/gizmo overlays are drawn after TAA on post-tonemap color
  (`add_grid_overlay_passes`); the sharpen touches only the TAA output and must leave overlay sharpness
  alone — confirm in the visual check.
- **Partial-update merge, not replace.** `set-taa-params` reads current params, overlays only the
  `Some` fields, and writes the merged whole — so `{"sharpness":0.5}` must not reset `feedback_max` to a
  DTO default. The handler in step 6 does the merge; a naive `From<SetTaaParamsParams>` that fills
  missing fields with `0.0` would be a silent regression.
- **Read-only classification.** `get-taa-params` is auto-read-only via the `get-` prefix in
  `is_read_only_command`, so it will not force a redraw on the reactive loop; `set-taa-params` is not, so
  it repaints — both correct, no allow-list edit needed.
- **Wire types changed → regenerate.** Unlike Phase 3 (no wire change), this phase adds DTOs and
  commands; `gen-protocol` must run or the byte-identical protocol tests and `tools/check-control-schema`
  fail.

## `sa` control command (keep-current)

This phase **is** the `sa` deliverable for the set: `sa get-taa-params` reads the live parameters and
`sa set-taa-params '{"sharpness":0.5}'` tunes them (both reach the registered commands through `sa`'s
`external_subcommand`, no bespoke CLI code). Phase 3 deliberately deferred the command here so it lands
with its DTOs, codegen, and docs in one change rather than as a partial.

## Verification

Run the milestone gate and confirm each item:

1. **Build + shaders + protocol + lint clean.** `just engine` (recompiles `taa.slang` via
   `xtask shaders`), then `cargo run -p xtask -- gen-protocol`, then `just prepare-for-commit`
   (`cargo fmt`, clippy `-D warnings`, oxlint). The protocol byte-identical tests, `DTO_TYPE_NAMES`
   coverage test, `registry_covers_the_protocol_manifest`, and `tools/check-control-schema` all stay
   green.

2. **`just e2e` stays green**, and the two new commands round-trip over the wire: a driver call to
   `get-taa-params` returns the defaults, `set-taa-params { sharpness: 0.5 }` echoes the merged params,
   and a follow-up `get-taa-params` reflects the change (session persistence).

3. **Command works over `sa`.** In a `just run-engine` (or headless) session: `sa get-taa-params` returns
   the current params; `sa set-taa-params '{"sharpness":0.5}'` applies and echoes it; a follow-up
   `sa get-taa-params` confirms it held for the session. A field left out of the JSON keeps its prior
   value (partial-update proof).

4. **Headless smoke, validation-clean.** `just run-engine-headless 8` under TAA with a non-zero
   `sharpness` boots with a clean validation log — the sharpen branch must not read out of bounds or emit
   NaN/negative HDR (a bad lobe denominator or missing clamp trips the tonemap/present path, not
   validation directly, so pair this with the visual check).

5. **Visual check** (`just run-engine`, TAA mode; no pixel-diff harness exists):
   - Raising `sharpness` via `sa set-taa-params` visibly crisps the temporally-softened image at
     `~0.3–0.6` without ringing halos on high-contrast edges (the contrast-adaptive lobe self-limits).
   - `sa set-taa-params '{"sharpness":0}'` matches the Phase-3 look exactly (sharpen fully bypassed).
   - The grid/gizmo overlays (drawn after TAA on post-tonemap color) are unchanged at every sharpness.
   - No frame-over-frame sharpening build-up on a static image (confirms the unsharpened-to-history
     write) — the image reaches a stable crispness and holds, it does not keep hardening.

6. **Docs build.** `cd docs && hugo` builds clean; the TAA page, its hub row, and the AA-modes page read
   correctly, match the shipped spine (no lingering fixed-`0.9` / bilinear / min-max description), and the
   corrected default-AA statement is present.

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ shaders) + `cargo run -p xtask --
  gen-protocol` + `just prepare-for-commit`; `just e2e`; `cd docs && hugo` build.
- **NO-LEGACY:** one resolve path with an in-shader sharpen branch; one registration per command; docs
  describe only the current spine. This phase closes the set — no naive-TAA remnant (fixed weight,
  bilinear fetch, RGB min/max clamp, no-jitter resolve) survives anywhere in shader, Rust, or docs.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).
