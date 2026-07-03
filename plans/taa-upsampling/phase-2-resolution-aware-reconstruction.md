# Phase 2 — Resolution-aware reconstruction

**Status:** COMPLETED

Part of the `plans/taa-upsampling/` feature (temporal upsampling / TAAU). Phase 1
(`phase-1-resolution-decoupling.md`) split the single per-view extent into an **input** grid
(`scaled_render_extent()`, where scene/depth/motion render) and a **display** grid
(`published_extent()`, where history, the resolve output, tonemap and overlays live), made
`add_taa_pass` read the input-extent inputs while dispatching over and writing the display grid (the
`texSize` / `screen_size` seam from `plans/modern-taa-core/` phase 2), and left a **temporary bilinear
upsample** of the current-frame color (`current.SampleLevel(uv)` reading the low-res texture at the
display `uv`) as the placeholder that filled the extra display samples. This phase makes reconstruction
**upscale-aware**: it scales the jitter phase count with the upscale ratio, replaces that temporary
bilinear upsample with a Lanczos-2 reproject-and-accumulate resample, and tracks a per-output-pixel
accumulated sample weight so freshly-covered display pixels converge instead of staying blurry. It reuses
the `plans/modern-taa-core/` resolve spine verbatim (Halton jitter, Catmull-Rom history, YCoCg variance
clip, the luma/velocity-adaptive feedback of phase 3) and specifies only the upsampling delta.

## Goal

- **Ratio-scaled jitter.** Replace the fixed `TAA_JITTER_PHASES` modulo in `ViewTarget::advance_jitter`
  with a resolution-aware `jitter_phase_count(input, display)` = `ceil(8·n²)` (FSR2 convention, `n =
  displayW / inputW`), so a heavier upscale spreads the jitter across enough phases that every display
  pixel is eventually covered by a jittered input sample. The Halton generator (`halton`, `jitter_offset`)
  is unchanged; only the cycle length and its extent basis (the input grid) change.
- **Lanczos current-frame reconstruction.** Replace Phase 1's temporary bilinear upsample with a
  Lanczos-2 (a = 2) separable resample of the input-extent `current` into the display grid inside
  `computeMain` — an FSR2-style reproject-and-accumulate current estimate. The kernel's negative lobes
  reconstruct sharp thin features the bilinear stretch smeared; taps clamp to the valid input rect and the
  result clamps to `≥ 0` to keep ringing out of the linear-HDR domain.
- **Accumulation confidence.** Carry a per-output accumulated sample count in the (currently dead)
  history **alpha** channel; scale the phase-3 feedback by `saturate(samples / sampleTarget)` so a
  freshly-covered display pixel (few temporal samples so far) leans on the Lanczos current estimate and
  converges in a few frames, then hands over to the temporal history as samples accumulate.
- **Upscale mapping in the push.** Add the upscale ratio + the accumulation target to `TaaPush` (the input
  extent is already `screen_size` from phase 3, so the input texel size is its reciprocal — no redundant
  field). `TaaPush` is an internal GPU push, not a wire DTO, so no protocol regen.

## NO-LEGACY checklist for this phase

One resolve path, one meaning per field — the Phase-1 placeholders are replaced in the same change, not
guarded beside the new code:

- The Phase-1 **temporary bilinear upsample** of the current color (`float3 cur =
  current.SampleLevel(uv, 0.0).rgb` reading the low-res texture at the display `uv`) is **gone**, replaced
  by `ReconstructCurrent` (Lanczos-2). No `#ifdef`, no "bilinear when scale==1" fork — at 1:1 the Lanczos
  gather degenerates to the exact texel tap on its own (weights collapse to a delta), so the native path
  falls out of the same code.
- The fixed `(self.jitter_index + 1) % crate::TAA_JITTER_PHASES` in `advance_jitter` is **replaced** by
  `% crate::jitter_phase_count(input, display)`. `TAA_JITTER_PHASES` survives only as the base-8 factor
  inside `jitter_phase_count`; there is no second "native vs upscaled" phase-count path.
- The history **alpha** write flips from a dead `float4(result, 1.0)` to the live accumulated sample count
  — one meaning for alpha across both ping-pong history images. No parallel sample-count texture.
- `TaaPush` gains one `upscale` vec2 (56 bytes total); the 48-byte phase-3 push is **replaced**, not
  duplicated. There is no second push type.

## Engine crate: `saffron-rendering`

### Resolution-aware jitter phase count (`aa.rs`)

**File `engine/crates/rendering/src/aa.rs`.**

1. Add a `jitter_phase_count` free function next to `TAA_JITTER_PHASES` (which remains the base-8 factor).
   `n = displayW / inputW` is the linear upscale ratio; FSR2 uses `ceil(8·n²)` phases so the jittered input
   samples eventually blanket every display pixel:

   ```rust
   /// Halton jitter phases for an input→display upscale (FSR2 `ceil(8·n²)`, `n = displayW /
   /// inputW`). More phases at heavier upscale so every display pixel is eventually covered by a
   /// jittered input sample; degenerates to `TAA_JITTER_PHASES` (8) at 1:1.
   pub fn jitter_phase_count(input: vk::Extent2D, display: vk::Extent2D) -> u32 {
       let n = display.width.max(1) as f32 / input.width.max(1) as f32;
       ((TAA_JITTER_PHASES as f32 * n * n).ceil() as u32).max(TAA_JITTER_PHASES)
   }
   ```

   > Key it off the width alone: horizontal and vertical render scale move together
   > (`scaled_render_extent` scales both by the one `render_scale`), so `displayW/inputW ==
   > displayH/inputH`; deriving `n` from width keeps one source of truth. Add a unit test to the `aa.rs`
   > tests module asserting `jitter_phase_count` is `8` at 1:1, `32` at n = 2 (a 0.5 render scale), and
   > monotonic non-decreasing as the input shrinks.

### Per-view jitter advance reads the ratio (`view_target.rs`)

**File `engine/crates/rendering/src/view_target.rs`, `ViewTarget::advance_jitter`.**

2. `advance_jitter` (from `plans/modern-taa-core/` phase 1) currently rolls `prev_jitter`, steps
   `jitter_index` modulo the fixed `crate::TAA_JITTER_PHASES`, and recomputes `jitter` from a single
   extent. Change it to read the **input** grid (`scaled_render_extent()`) for both the offset basis and
   the ratio, and the **display** grid (`published_extent()`) for the ratio — the offset is a fraction of
   an *input* pixel (1 input px = `2/inputW` in NDC), so `jitter_offset` must divide by the input dims:

   ```rust
   pub fn advance_jitter(&mut self) {
       self.prev_jitter = self.jitter;
       let input = self.scaled_render_extent();     // the grid the scene renders on
       let display = self.published_extent();        // the grid the resolve writes
       let phases = crate::jitter_phase_count(input, display);
       self.jitter_index = (self.jitter_index + 1) % phases;
       self.jitter = crate::jitter_offset(self.jitter_index, input.width, input.height);
   }
   ```

   > Phase 1 already retired the ambiguous `extent()` and repointed this basis to the input grid
   > (`scaled_render_extent()`); the change here is switching the modulo from the constant
   > `TAA_JITTER_PHASES` to `jitter_phase_count(input, display)` so the cycle length scales with the
   > upscale ratio. The phase-1 seam in
   > `render_scene` (`SceneRenderer::jitter_offset` → `Renderer::active_view_jitter` → `view.jitter`) is
   > **unchanged** — it still reads the stored `view.jitter`; only that offset's basis (input dims) and its
   > cycle length change here. Because the scene renders at input extent, `viewport_width()/height()` in
   > `render_scene` already report the input dims, so the jitter and the projection agree with no
   > `render_scene` edit.

### Expanded `TaaPush` for the upscale mapping (`aa.rs`)

**File `engine/crates/rendering/src/aa.rs`, `TaaPush`.**

3. Add one `upscale` vec2 to the phase-3 `TaaPush` (six vec2s → seven, 48 → **56** bytes; `vec2` is
   8-byte aligned so this stays tight in std430). `screen_size` already carries the **input** extent in
   pixels (phase 3), so the input texel size is its reciprocal shader-side — do **not** add a redundant
   input-texel field:

   ```rust
   /// … existing phase-3 fields (feedback, jitter, prev_jitter, screen_size, gamma_valid,
   /// reject_sharp) unchanged; `screen_size` is the input render extent in pixels …

   /// `x` = upscale ratio `n = displayW / inputW` (1.0 at native); `y` = accumulation target
   /// (`sampleTarget`) the per-output confidence saturates against.
   pub upscale: saffron_geometry::glam::Vec2,
   ```

   Update the `const _: () = assert!(size_of::<TaaPush>() == 48);` to `== 56` and the
   `push_layouts_match_shaders` test's `TaaPush` size assertion to `56`.

### Push build in the resolve dispatch (`renderer.rs`)

**File `engine/crates/rendering/src/renderer.rs`, `add_taa_pass`.**

4. Extend the phase-3 push build with the two extents Phase 1 already surfaces on the view. Keep
   `screen_size` = the **input** extent (unchanged from phase 3 — velocity-in-pixels is measured on the
   source grid); compute `n` from the display/input width, and derive `sampleTarget` as `8·n²` (the jitter
   cycle length, floored at `TAA_JITTER_PHASES` so native still ramps like a normal TAA warm-up):

   ```rust
   let input   = view.scaled_render_extent();   // source grid (scene/motion), == screen_size
   let display = view.published_extent();         // resolve output / dispatch grid
   let n = display.width.max(1) as f32 / input.width.max(1) as f32;
   let sample_target = (crate::TAA_JITTER_PHASES as f32 * n * n).max(crate::TAA_JITTER_PHASES as f32);
   let push = crate::TaaPush {
       // … phase-3 fields; screen_size stays Vec2::new(input.width as f32, input.height as f32) …
       upscale: Vec2::new(n, sample_target),
   };
   ```

   The dispatch grid stays the display extent (Phase 1); the payload grows to 56 bytes but the
   `bytemuck::bytes_of(&push)` COMPUTE push at offset 0 is otherwise unchanged. No descriptor / binding
   change this phase — `current` (set-0 binding 0) keeps its linear sampler (the Lanczos taps land at
   input texel centres, correct under either filter), and the history rgba16f format is unchanged (alpha
   already exists; this phase gives it meaning).

## Engine shader: `engine/assets/shaders/taa.slang`

**File `engine/assets/shaders/taa.slang`.** This phase edits `computeMain`'s current-color read, its
input-grid gather steps, and its blend tail; it adds the `upscale` push field and two helpers. The
phase-2/phase-3 core helpers (`SampleHistoryCatmullRom`, `RGB_to_YCoCg`/`YCoCg_to_RGB`, `clip_aabb`,
`DilatedMotion`, `Luma`, the m1/m2 variance accumulation, the adaptive feedback) stay; this phase feeds
them the reconstructed current color and wraps the feedback with the confidence term.

5. Mirror the push in the shader `Push` struct — add `float2 upscale; // x = ratio n, y = sampleTarget`
   as the seventh vec2, matching `TaaPush`.

6. Add the Lanczos-2 kernel and a separable 4×4 reconstruction of the current color at display `uv`. The
   input extent is `push.screenSize`; the display `uv` maps directly into input-texel space
   (`uv * inputSize`). The support radius of Lanczos-2 is 2, so gather the surrounding 4×4 input taps:

   ```hlsl
   float Lanczos2(float x)
   {
       x = abs(x);
       if (x < 1e-4) return 1.0;      // sinc(0) = 1
       if (x >= 2.0) return 0.0;      // outside the a=2 support
       const float PI = 3.14159265;
       float px = PI * x;
       return 2.0 * sin(px) * sin(0.5 * px) / (px * px);   // sinc(x) * sinc(x/2)
   }

   // FSR2-style resample of the input-extent current color into the display grid: the display
   // pixel's position in input-texel space, a 4x4 Lanczos-2 gather. The negative lobes
   // reconstruct sharp thin features the bilinear upscale smeared; clamp taps to the valid rect
   // and the result to >= 0 so ringing never injects negative HDR.
   float3 ReconstructCurrent(Sampler2D tex, float2 uv, float2 inputSize)
   {
       float2 pos  = uv * inputSize;                 // sample position, input-texel space
       float2 base = floor(pos - 0.5) + 0.5;         // centre-most texel centre below pos
       float2 inv  = 1.0 / inputSize;
       float3 sum  = float3(0.0, 0.0, 0.0);
       float  wsum = 0.0;
       [unroll] for (int j = -1; j <= 2; j = j + 1)
       {
           [unroll] for (int i = -1; i <= 2; i = i + 1)
           {
               float2 c  = base + float2(i, j);      // this tap's texel centre
               float  w  = Lanczos2(pos.x - c.x) * Lanczos2(pos.y - c.y);
               float2 tapUv = clamp(c * inv, 0.5 * inv, 1.0 - 0.5 * inv);   // clamp to valid rect
               sum  += tex.SampleLevel(tapUv, 0.0).rgb * w;
               wsum += w;
           }
       }
       return max(sum / max(wsum, 1e-5), 0.0);
   }
   ```

   > At 1:1 (`inputSize == displaySize`) the display pixel centres land on input texel centres, so
   > `pos.x - c.x` is `0` for the covering tap and `≥ 1` for the rest — `Lanczos2` collapses to a delta and
   > `ReconstructCurrent` returns the exact current texel. The native-res resolve therefore falls out of
   > this same code with no branch (the NO-LEGACY "degenerates at 1:1" guarantee).

7. **Replace** the Phase-1 temporary bilinear read of the current color with the Lanczos reconstruction:

   ```hlsl
   float2 inputTexel = 1.0 / push.screenSize;                 // input grid step (screenSize = input px)
   float3 cur = ReconstructCurrent(current, uv, push.screenSize);
   ```

8. Step every **input-extent** texture on the input grid, not the display grid. The current-color
   variance neighborhood (phase 2's 3×3 `m1`/`m2` loop over `current`) and the closest-depth
   `DilatedMotion` search both read input-extent textures (`current`, `motion`, `motionDepth` all render
   at `scaled_render_extent()` per Phase 1); stepping them by the display texel would re-read the same
   input texel `n` times and collapse the variance box / defeat the dilation. Use `inputTexel` (from step
   7) as their neighborhood step:

   - In the YCoCg moment loop, sample `current.SampleLevel(uv + float2(x, y) * inputTexel, 0.0)`.
   - Call `DilatedMotion(uv, inputTexel)` (its internal `float2(x,y) * texel` search now walks true input
     texels). The display `texel` (from `outColor.GetDimensions`) remains only for the output-pixel `uv`.

9. Fold the accumulation-confidence term into the phase-3 feedback and carry the sample count in history
   alpha. Read the reprojected count with a plain bilinear alpha tap (the Catmull-Rom sampler returns rgb
   only), grow it by one per valid frame capped at `sampleTarget`, reset to `1` on the `valid == false`
   (disocclusion / first frame) branch phase 3 already computes, and scale the phase-3 `feedback` by the
   confidence so fresh pixels take the Lanczos current whole and converge as samples land:

   ```hlsl
   // --- accumulation confidence (upsampling) ---
   float sampleTarget = push.upscale.y;
   float histSamples  = valid ? history.SampleLevel(histUv, 0.0).a : 0.0;
   float samples      = valid ? min(histSamples + 1.0, sampleTarget) : 1.0;
   float confidence   = saturate(samples / sampleTarget);   // 0 freshly covered … 1 converged

   feedback *= confidence;   // extend phase-3 feedback: low confidence => lean on ReconstructCurrent

   // … phase-3 luma-weighted blend produces `result` from cur/hist and the scaled feedback …

   outColor[tid.xy]   = float4(result, 1.0);
   outHistory[tid.xy] = float4(result, samples);   // alpha carries the accumulated count next frame
   ```

   > This is an **extension** of the phase-3 feedback, not a rewrite: the velocity/luma-adaptive
   > `feedback` and the `valid` gate are exactly as phase 3 landed them; the only new factor is
   > `*= confidence`, and the only new write is the history alpha. `history_valid` (phase 3's
   > `gamma_valid.y`) still forces `feedback = 0` on the first frame after a resize/mode switch, and the
   > `valid ? … : 1.0` reset keeps a disoccluded pixel from inheriting a stale high count.

10. Update the file header comment to describe the upsampling spine (Lanczos-2 reconstruct current at
    display extent → dilated reproject → Catmull-Rom history → YCoCg variance clip → luma-weighted,
    confidence-scaled adaptive blend; history alpha = accumulated sample count). No change-journey wording
    ("used to bilinear-upscale") — describe what the shader does now, per the code-style rule.

`cargo run -p xtask -- shaders` recompiles `taa.slang` to SPIR-V; no `gen-protocol` (`TaaPush` is an
internal rendering-crate GPU push, absent from `saffron-protocol` — confirmed: it is defined in `aa.rs`,
not `protocol/src/dto.rs`).

## Edge cases & risks

- **Jitter basis (load-bearing).** `jitter_offset` must divide by the **input** dims — the sub-pixel
  offset is a fraction of an input pixel. Dividing by display dims would shrink the jitter to sub-input-
  sub-pixel and the accumulated samples would never cover the display grid. Verify against `render_scene`:
  the scene projection is jittered in NDC and the scene renders at input extent, so input-basis jitter is
  the consistent choice.
- **Lanczos ringing → negative HDR.** The negative lobes can drive a tap sum below zero on a hard bright
  edge; the `max(…, 0.0)` clamp keeps that out of the linear-HDR accumulator (a negative sample would
  poison the YCoCg moments and the luma weighting). Keep the clamp.
- **Tap clamping at the frame border.** `clamp(c * inv, 0.5*inv, 1 - 0.5*inv)` keeps the 4×4 window inside
  the valid input rect so border display pixels do not fetch garbage / wrap. Confirm the `current` sampler
  address mode is clamp-to-edge (it already is for the resolve set); the explicit UV clamp is belt-and-
  braces for the outermost taps.
- **Confidence vs disocclusion double-count.** On the first valid frame after a reset, `history_valid`
  (phase 3) already zeroes `feedback`; the confidence read is guarded by the same `valid`, so `samples`
  resets to `1` in lockstep — no stale count survives a disocclusion, and no "count high but color
  disoccluded" mismatch. The bilinear alpha tap can smear a neighbour's high count across a disocclusion
  edge by a texel; the variance clip + `valid` gate already reject the color there, so the residual is a
  one-texel-soft confidence ramp, acceptable.
- **`sampleTarget` at native.** At n = 1, `sampleTarget = 8`, so confidence ramps over ~8 frames — a
  benign TAA warm-up identical in feel to a normal history ramp; in steady state `confidence == 1` and the
  resolve is exactly the phase-3 native blend (the "degenerates at 1:1" guarantee holds where it matters,
  steady state). At n = 2 the target is 32; the pixel is *sharp immediately* (Lanczos current dominates
  while confidence is low) and grows *temporally stable* over the cycle — it is never blurry-and-waiting.
- **History alpha reuse.** Both ping-pong `history[i]` images now carry the count in alpha; the resolve
  reads one and writes the other every frame, so the semantics stay consistent across the flip. The
  freshly-built images' undefined alpha is never read (the first frame is `valid == false`).
- **Push size lockstep.** The `TaaPush` (56) / shader `Push` (seven vec2s) sizes must match; a mismatch is
  a push-constant-range validation error. The `push_layouts_match_shaders` assert and the headless smoke
  below catch it.

## Ordering / dependencies

Depends on **Phase 1** (`phase-1-resolution-decoupling.md`): the input/display extent split, `add_taa_pass`
reading input-extent inputs while dispatching over the display grid, and the `screen_size`/`texSize` seam
must exist before there are "extra" display samples to reconstruct and before `advance_jitter` has two
extents to reason about. It is the prerequisite for **Phase 3** (robust reconstruction — locks + reactive
mask + shading/parallax rejection layer on top of this reconstruction and its confidence channel) and
**Phase 4** (RCAS sharpen, dynamic-resolution hook, `set-upscale` tooling, docs). This phase touches no
wire type, no descriptor set, and no control command — those live downstream (Phase 4).

## Verification

The milestone gate plus a runtime proof appropriate to a rendering change (there is no pixel-diff harness
— do not invent one):

1. **Build + lint clean.** `just engine` (runs `xtask shaders`, recompiling `taa.slang`) then
   `just prepare-for-commit` — `cargo clippy -D warnings` + `cargo fmt --check` + oxlint pass. A Slang
   error (bad push size, kernel typo) fails the build. `just e2e` stays green — no wire contract changed
   (`TaaPush` is internal; no `gen-protocol`).

2. **Unit tests.** `jitter_phase_count` returns `8` at 1:1, `32` at n = 2, and is monotonic
   non-decreasing as the input width shrinks; `push_layouts_match_shaders` asserts
   `size_of::<TaaPush>() == 56`.

3. **Validation-clean headless smoke at render scale < 1.** Drive the input extent below the display
   extent (a `render_scale < 1` via the `Renderer::set_render_scale` seam / the dynamic-resolution budget
   Phase 1 wired, so `scaled_render_extent()` < `published_extent()` and the upscale path actually runs),
   then `just run-engine-headless 32` under TAA (at least one full jitter cycle at 2× upscale) boots and
   exits with **zero validation-layer errors** — no push-constant-range warning for the 56-byte push, no
   sampler/descriptor error from the extra taps.

4. **Visual check via `just run-engine` (or the editor) at a sub-1 render scale, TAA selected.**
   - **Sharp, not stretched.** The upscaled display image reads as reconstructed detail, not a bilinear
     blur of a low-res frame — high-frequency texture and text-like detail are legible where the Phase-1
     temporary upsample looked soft (the Lanczos negative lobes at work).
   - **Thin features reconstruct.** Wires / rigging lines / distant edges resolve to continuous thin lines
     rather than dropping out or shimmering — the ratio-scaled jitter is covering the display grid.
   - **Fresh pixels converge, don't stay blurry.** After a fast camera move or a resize, newly-exposed
     regions are immediately sharp (Lanczos current dominates at low confidence) and settle to stable over
     the next several frames — no lingering soft patch that a fixed-feedback upsampler would leave.
   - **No new ghosting.** Motion is as clean as the phase-3 native resolve — the confidence term must not
     re-introduce a trail (it only *lowers* history weight for fresh pixels; it never raises it).

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ `xtask shaders`) + `just prepare-for-commit`;
  `just e2e`.
- **One resolve path (NO-LEGACY):** the Phase-1 temporary bilinear upsample and the fixed
  `% TAA_JITTER_PHASES` are deleted, not guarded; the resolve degenerates to the core native path at 1:1
  by construction, not by a fork.
- **Stay in linear HDR:** `ReconstructCurrent` resamples the pre-tonemap linear-HDR current color into the
  linear-HDR display accumulator; tonemap + overlays still run *after*, at display extent (Phase 1). Never
  reconstruct or accumulate tonemapped values.
- **No control command / docs here:** the upscale ratio becomes tunable/inspectable state only in Phase 4
  (`get-upscale`/`set-upscale` + the docs rewrite); do not split that concept into this phase.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).
