# Phase 3 — Robust temporal blend: luma weighting, adaptive feedback, dilated velocity, rejection

**Status:** COMPLETED

Part of the `plans/modern-taa-core/` feature (rebuilding the naive TAA resolve into a modern,
balanced native-resolution temporal anti-aliaser). This phase turns the *blend* from a fixed,
context-free `lerp(cur, hist, 0.9)` into the adaptive accumulation every reference TAA uses:
luma-weighted resolve, a velocity- and shading-adaptive feedback weight, closest-depth velocity
dilation, and real rejection. It builds directly on the sub-pixel jitter enabled in
[`phase-1-jitter-and-unjitter.md`](phase-1-jitter-and-unjitter.md) and the Catmull-Rom + YCoCg
variance-clip reconstruction landed in [`phase-2-history-reconstruction.md`](phase-2-history-reconstruction.md);
[`phase-4-sharpen-and-tooling.md`](phase-4-sharpen-and-tooling.md) then folds a sharpen pass onto the
Phase-3 push and exposes the parameters over the control plane + docs.

## Goal

Replace the last two naive pieces of the resolve — the compile-time `TAA_HISTORY_WEIGHT = 0.9`
feedback and the crude "on-screen-only" gate — with the standard modern blend, and make the tuning
knobs runtime state instead of a `const`:

- **Runtime `TaaParams`** (feedback min/max, velocity rejection, clip gamma, sharpness) on the
  renderer, replacing the `TAA_HISTORY_WEIGHT` constant. Phase 4 wires the control command; Phase 3
  lands the state + the renderer seam (`Renderer::taa_params` / `Renderer::set_taa_params`).
- **Closest-depth velocity dilation.** The resolve reads the motion vector of the *nearest-depth*
  pixel in the 3×3, so silhouette edges reproject with the foreground surface's velocity instead of a
  bilinear average — this is what removes the halo of ghosting around moving object edges. Requires
  binding `motion_depth` (already produced by the motion prepass) into the TAA set.
- **Tonemap/luma sample weighting.** Weight each colour sample by `1/(1+luma)` (Karis anti-flicker)
  during the blend. Because this resolve runs on **pre-tonemap linear HDR**, an unweighted average
  lets a single bright sample flicker the whole pixel; the reciprocal-luma weight suppresses fireflies
  without leaving the linear domain.
- **Velocity- and luma-adaptive feedback** with shading-change rejection: history weight rides high
  (~0.97) when the surface is still and its luma is stable, and drops toward `feedback_min` (~0.88) as
  screen-space velocity grows or the reprojected luma disagrees with the current luma — replacing the
  flat `0.9` and the binary off-screen gate.

The motion invariant from Phase 1 is preserved exactly: **dilation only chooses *which* velocity to
read** (the closest-depth neighbour's), it never alters the stored velocities, so static geometry
still reads zero motion and un-jittered reprojection stays correct.

The resolve spine stays reusable for the later `taa-upsampling` set: input reads (`current`, `motion`,
`motion_depth`) are kept conceptually at *input/render* extent and the write/history/dispatch grid at
*output* extent (identical today), and `screen_size` in the push is the **input** extent so velocity-
in-pixels is computed against the source grid.

## NO-LEGACY checklist for this phase

One resolve path only — the naive pieces are deleted in the same change, not toggled:

- `pub const TAA_HISTORY_WEIGHT: f32 = 0.9` in `aa.rs` is **deleted**. No constant feedback survives;
  every reader (the `TaaPush` build in `add_taa_pass`) moves to `TaaParams`.
- `struct TaaPush { params: Vec4 }` (16 bytes) is **replaced** by the expanded 48-byte push. There is
  no second push type and no `params.x/y` decoding left anywhere.
- The shader's fixed `lerp(cur, hist, push.params.x)` and the `push.params.y < 0.5 || histUv out of
  [0,1]` gate are **removed** — the adaptive feedback + rejection is the only blend.
- The straight `motion.SampleLevel(uv)` velocity fetch is **replaced** by the dilated fetch; there is
  no un-dilated path left beside it.
- The TAA descriptor set gains binding 5 (`motion_depth`) in `create_taa_layout`, `write_aa_sets`, and
  the `add_taa_pass` accesses — one layout, updated in all three places together (a set/layout mismatch
  is a validation error, so this must be atomic).

By the end of this phase the resolve is: dilated reproject → Catmull-Rom history (Phase 2) → YCoCg
variance clip (Phase 2) → luma-weighted, velocity/shading-adaptive blend. Nothing of the original
bilinear + RGB-min/max + fixed-0.9 path remains.

## Engine crate: `saffron-rendering`

### Runtime TAA parameters (`aa.rs`)

**File `engine/crates/rendering/src/aa.rs`.**

1. **Delete** `pub const TAA_HISTORY_WEIGHT: f32 = 0.9;` and its doc comment.

2. Add a plain-data `TaaParams` struct — the runtime tuning the resolve reads, defaulted to the
   balanced reference values (Karis/Lottes/Playdead):

   ```rust
   /// The runtime TAA resolve tuning (replaces the old `TAA_HISTORY_WEIGHT` constant). All are
   /// live-tunable via the control plane (Phase 4). Defaults are the balanced reference values.
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub struct TaaParams {
       /// History weight under fast motion / shading change (the floor). ~0.88.
       pub feedback_min: f32,
       /// History weight when still with stable luma (the ceiling). ~0.97.
       pub feedback_max: f32,
       /// How hard screen-space velocity pulls feedback toward `feedback_min`
       /// (per-pixel-per-frame scale; `saturate(velPx * velocity_rejection)`). ~0.025 ≈ full
       /// rejection near 40 px/frame.
       pub velocity_rejection: f32,
       /// The YCoCg variance-clip half-extent multiplier `gamma` (Phase 2). ~1.0.
       pub clip_gamma: f32,
       /// RCAS-style sharpen strength consumed in Phase 4 (0 = off). Carried here so the push
       /// layout is final; Phase 3's resolve does not sharpen yet.
       pub sharpness: f32,
   }

   impl Default for TaaParams {
       fn default() -> Self {
           Self {
               feedback_min: 0.88,
               feedback_max: 0.97,
               velocity_rejection: 0.025,
               clip_gamma: 1.0,
               sharpness: 0.0,
           }
       }
   }
   ```

3. **Replace** `struct TaaPush { params: Vec4 }` (and its `size_of == 16` assert) with the expanded
   48-byte push. Use six `Vec2`s so the std430 layout is unambiguous (each `vec2` is 8-byte aligned,
   packing tightly to 48 bytes with no padding surprises):

   ```rust
   /// The TAA resolve push (48 bytes, matching `taa.slang`'s `Push`). Six `vec2`s: the adaptive
   /// feedback range, the current + previous NDC jitter (from Phase 1, so the resolve/upsampling
   /// seam can account for sub-pixel offset), the **input** render extent in pixels (velocity →
   /// pixels), the clip gamma + history-valid flag, and the velocity-rejection + sharpen knobs.
   #[repr(C)]
   #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
   pub struct TaaPush {
       /// `x` = feedback_min, `y` = feedback_max.
       pub feedback: saffron_geometry::glam::Vec2,
       /// This frame's NDC jitter offset (Phase 1 `ViewTarget::jitter`).
       pub jitter: saffron_geometry::glam::Vec2,
       /// Last frame's NDC jitter offset (Phase 1 `ViewTarget::prev_jitter`).
       pub prev_jitter: saffron_geometry::glam::Vec2,
       /// The input/render extent in pixels (`velPx = length(mv * screen_size)`).
       pub screen_size: saffron_geometry::glam::Vec2,
       /// `x` = clip gamma (Phase 2 variance clip), `y` = 1.0 if history is valid this frame.
       pub gamma_valid: saffron_geometry::glam::Vec2,
       /// `x` = velocity_rejection, `y` = sharpness (Phase 4).
       pub reject_sharp: saffron_geometry::glam::Vec2,
   }

   const _: () = assert!(size_of::<TaaPush>() == 48);
   ```

   Update the `push_layouts_match_shaders` test in the same file: assert `size_of::<TaaPush>() == 48`.

### Renderer seam + push build (`renderer.rs`)

**File `engine/crates/rendering/src/renderer.rs`.**

4. Add a `taa_params: crate::TaaParams` field to `Renderer`, initialised to
   `crate::TaaParams::default()` where `self.aa` is constructed at renderer init. Add the accessor +
   setter next to `set_aa` / `aa_mode` (`Renderer::set_aa` is the placement precedent):

   ```rust
   /// The current TAA resolve tuning (read by the control `get-taa-params`, Phase 4).
   pub fn taa_params(&self) -> crate::TaaParams {
       self.taa_params
   }

   /// Sets the TAA resolve tuning. Takes effect next frame (the push is rebuilt each frame from
   /// this state); no GPU idle / PSO rebuild needed — it is push-constant data, not baked state.
   pub fn set_taa_params(&mut self, params: crate::TaaParams) {
       self.taa_params = params;
   }
   ```

5. In `add_taa_pass`, **replace** the `TaaPush { params: Vec4::new(TAA_HISTORY_WEIGHT, …) }` build with
   the expanded push, reading `self.taa_params` and the per-view jitter/extent state. The dispatch grid
   stays the output extent; `screen_size` is the **input** extent (identical today — kept distinct so
   the upsampling set can diverge them):

   ```rust
   let params = self.taa_params;
   let input = view.extent(); // input/render extent; the upsampling set reads a source extent here
   let push = crate::TaaPush {
       feedback: Vec2::new(params.feedback_min, params.feedback_max),
       jitter: view.jitter,               // Phase 1 per-view state
       prev_jitter: view.prev_jitter,     // Phase 1 per-view state
       screen_size: Vec2::new(input.width as f32, input.height as f32),
       gamma_valid: Vec2::new(params.clip_gamma, if view.history_valid { 1.0 } else { 0.0 }),
       reject_sharp: Vec2::new(params.velocity_rejection, params.sharpness),
   };
   ```

   (`Vec2` = `saffron_geometry::glam::Vec2`; import or fully-qualify to match the file's existing use.)
   The push is now 48 bytes; the `bytemuck::bytes_of(&push).to_vec()` payload and COMPUTE stage / offset
   0 are otherwise unchanged.

6. **Bind `motion_depth` into the TAA set as slot 5.** `add_taa_pass` currently takes
   `motion: Option<RgResource>`. The motion prepass (`add_motion_pass`) already imports the per-view
   `motion_depth` (D32) as its depth attachment; the cleanest wiring is to have `add_motion_pass`
   **return that `motion_depth` `RgResource` alongside the motion colour** and thread both into the
   `add_taa_pass` call at the dispatch site (where
   `self.add_taa_pass(&mut graph, &pipelines, scene_output, color, motion_resource)` is invoked). Add a
   `motion_depth: Option<RgResource>` parameter to `add_taa_pass`, early-return `None` if it is absent
   (mirroring the existing `let motion = motion?;` guard — the prepass produces both or neither), and add
   its access to the `add_compute_pass` `accesses` array as `SampledReadCompute`, **after** the `motion`
   entry, so the frame graph orders the resolve after the motion prepass's depth store:

   ```rust
   (scene_output, RgUsage::SampledReadCompute),
   (motion, RgUsage::SampledReadCompute),
   (motion_depth, RgUsage::SampledReadCompute),   // NEW — closest-depth dilation source
   (hist_read, RgUsage::SampledReadCompute),
   (color, RgUsage::StorageImageRwCompute),
   (hist_write, RgUsage::StorageImageRwCompute),
   ```

### Descriptor layout (`descriptors.rs`)

**File `engine/crates/rendering/src/descriptors.rs`, `create_taa_layout`.**

7. Add binding 5 as a `COMBINED_IMAGE_SAMPLER` (the depth read), alongside the existing 0–2 samplers
   and 3–4 storage images, and update the doc comment to name it:

   ```rust
   /// The TAA resolve compute set: current/history/motion samplers (0–2), offscreen/history
   /// storage images (3–4), and the motion-prepass depth (5) for closest-depth velocity dilation.
   fn create_taa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
       let bindings = [
           compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(3, vk::DescriptorType::STORAGE_IMAGE),
           compute_binding(4, vk::DescriptorType::STORAGE_IMAGE),
           compute_binding(5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
       ];
       // …unchanged create + checked(...)
   }
   ```

### Descriptor set writes (`view_target.rs`)

**File `engine/crates/rendering/src/view_target.rs`, `write_aa_sets`.**

8. In the `for p in 0..2usize` TAA-parity loop, after the existing slot 0–4 writes for
   `taa = self.taa_sets[p]`, add the slot-5 write pointing at the motion-prepass depth view. Use the
   existing `view_of` helper for the `motion_depth` `Option<Image>` (it returns the offscreen
   placeholder when unbuilt, which the set never samples until TAA is on — the same contract the
   `motion` binding already relies on). The `linear` sampler already in scope is exact at texel centres,
   which is what the 3×3 closest-depth search reads:

   ```rust
   // hoist next to the existing `offscreen` / `motion` / `scene_input` view fetches:
   let motion_depth = self.view_of(&self.motion_depth);
   // …inside the parity loop, after `plan.push(Binding::storage(taa, 4, self.taa_history_view(p)));`
   plan.push(Binding::sampled(taa, 5, linear, motion_depth));
   ```

## Shader: `engine/assets/shaders/taa.slang`

**File `engine/assets/shaders/taa.slang`.** This phase edits `computeMain`'s velocity fetch, blend, and
gate, and expands the push + adds binding 5. The Phase-2 helpers (`SampleHistoryCatmullRom`,
`RGB_to_YCoCg` / `YCoCg_to_RGB`, `clip_aabb`, the m1/m2 variance accumulation) are already present and
stay; this phase feeds them the dilated velocity and wraps them in the adaptive blend.

9. Add binding 5 and replace the push struct to mirror `TaaPush`:

   ```hlsl
   [[vk::binding(5, 0)]] Sampler2D motionDepth;  // motion-prepass depth, for closest-depth dilation

   struct Push
   {
       float2 feedback;     // x = feedbackMin, y = feedbackMax
       float2 jitter;       // current NDC jitter (Phase 1)
       float2 prevJitter;   // previous NDC jitter (Phase 1)
       float2 screenSize;   // input render extent, px
       float2 gammaValid;   // x = clipGamma, y = historyValid
       float2 rejectSharp;  // x = velocityRejection, y = sharpness (Phase 4)
   };
   [[vk::push_constant]] Push push;
   ```

   Update the Phase-2 variance clip to read `push.gammaValid.x` for its `gamma` (it currently uses a
   literal ~1.0 — swap it for the push field so the runtime `clip_gamma` is live).

10. Add a luma helper and the closest-depth dilation. Dilation searches the 3×3 depth neighbourhood for
    the nearest surface and reads the motion vector at that offset. Confirm the depth sense against the
    motion-prepass depth store when implementing — under the engine's `perspective_rh_gl` + Y-flip the
    near plane is the smaller depth, so **nearest = min**; if the motion depth is not reversed, flip the
    comparison. Only the comparison operator changes:

    ```hlsl
    float Luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

    // Closest-depth velocity dilation: read the motion vector of the nearest-depth pixel in the
    // 3x3, so silhouettes reproject with the foreground velocity (kills edge ghosting). This only
    // *chooses which* stored velocity to read — it never alters velocities, so the Phase-1 motion
    // invariant (static geometry = zero motion, un-jittered) is preserved.
    float2 DilatedMotion(float2 uv, float2 texel)
    {
        float2 bestOffset = float2(0.0, 0.0);
        float  closest = motionDepth.SampleLevel(uv, 0.0).r;
        for (int y = -1; y <= 1; y = y + 1)
        {
            for (int x = -1; x <= 1; x = x + 1)
            {
                float2 o = float2(x, y) * texel;
                float  d = motionDepth.SampleLevel(uv + o, 0.0).r;
                if (d < closest) { closest = d; bestOffset = o; }  // near = min for this projection
            }
        }
        return motion.SampleLevel(uv + bestOffset, 0.0).xy;
    }
    ```

11. Rewrite the tail of `computeMain` — the velocity fetch, the clamp/clip (kept from Phase 2), and the
    blend — as the adaptive resolve. **Delete** the `float2 mv = motion.SampleLevel(uv,0.0).xy;` straight
    fetch and the `weight = push.params.x` / `params.y`+bounds gate; the new blend is the only path:

    ```hlsl
    // --- reproject (dilated) ---
    float2 mv = DilatedMotion(uv, texel);
    float2 histUv = uv + mv;

    // --- history reconstruction + variance clip (Phase 2, unchanged) ---
    // cur = current.SampleLevel(uv, 0.0).rgb;  (Phase 2 tap)
    // hist = SampleHistoryCatmullRom(history, linearSampler, histUv, push.screenSize);
    // …accumulate YCoCg m1/m2 over the 3x3, build mu ± push.gammaValid.x * sigma,
    //   hist = clip_aabb(...); convert back to RGB.

    // --- rejection ---
    bool onScreen = all(histUv >= 0.0) && all(histUv <= 1.0);
    bool valid    = push.gammaValid.y >= 0.5 && onScreen;

    // --- luma-weighted, adaptive-feedback blend ---
    float lumaCur  = Luma(cur);
    float lumaHist = Luma(hist);

    // velocity-adaptive: history weight decays as screen-space motion grows.
    float velPx    = length(mv * push.screenSize);
    float velFac   = saturate(velPx * push.rejectSharp.x);          // 0 still … 1 fast
    // shading-change: distrust history whose luma disagrees with current.
    float lumaDiff = abs(lumaCur - lumaHist) / max(lumaCur, max(lumaHist, 0.2));
    float stable   = 1.0 - saturate(lumaDiff);
    // combine into the feedback (history) weight in [feedbackMin, feedbackMax].
    float feedback = lerp(push.feedback.y, push.feedback.x, velFac);
    feedback      *= stable * stable;
    feedback       = valid ? feedback : 0.0;                        // disocclusion / first frame

    // Karis 1/(1+luma) tonemap weighting — suppresses fireflies in the pre-tonemap linear HDR.
    float wCur  = (1.0 - feedback) * (1.0 / (1.0 + lumaCur));
    float wHist =        feedback  * (1.0 / (1.0 + lumaHist));
    float3 result = (cur * wCur + hist * wHist) / max(wCur + wHist, 1e-5);

    outColor[tid.xy]   = float4(result, 1.0);
    outHistory[tid.xy] = float4(result, 1.0);
    ```

    Update the file header comment to describe the modern spine (dilated reproject → Catmull-Rom →
    YCoCg variance clip → luma-weighted adaptive blend), naming binding 5 = motion depth. No
    change-journey wording — describe what the shader does now.

## Edge cases & risks

- **Motion invariant (load-bearing).** Dilation must only pick *which* neighbour's stored velocity to
  read; it must never recompute or offset a velocity. A static scene must still resolve to zero motion
  and read history at `uv`. Verify by leaving the camera still: the resolved image must be rock-steady
  (no creep), the same guarantee Phase 1 established.
- **Jitter stays out of velocity.** Phase 1 keeps the motion prepass jitter-free; this phase reads that
  velocity unchanged. `jitter`/`prev_jitter` ride in the push only for the resolve/upsampling seam —
  they are **not** subtracted from `mv` here (the velocity is already jitter-free). Do not
  "double-unjitter."
- **Depth sense.** The closest-depth comparison assumes the depth convention of the motion-prepass
  `motion_depth` (D32). Confirm min-vs-max against `motion.slang` / the projection before trusting the
  `d < closest` operator; a wrong sense dilates toward the *background* and *adds* silhouette ghosting.
- **`history_valid` on resize / mode change.** `build_aa_targets` already resets `history_valid = false`
  (and Phase 1 resets `prev_view_proj_valid` / jitter). The push carries `history_valid` in
  `gamma_valid.y`; the shader takes the current frame whole (`feedback = 0`) on the first frame after
  any resize, mode switch, or view reactivation — no stale-history smear on resize.
- **Pre-tonemap HDR domain.** The resolve still runs before `add_tonemap_pass` (unchanged order). The
  `1/(1+luma)` weighting is the correct anti-flicker in linear HDR; do **not** move the resolve after
  tonemap or blend tonemapped values (that would break the accumulation domain).
- **`velocity_rejection` units.** It is a per-pixel-per-frame scale folded into `saturate(velPx * k)`
  (default `0.025` ≈ full rejection near 40 px/frame). A value of `0` disables velocity rejection
  (feedback stays at `feedback_max` regardless of motion) — a valid "maximum accumulation" setting, not
  a bug. Setting `feedback_min == feedback_max` reproduces a fixed-weight look — a quick manual way to
  confirm the adaptivity is what changed.
- **Input vs output extent (upsampling seam).** `screen_size` is the **input** render extent so `velPx`
  is measured on the source grid; the dispatch grid + history + write stay output extent. Today they are
  equal, so keep the two reads textually distinct (comment them) so the `taa-upsampling` set can diverge
  them without re-plumbing.
- **Descriptor/layout atomicity.** The layout (step 7), the set writes (step 8), and the pass accesses
  (step 6) must land together; a set written with a binding the layout lacks, or a pass declaring an
  access with no matching image, is a validation error. The headless smoke below catches any mismatch.

## `sa` control command + docs (owned by Phase 4)

Per the keep-current rules, tunable engine state gets a control command and a docs update — but those
are **explicitly owned by [`phase-4-sharpen-and-tooling.md`](phase-4-sharpen-and-tooling.md)**, which
adds the `get-taa-params` / `set-taa-params` command pair over `Renderer::taa_params` /
`set_taa_params` (the seam this phase lands) and rewrites `screen-space-and-post/taa.md` to the modern
spine. Phase 3 deliberately lands only the renderer-side state + seam so Phase 4's control work is a
thin protocol-and-handler change with nothing to refactor. Do **not** add the command or edit the docs
in this phase — that would split one concept across two phases.

## Verification

The milestone gate plus a runtime proof appropriate to a rendering change (there is no pixel-diff
harness — verification is validation-clean smoke, green e2e, and a described visual check):

1. **Build + lint clean.** `just engine` then `just prepare-for-commit` — `cargo clippy -D warnings` +
   `cargo fmt --check` + oxlint pass. `just engine` recompiles `taa.slang` to SPIR-V
   (`cargo run -p xtask -- shaders`); a Slang error here (bad binding, push size) fails the build.
   Confirm `grep -rn TAA_HISTORY_WEIGHT engine/` returns nothing (the constant is gone) and the
   `push_layouts_match_shaders` test asserts `size_of::<TaaPush>() == 48`.

2. **Validation-clean headless smoke.** `just run-engine-headless 30` boots and exits clean with **zero
   validation-layer errors** — specifically no descriptor set/layout mismatch on the new binding 5 and
   no push-constant-range warning for the 48-byte push. The 30-frame count lets several history frames
   accumulate so the adaptive feedback actually runs.

3. **`just e2e` stays green.** The suite boots a headless host and drives it over the control plane; it
   must stay green. This phase changes no wire contract (Phase 4 adds the command), so no `gen-protocol`
   is needed; a validation error introduced by the binding/push change would surface as a dirty log the
   harness rejects.

4. **Visual check via `just run-engine` (or the editor with TAA selected).** With `aa_mode() == "taa"`:
   - **Still image** anti-aliases and is stable — edges are smooth and do not creep or flicker when the
     camera is stationary (confirms the luma-weighted accumulation + Phase-1 jitter).
   - **Motion no longer smears** — pan the camera / move an object across a busy silhouette: the trailing
     ghost the fixed-0.9 bilinear blend produced is gone (confirms dilation + adaptive feedback).
   - **No firefly flicker** on bright specular highlights in motion (confirms the `1/(1+luma)` weighting
     in the pre-tonemap HDR domain).
   - **Resize / AA toggle** does not flash a smear — the first post-resize frame takes current whole
     (confirms `history_valid` gating through `gamma_valid.y`).

5. **Milestone gate at the phase boundary.** `just engine` + `just prepare-for-commit`; leave all
   changes unstaged and report — git is read-only by default; the user stages and commits.
