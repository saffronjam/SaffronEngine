# Phase 1 — Resolution decoupling (input vs display extent)

**Status:** COMPLETED

Part of the `plans/taa-upsampling/` feature (temporal upsampling / TAAU). This is the architectural
prerequisite: it splits the single per-view render extent into two classes — **input** extent
(`scaled_render_extent()`, where the scene / depth / motion / G-buffer chain rasterises) and **display**
extent (`published_extent()`, where the TAA history, the resolve output, tonemap, overlays, and the
present source live) — and makes the reused `plans/modern-taa-core/` resolve spine dispatch over the
display grid while reading input-extent samples. It builds directly on `plans/modern-taa-core/`
(**COMPLETED first**): jitter (`TAA_JITTER_PHASES`, `advance_jitter`, un-jittered motion), the
Catmull-Rom history reconstruction + YCoCg variance clip in `taa.slang`, the `texSize` seam that phase
established, and the luma/velocity-adaptive feedback. Do **not** re-specify any of that; this phase only
moves where the samples come from and where the resolve writes.

## Goal

Today every per-view target in `ViewTarget::build_screen_space` / `build_aa_targets` is sized from the
single `self.offscreen.extent` surfaced by `ViewTarget::extent()`, and the only input≠output split is
the terminal `Renderer::record_shm_copy` blit that `vk::Filter::LINEAR`-stretches the render-extent
offscreen up to `published_extent()`. After this phase:

- The scene color scratch, depth, motion, the whole G-buffer / SSGI / DFAO / clustered-lighting /
  ReSTIR chain, and the MSAA scene targets are sized to **input** extent (`scaled_render_extent()`).
- TAA's two `history[2]`, the resolve output `offscreen`, the overlay depth, tonemap, and the grid /
  gizmo overlays are sized to **display** extent (`published_extent()`). `history[2]` and the resolve
  output are **new at display extent** (today `history` is render-extent and `offscreen` is
  render-extent).
- `Renderer::add_taa_pass` reads the input-extent current color + motion with normalized UVs, dispatches
  over the **display** grid, and writes the display-extent `offscreen` + `history` (the `texSize` seam
  from `plans/modern-taa-core/` phase 2 now unambiguously carries the display / history dimensions).
- `apply_render_extent` recreates the two extent classes **independently** — a display-size change
  rebuilds both, a `render_scale` change rebuilds only the input class — and both reset `history_valid`
  + jitter so the reconstruction restarts.
- The terminal `vk::Filter::LINEAR` upscale in `record_shm_copy` is **deleted**; the present blit is 1:1
  because its source (`offscreen`) is already display extent (the blit survives only to convert
  `RGBA16F` → `BGRA8`, at matching extent).

Phase 1 keeps the current-frame reconstruction deliberately simple — a temporary bilinear/point upscale
of the current input sample as it accumulates — so the frame stays coherent. The real
Lanczos/bicubic reproject-and-accumulate kernel and the accumulated-weight tracking are Phase 2
(`phase-2-resolution-aware-reconstruction.md`).

## NO-LEGACY checklist for this phase

- There is **one** resolve path. The upsampler *is* the resolve when input extent < display extent, and
  degenerates to the core native-res resolve when the two are equal. No `scale == 1` fork, no "native
  resolve vs upscaler" branch, no feature flag.
- The `vk::Filter::LINEAR` present-upscale blit in `record_shm_copy` is **removed**, not kept beside the
  resolve. Present is 1:1 from the display-extent `offscreen`.
- The ambiguous `ViewTarget::extent()` (== `offscreen.extent`) is **retired**; every caller is repointed
  to the explicit `scaled_render_extent()` (scene side) or `published_extent()` (resolve / present side). No
  single accessor silently means two different sizes.
- Accumulate in the same linear-HDR domain the scene rendered in. The resolve stays pre-tonemap; tonemap
  + overlays run *after*, at display extent. Never blend tonemapped values.

## Engine crate: `saffron-rendering`

### The two extent classes on `ViewTarget`

**File `engine/crates/rendering/src/view_target.rs`.**

1. `ViewTarget` already carries both class computations: `scaled_render_extent()` = `round(desired ×
   render_scale)` — the **input** extent, where the scene, depth, motion, and the whole G-buffer /
   screen-space chain rasterise — and `published_extent()` = `desired` (independent of `render_scale`) —
   the **display** extent, where TAA history, the resolve output, tonemap, and the overlays live, and
   what the frame is presented at. Both clamp to ≥ 1px. These two are *the* named extent accessors for the
   whole set; this plan calls them by these exact names throughout (there is deliberately no
   `scaled_render_extent()` / `published_extent()` alias — one name per extent).

   Delete the ambiguous `pub fn extent(&self)` (== `offscreen.extent`, historically "the one extent") and
   repoint **every** caller to the right class. That includes the `advance_jitter` method added by
   `plans/modern-taa-core/` phase 1 in this same file, which reads `self.extent()` for the jitter basis:
   repoint it to `scaled_render_extent()` (the scene renders at input res, so jitter is a fraction of an
   *input* pixel). Phase 2 later scales that method's phase-count modulo; this phase only keeps it
   compiling against the retired `extent()`. A single accessor can no longer silently stand for two sizes.

2. The offscreen image is now the **display**-extent resolve output, and the scene color scratch is the
   **input**-extent render target, so `offscreen.extent` no longer equals the render extent. The two
   last-built sizes are recoverable directly: `offscreen.extent` is the last-built **display** extent,
   `scratch.extent` (see step 4 — `scratch` is now allocated unconditionally) is the last-built
   **input** extent. `apply_render_extent` uses those to detect per-class changes (step 6).

### Per-view target sizing split

**File `engine/crates/rendering/src/view_target.rs`.**

3. **`ViewTarget::new(device, width, height)`** — construction is at `render_scale == 1.0`, so input ==
   display == `(width, height)`; the code below only needs to route each target through the right class,
   which is a no-op at scale 1 but correct once `render_scale` drops:
   - `offscreen` → `published_extent()` (`OFFSCREEN_COLOR_FORMAT`, unchanged usage:
     `COLOR_ATTACHMENT | SAMPLED | TRANSFER_SRC | STORAGE`).
   - `depth` → `scaled_render_extent()` (D32, unchanged usage). The scene depth-prepass writes it at input res.
   - Initialise `desired_width`/`desired_height` from `(width, height)` and `render_scale = 1.0` (as
     today).

4. **`build_screen_space`** currently opens with `let extent = self.offscreen.extent;`. Change it to
   `let extent = self.scaled_render_extent();`. Everything this function builds is a scene-side / screen-space
   target and stays keyed to `extent` (now input): `g_normal`, `g_roughness`, `g_depth`, `ao_raw`,
   `ao_map`, `contact_map`, `ssgi_map`, `ssr_map`, `ssgi_denoised`, `ssgi_resolved`, `dfao_raw`,
   `dfao_denoised`, `dfao_resolved`, `dfao_history[2]`, `specocc_raw`, `specocc_denoised`, `gi_indirect`,
   `prev_color`, `ssgi_history[2]`, and the `half_extent` derived from it. No per-target change beyond
   the one `extent` binding — they are all correctly input-extent already; only their *source* extent
   moves off `offscreen`.

5. **`build_aa_targets`** currently reads `let extent = self.offscreen.extent;` for every AA target.
   Split it into the two classes:

   ```rust
   let input   = self.scaled_render_extent();
   let display = self.published_extent();
   ```

   - `motion`, `motion_depth` → `input` (the motion prepass rasterises at input res; the resolve samples
     it with normalized UV — step 8).
   - `scratch` → `input`. **Allocate it unconditionally** (drop the `aa.fxaa() || aa.taa()` gate): the
     scene always rasterises into an input-extent color target, and the resolve stage always writes the
     display-extent `offscreen`. This removes the "scene renders straight into offscreen" no-AA path
     (which cannot exist once offscreen is display extent and depth is input extent — a graphics pass
     cannot mix attachment extents). Keep its usage `COLOR_ATTACHMENT | SAMPLED | TRANSFER_SRC`.
   - `msaa_color`, `msaa_depth` → `input` (multisampled). The MSAA color resolve target becomes the
     input-extent `scratch` (see step 9), not `offscreen`.
   - `history[2]` → `display` (`OFFSCREEN_COLOR_FORMAT`, `storage_sampled`). This is the change that
     makes the accumulator display-resolution. Its resting-layout seeding
     (`initialize_screen_space_layouts` → `SHADER_READ_ONLY_OPTIMAL`) is unchanged.
   - Add `depth_display` → `display` (D32, `DEPTH_STENCIL_ATTACHMENT | SAMPLED`) — the overlay depth
     (step 11).
   - Keep the existing reset of `history_valid`, `history_index`, `prev_view_proj_valid`, and the jitter
     state (`jitter_index`/`jitter`/`prev_jitter`, from `plans/modern-taa-core/` phase 1) at the top of
     the function.

6. **`resize`** currently recreates `offscreen` + `depth` at one `(width, height)`. Replace its single
   extent with the two classes — the cleanest signature is `resize(&mut self, device, input:
   vk::Extent2D, display: vk::Extent2D)`: recreate `offscreen` at `display`, `depth` at `input`, bump
   `generation`. `build_screen_space` / `build_aa_targets` (called by `apply_render_extent` right after)
   rebuild the rest from `scaled_render_extent()` / `published_extent()`.

7. **Descriptor set writes are extent-agnostic.** `write_aa_sets` / `write_screen_space_sets` bind image
   *views*, not extents, so the TAA set plan (sampler 0 = `scratch` (input), sampler 1 = `history`
   (display), sampler 2 = `motion` (input), storage 3 = `offscreen` (display), storage 4 = `history`
   (display)) and the tonemap set (storage 0 = `offscreen` (display)) need **no structural change** —
   only the underlying image allocations move class. `depth_display` is a graph attachment, not a set
   binding, so it is not written into any per-view set.

### Independent recreation in `apply_render_extent`

**File `engine/crates/rendering/src/renderer.rs`.**

8. `apply_render_extent(i)` today computes one `target = scaled_render_extent()`, early-returns if
   `extent()` matches, then rebuilds everything. Rewrite it to reconcile the two classes independently:

   ```rust
   fn apply_render_extent(&mut self, i: usize) -> Result<()> {
       let input   = self.views[i].scaled_render_extent();
       let display = self.views[i].published_extent();
       let cur_input   = self.views[i].scratch.as_ref().map(Image::extent);  // last-built input
       let cur_display = self.views[i].offscreen.extent;                     // last-built display
       let input_changed   = cur_input != Some(input);
       let display_changed  = cur_display != display;
       if !input_changed && !display_changed {
           return Ok(());
       }
       self.device.wait_idle()?;
       self.views[i].resize(&self.device, input, display)?;
       // Screen-space + AA targets follow their class extents; rebuilding both is correct and
       // idempotent (a display-only change re-sizes offscreen/history/depth_display, an
       // input-only change re-sizes the scene/G-buffer chain). Both reset the temporal history.
       self.views[i].build_screen_space(&self.device, &self.descriptors, &self.ssao)?;
       self.views[i].build_aa_targets(&self.device, &self.descriptors, self.aa)?;
       self.views[i].restir.reset_history();
       self.views[i].restir.build(&self.device, &self.descriptors, &self.restir, input)?;
       Ok(())
   }
   ```

   - `set_viewport_desired_size` changes `desired_*`, so **both** `input` and `display` move → both
     classes rebuild.
   - `set_render_scale` changes only `render_scale`, so only `input` moves → the input class rebuilds,
     the display class (offscreen / history / depth_display) is left at its existing size. Either way the
     rebuild path above resets `history_valid` + jitter (via `build_aa_targets`), because a
     render-scale change re-grids the *input* samples feeding a display-extent accumulator and the
     reconstruction must restart. (Do not try to preserve history across a render-scale change in Phase
     1; resurrection is out of scope until the locks work in Phase 3.)
   - Note `Image::extent` (a small accessor on `Image`) is used to read `scratch`'s built extent; add it
     if not already present, or read `self.views[i].scratch.as_ref().map(|s| s.extent)`.

9. **Retire `ViewTarget::extent()` at every call site** in `renderer.rs`, choosing the class per
   consumer. Scene / screen-space side → `scaled_render_extent()`; resolve / present / overlay side →
   `published_extent()`:
   - `viewport_width` / `viewport_height` (the `SceneRenderer` seam driving `render_scene`) →
     `scaled_render_extent()`. The scene rasterises at input res.
   - `record_scene_graph`'s scene-pass extent, the G-buffer / SSGI / DFAO / cluster passes, and the
     ReSTIR build → `scaled_render_extent()`.
   - `add_fxaa_pass`, `add_taa_pass`, `add_tonemap_pass`, `add_grid_overlay_passes`,
     `add_motion_visualize_pass`, `add_lit_wireframe_pass` (all dispatch/raster over the resolve output)
     → `published_extent()`.
   - The MSAA color resolve target (`color_att.resolve = Some(scene_output)`): `scene_output` is now
     always the input-extent `scratch` (never `offscreen`), and the MSAA depth resolves into the
     input-extent `depth`. The subsequent resolve stage (step 10) takes `scratch` → `offscreen`.

### The resolve reads input, writes display

**File `engine/crates/rendering/src/renderer.rs`.**

10. The scene-graph build (`record_scene_graph`) currently sets `scene_output = scratch` (input) when
    fxaa/taa is on, else `color` (offscreen). With `scratch` always allocated, **`scene_output` is
    always the input-extent `scratch`**, and the offscreen `color` is always the resolve *output*. The
    resolve stage then covers all AA modes with the one input→display seam:
    - `add_taa_pass` (TAA): reads `scene_output` (scratch, input) + `motion` (input) as `SampledRead`,
      reads `hist_read` (display), writes `color` (offscreen, display) + `hist_write` (display).
      **Dispatch groups come from `view.published_extent()`**, not the input extent — the pass produces one
      invocation per *display* pixel. Change `let extent = view.extent();` → `let extent =
      view.published_extent();`; the `groups(extent.width/height)` lines then cover the display grid.
    - `add_fxaa_pass` (FXAA): same shape — reads input `scene_output`, writes display `color`, dispatched
      over `published_extent()`. FXAA's compute samples its source by normalized UV, so this degrades to a
      bilinear upscale of an FXAA-edge-blurred input (acceptable; the sharp reconstruction is TAA's job).
    - No-AA / MSAA: the scene resolves into input `scratch`; a normalized-UV copy compute takes `scratch`
      → `offscreen` at display extent (bilinear when input < display). This replaces the old "scene
      renders straight into offscreen" path (impossible now that the two are different extents) and the
      deleted present-time upscale. One present path, no `scale == 1` fork.
    - `add_tonemap_pass` runs in-place on `offscreen` (display); dispatch from `published_extent()`.

11. **The overlay / depth mismatch (resolve it explicitly).** `add_grid_overlay_passes` (and
    `add_lit_wireframe_pass`) draw on the post-tonemap `offscreen` (**display** extent) but depth-test
    read-only against `depth` (**input** extent). A Vulkan graphics pass requires all attachments to
    share extent + sample count, so the input-extent `depth` cannot back a display-extent overlay pass.

    **Chosen approach: render the overlays at display extent against a display-extent `depth_display`,
    populated by a point (NEAREST) upscale of the input-extent scene `depth`.** Justification: (a) the
    overlays composite on the display-extent color, so their depth attachment *must* be display extent —
    upscaling the depth is the only option that keeps the pass valid; (b) a **point** upscale (not
    bilinear) is correct for depth — interpolating depth across a silhouette would fabricate an
    in-between surface and mis-occlude the gizmo — and the grid / gizmo occlusion is coarse, so
    per-input-pixel depth is visually sufficient; (c) it keeps the overlays untouched otherwise (same
    `GridPush`, same `record_grid` / `record_overlay`). Rejected alternative: rendering the overlays at
    input extent then upscaling the composited color — that would re-blur the sharp gizmo/grid edges the
    resolve just produced, and there is no cheap re-inject after the display resolve.

    Depth cannot be scaled by `vkCmdBlitImage` (depth/stencil blits forbid scaling) and cannot be a
    storage image, so the upscale is a tiny fullscreen graphics pass: sample the input `depth`
    (`SampledRead`, NEAREST) at the display pixel's UV and emit it as `SV_Depth` into `depth_display`
    (`DepthWrite`, depth-write-always). Add it in the post chain right before the grid / overlay passes;
    the render graph derives the `DepthWrite → DepthRead` transition. `add_grid_overlay_passes` and
    `add_lit_wireframe_pass` then take `depth_display` (display) instead of `depth`, and their pass
    extent is `published_extent()`. `add_motion_visualize_pass` needs no depth (it is an in-place compute
    over `color`), so it only moves to `published_extent()`.

### The present becomes 1:1

**File `engine/crates/rendering/src/renderer.rs`.**

12. `record_shm_copy` currently blits `render_extent` → `publish_extent` with `vk::Filter::LINEAR`. Now
    `offscreen.extent == published_extent()` (both display), so the stretch is gone:
    - `let render_extent = self.views[active].offscreen.extent;` and `publish_extent` are now equal; the
      blit `src_offsets` and `dst_offsets` both use the display extent.
    - The `vkCmdBlitImage` **stays** — it performs the `RGBA16F` (`OFFSCREEN_COLOR_FORMAT`) → `BGRA8`
      (`B8G8R8A8_UNORM`) format conversion, which `vkCmdCopyImage` cannot do — but at matching extent and
      with `vk::Filter::NEAREST` (1:1, so no filtering). Delete the `LINEAR`-for-upscale comment; the
      blit no longer scales.
    - `ensure_shm_capture` already sizes the capture at `published_extent()`, so nothing changes there.

## Engine shader: `engine/assets/shaders/taa.slang`

The core `taa.slang` from `plans/modern-taa-core/` (reproject → Catmull-Rom history → YCoCg variance
clip → adaptive blend) already samples `current`, `history`, and `motion` by **normalized UV** and reads
`history`'s `texSize` from `outColor.GetDimensions`. With `outColor` / `outHistory` / `history` now at
**display** extent, `GetDimensions` yields the display size automatically — the `texSize` seam from
phase 2 already carries the display / history dimensions with no code change, and the Catmull-Rom history
reconstruction is correct at display extent.

13. **Current + motion sampling is already resolution-independent — note it, change nothing structural.**
    `current` (scratch) and `motion` are now input extent while the dispatch and `outColor` are display
    extent; because both are sampled with the normalized `uv = (tid.xy + 0.5) / displaySize`, the linear
    sampler maps that UV onto whichever extent each texture actually is. Reading `current` at the display
    UV therefore **bilinearly upsamples** the input color — the temporary Phase-1 reconstruction. This is
    intentionally the FSR2/TSR "point/bilinear current sample" placeholder; it keeps the frame coherent
    while the real kernel lands next.

14. **Do not add the Lanczos kernel or the accumulated-weight tracking here.** The proper
    resolution-aware reconstruction — a Lanczos/bicubic reproject-and-accumulate that resamples the
    input `current` into the display accumulator, an explicit input-extent neighborhood for the variance
    clip, per-output accumulated sample weight so freshly-covered display pixels converge, and the
    upscale ratio (`screen_size / input extent`) carried into `TaaPush` — is
    **`phase-2-resolution-aware-reconstruction.md`**. Phase 1 leaves `TaaPush` and the neighborhood loop
    exactly as `plans/modern-taa-core/` left them; only the extents of the bound images changed. Add a
    one-line note in the file header that the current sample is a temporary bilinear upsample pending the
    Phase-2 kernel (state what it is, not a change-journey).

`cargo run -p xtask -- shaders` recompiles `taa.slang`. No `gen-protocol` this phase (no wire type
changed).

## Edge cases / risks

- **`render_scale == 1` degeneracy.** When input == display every target in both classes is the same
  size; the resolve dispatches 1:1, `current` samples 1:1, and the present blit is a same-extent format
  convert. This is the core native-res TAA resolve — verify it is byte-for-byte the behaviour after
  `plans/modern-taa-core/` COMPLETED, so the decoupling is invisible at scale 1.
- **Half-res screen-space chain.** `build_screen_space`'s `half_extent = input.div_ceil(2)` now halves
  the *input* extent — correct; the GI / AO traces stay at half of the render res, not half of display.
- **`scratch` always allocated.** Dropping the `aa.fxaa() || aa.taa()` gate means the no-AA / MSAA paths
  gain a `scratch` + a copy resolve. Confirm the copy compute (or FXAA/TAA resolve) leaves `offscreen`
  in the layout `record_shm_copy` / tonemap expect (the graph derives it; the existing external-layout
  slot writeback for `offscreen` already handles the exit layout).
- **`depth_display` layout.** It is `DepthWrite` in the upscale pass then `DepthRead` in the grid /
  overlay / lit-wireframe passes; seed its resting layout like the other AA targets so the first frame's
  attachment is valid. It only exists while overlays can run (build it in `build_aa_targets`).
- **Lit-wireframe re-raster.** `add_lit_wireframe_pass` re-draws scene geometry at display extent
  depth-tested against the point-upscaled `depth_display`; minor z-fighting against the upscaled depth is
  acceptable for a debug view mode. Do not special-case it.
- **First frame / disocclusion.** `history_valid` is reset on every rebuild (both classes), so the first
  frame after any extent change forces `weight = 0` (the core resolve's gate) — no stale display-extent
  history is blended against a re-gridded input.

## Ordering / dependencies

Depends on **`plans/modern-taa-core/` COMPLETED** (this phase reuses its resolve spine, `texSize` seam,
jitter, and adaptive feedback verbatim). It is the prerequisite for every later phase in this set:
`phase-2-resolution-aware-reconstruction.md` fills the extra display samples the decoupling created (the
real Lanczos kernel + accumulated weight + the `ceil(8·n²)` jitter phase scaling);
`phase-3-robust-reconstruction.md` adds locks + the reactive mask on top; `phase-4` sharpens (RCAS),
wires the `BudgetStep::Scale` dynamic-resolution hook to drive the input extent, and exposes
`get-upscale`/`set-upscale`. This phase touches no wire type and no control command.

## Verification

Run the milestone gate and confirm each item:

1. **Build + lint clean.** `just engine` (which runs `xtask shaders`, recompiling `taa.slang`) then
   `just prepare-for-commit` — `cargo clippy -- -D warnings` and oxlint pass. No protocol change (no
   wire type touched, so no `gen-protocol`).

2. **`just e2e` stays green.** No control-plane surface changed; the existing viewport / camera / scene
   tests still pass at their default (native) resolution.

3. **Headless smoke at a sub-native render scale, validation-clean.** Boot the host headless under TAA
   with `render_scale < 1` and run at least one full jitter cycle:
   `just run-engine-headless 8` (set the active view's render scale below 1 via `set-render-scale` / the
   control seam before the frames, or seed it in the smoke harness). The validation layer must be clean —
   the resolve now samples input-extent `current` / `motion` while writing display-extent `outColor` /
   `history`, and the point-upscale depth pass writes `depth_display`; any attachment-extent or
   descriptor-extent mismatch trips a VUID here.

4. **Visual check — the payoff (described, not pixel-diffed).** `just run-engine` with TAA and
   `render_scale ≈ 0.6`:
   - The final frame is presented at **display** resolution with **no** overall bilinear-stretch softness
     across the whole image — the low-res frame is reconstructed and presented 1:1 (the terminal
     `LINEAR` stretch is gone). The image is not yet as sharp as native (the Phase-1 current sample is a
     bilinear upsample) but it is a *reconstructed display-res frame*, not a magnified low-res one.
   - The ground grid and the gizmo overlay composite crisply at display resolution and are still occluded
     by scene geometry (the `depth_display` point-upscale holds) — no gizmo bleeding through solid
     meshes, no grid drawn at the low input resolution.
   - Toggling `render_scale` back to `1.0` returns the exact `plans/modern-taa-core/` native resolve
     (the degenerate 1:1 path) with no visible transition artifact beyond the expected one-frame history
     reset.

   There is **no** pixel-diff test harness in this project — do not invent one.

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ `xtask shaders`) + `just prepare-for-commit`;
  `just e2e` green.
- **NO-LEGACY:** one resolve path, no `scale == 1` fork, the `vk::Filter::LINEAR` present-upscale blit is
  deleted, and the ambiguous `extent()` is retired — not kept beside the two new accessors.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).
