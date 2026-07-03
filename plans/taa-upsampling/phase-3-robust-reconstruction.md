# Phase 3 — Robust reconstruction (locks, reactive mask, disocclusion)

**Status:** COMPLETED

Part of the `plans/taa-upsampling/` feature (temporal upsampling / TAAU). This phase adds the robustness
a sub-native **input** extent forces on the resolve: FSR2-style pixel **locks** that protect stable thin
features from the variance clip, a `[0..1]` **reactive mask** that raises current-frame weight on
alpha-blended content with no reliable motion vector, a **shading-change + parallax disocclusion**
rejection layered on top of the core feedback, and a single consistent linear/exposure accumulation
domain. It builds on the decoupled extents from [`phase-1-resolution-decoupling.md`](phase-1-resolution-decoupling.md)
(scene/depth/motion at `scaled_render_extent()`; history + resolve output at `published_extent()`) and
the upscale-aware reconstruction from [`phase-2-resolution-aware-reconstruction.md`](phase-2-resolution-aware-reconstruction.md)
(Lanczos reproject-and-accumulate into the display grid, per-output accumulated weight, the upscale
ratio in `TaaPush`). [`phase-4-sharpen-dynres-and-tooling.md`](phase-4-sharpen-dynres-and-tooling.md)
then folds in RCAS, the dynamic-resolution hook, and the `set-upscale` control surface — and exposes the
tuning knobs this phase lands.

It **layers on** the `plans/modern-taa-core/` phase-3 feedback (velocity/luma-adaptive `feedback`,
closest-depth velocity dilation reading `motionDepth` at binding 5, the YCoCg variance clip's
`push.gammaValid.x` gamma) — that blend is reused verbatim; this phase modulates its inputs (the lock
resists the clip, the reactive mask raises current weight, the disocclusion gate forces `feedback = 0`).
It does **not** re-specify jitter, Catmull-Rom history, the variance clip, or the base adaptive feedback.

## Goal

- **Pixel locks.** A per-view **reconstruction-state** ping-pong image pair (`lock[2]`, RGBA16F) rides
  alongside `history[2]` at display extent, sharing the `history_index` parity so `flip_history` rolls it
  for free. It stores, per output pixel: `.r` = lock **lifetime** (frames remaining), `.g` = lock
  **luminance** (the resolved luma captured when the lock was created — the shading-change reference),
  `.b` = previous-frame **linear view depth** (the disocclusion reference), `.a` = spare. A locked pixel
  (`lifetime > 0`) **tightens toward keeping history** — the variance clip is widened and the feedback
  floor is raised — so a jittered thin feature that the 3×3 neighbourhood would otherwise reject stays
  stable across frames; the lock is **broken** (lifetime zeroed) on a shading-change (current luma
  disagrees with the stored lock luma) or a disocclusion, and **re-created** when a pixel newly resolves
  as a thin high-frequency detail.
- **Reactive mask.** A new `[0..1]` per-view reactive input (`reactive`, R8) is bound as a **new TAA set
  slot** and sampled in `computeMain`. Alpha-blended / translucent surfaces write their blend coverage
  into it during the translucent scope (their motion vector is unreliable — the geometry behind them
  moved, not the blended layer), and the resolve raises the current-frame weight there so translucent
  content does not ghost.
- **Shading-change + disocclusion rejection.** A parallax disocclusion test at display extent (reprojected
  linear-depth mismatch, using the dilated `motionDepth` this frame and `lock.b` from last) plus the
  lock's shading-change test **compose with** the core luma/variance rejection (they force `feedback = 0`
  and break the lock; they never duplicate the variance clip).
- **Consistent linear/exposure domain.** The resolve stays pre-tonemap in linear HDR. If a pre-exposure
  scale is ever applied, it is applied identically to `current` and `history` so the accumulation domain
  is stable; the resolve must never blend tonemapped values (tonemap + overlays stay *after* it, at
  display extent — the `plans/taa-upsampling/` ground rule).

## NO-LEGACY checklist for this phase

One resolve path; the new robustness extends the single upsampling resolve — no parallel duplicate, no
"scale==1 skips locks" fork, no feature flag toggling old-vs-new:

- The lock state lives in **one** place — the dedicated `lock[2]` ping-pong pair — not duplicated into
  `history.a` (Phase 2 owns `history.a` for accumulated weight) and not recomputed from scratch each frame.
  There is exactly one lock read (slot 7) and one lock write (slot 8).
- The reactive mask is **one** input at **one** binding (slot 6). No second "auto-reactive" copy path is
  added beside the translucent-pass write; the translucent scope is the single producer.
- The disocclusion + shading-change gate **feeds** the existing `plans/modern-taa-core/` phase-3
  `feedback` (forces it to `0`, widens/tightens the clip via the lock) — it does **not** introduce a second
  blend or a second rejection of the variance-clip kind. The variance clip stays the phase-2-core clip; the
  lock only scales its `gamma`.
- The `create_taa_layout` set, the `write_aa_sets` writes, and the `add_taa_pass` accesses gain slots
  6/7/8 **together** in one change (a set/layout mismatch or an access with no matching image is a
  validation error, so the three edits are atomic).
- The resolve remains pre-tonemap. No tonemapped-value blend is introduced; `add_tonemap_pass` order is
  unchanged.

By the end of this phase the display-extent resolve is: dilated reproject (core) → Lanczos accumulate
(phase 2) → YCoCg variance clip **scaled by the lock** → disocclusion/shading gate → luma-weighted
adaptive blend (core) with the **reactive** current-weight boost → dual write of color + history **and**
the updated lock state.

## Engine crate: `saffron-rendering`

### Reconstruction-state (lock) + reactive-mask targets (`view_target.rs`)

**File `engine/crates/rendering/src/view_target.rs`.**

1. Add two per-view target fields on `ViewTarget`, next to `history: [Option<Image>; 2]`:

   ```rust
   /// FSR2-style reconstruction state (RGBA16F), display extent, ping-pong sharing TAA's
   /// `history_index` parity: `.r` = lock lifetime (frames), `.g` = lock luminance (shading-
   /// change reference), `.b` = previous-frame linear view depth (disocclusion reference),
   /// `.a` spare. Built only when TAA is on.
   pub lock: [Option<Image>; 2],
   /// The `[0..1]` reactive mask (R8, **input** extent): per-pixel current-frame-weight boost
   /// the translucent scope writes for alpha-blended content. Built only when TAA is on.
   pub reactive: Option<Image>,
   ```

   Initialise both to `[None, None]` / `None` in `ViewTarget::new` (alongside the existing `history`,
   `motion_depth` initialisers). (Note: the existing `reactive` **module** — `PowerState` /
   `ReactiveState` in `reactive.rs` — is the dynamic-resolution power state, unrelated to this TAA
   reactive mask; keep the field name `reactive` on `ViewTarget` distinct from that module.)

2. In `build_aa_targets`, reset both new fields at the top with the other AA targets
   (`self.lock = [None, None]; self.reactive = None;` next to `self.history = [None, None];`), and build
   them inside the existing `if aa.taa()` block, right after the `history` pair:

   ```rust
   // Reconstruction-state ping-pong at DISPLAY extent (same extent as history[2]).
   let mut lock_0 = Image::new(resources,
       &ImageDesc::color_2d(extent, OFFSCREEN_COLOR_FORMAT, storage_sampled))?;
   let mut lock_1 = Image::new(resources,
       &ImageDesc::color_2d(extent, OFFSCREEN_COLOR_FORMAT, storage_sampled))?;
   initialize_screen_space_layouts(device, &[&lock_0, &lock_1])?;
   lock_0.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
   lock_1.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
   self.lock = [Some(lock_0), Some(lock_1)];
   ```

   Build `reactive` at **input** extent (it is a scene-side signal — the translucent draws write it, and
   they run at `scaled_render_extent()`). In Phase 1 the AA targets that follow the scene grid are sized
   from the input extent; size `reactive` the same way (an `R8_UNORM`, `COLOR_ATTACHMENT | SAMPLED`
   image), cleared to `0` each frame (step 8). The lock pair follows the **display** grid like `history`
   — `extent` in `build_aa_targets` is whichever grid the Phase-1 split assigns to `history`; the lock
   uses that exact one so its reproject arithmetic matches history's.

   > NO-COMPAT: do not gate the lock/reactive build on the upscale ratio. They are built whenever
   > `aa.taa()`; at ratio 1.0 the lock simply protects native-res thin features (still useful), and the
   > reactive mask still de-ghosts translucency. One path.

3. `flip_history` needs **no change** — it already flips `history_index`, and the lock pair is indexed by
   that same parity (step 5's writes use `history_index` exactly as `history` does). Add a one-line note
   to `flip_history`'s doc that the lock pair rides the same parity. `reactive` is single-buffered
   (rewritten every frame), so it is not part of the ping-pong.

### Runtime tuning knobs (`aa.rs`)

**File `engine/crates/rendering/src/aa.rs`.**

4. Extend `TaaParams` (defined by `plans/modern-taa-core/` phase 3) with the lock/reactive/disocclusion
   knobs — plain fields with balanced defaults; the control-plane exposure is owned by Phase 4:

   ```rust
   /// Frames a freshly created lock survives before it must be renewed (FSR2 ≈ a few frames). 0 = locks off.
   pub lock_lifetime: f32,          // default 4.0
   /// How hard the reactive mask pulls the blend toward the current frame (`saturate(mask * reactive_scale)`).
   pub reactive_scale: f32,         // default 1.0
   /// Relative reprojected-depth mismatch that counts as a disocclusion (`|d - dPrev| > k * d`).
   pub disocclusion_threshold: f32, // default 0.10
   /// Luma disagreement (vs. the stored lock luma) that breaks a lock, as a fraction. 
   pub lock_break_luma: f32,        // default 0.25
   ```

   Set them in `TaaParams::default()`. These are the only new `aa.rs` state; the renderer already carries
   `taa_params` (core phase 3) and rebuilds the push each frame from it.

5. Extend `TaaPush` (the 48-byte, six-`vec2` push from core phase 3, which Phase 2 already grew with the
   upscale ratio / display extent) by **appending** three more `vec2`s so std430 packing stays
   unambiguous — do not reorder the existing fields Phases core-3 / 2 rely on, append at the end:

   ```rust
   /// x = lock initial lifetime (frames), y = reactive_scale.
   pub lock_reactive: saffron_geometry::glam::Vec2,
   /// x = disocclusion_threshold (relative), y = lock_break_luma.
   pub disoccl: saffron_geometry::glam::Vec2,
   /// Camera near/far for linearizing `motionDepth` in the disocclusion test (x = near, y = far).
   pub depth_params: saffron_geometry::glam::Vec2,
   ```

   Update the `const _: () = assert!(size_of::<TaaPush>() == …)` and the `push_layouts_match_shaders`
   assertion to the new size (the phase-2 size + 24 bytes for the three `vec2`s), and mirror the fields in
   `taa.slang`'s `Push` (step 9). Fill `depth_params` from the active camera's near/far at the
   `add_taa_pass` push build (they are already available where the motion prepass builds its viewProj).

### Descriptor layout (`descriptors.rs`)

**File `engine/crates/rendering/src/descriptors.rs`, `create_taa_layout`.**

6. The on-disk `taa.slang` set is bindings `0..4` (0 current, 1 history, 2 motion, 3 outColor,
   4 outHistory); `plans/modern-taa-core/` phase 3 appends binding **5** (`motionDepth`). This phase
   appends three more, so `create_taa_layout` grows from six bindings to nine:

   ```rust
   /// The TAA resolve compute set: current/history/motion samplers (0–2), offscreen/history storage
   /// (3–4), motion-prepass depth (5, closest-depth dilation), the reactive mask (6), and the
   /// reconstruction-state (lock) read sampler (7) + write storage (8).
   fn create_taa_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
       let bindings = [
           compute_binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
           compute_binding(3, vk::DescriptorType::STORAGE_IMAGE),
           compute_binding(4, vk::DescriptorType::STORAGE_IMAGE),
           compute_binding(5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER), // motionDepth (core phase 3)
           compute_binding(6, vk::DescriptorType::COMBINED_IMAGE_SAMPLER), // reactive mask  (NEW)
           compute_binding(7, vk::DescriptorType::COMBINED_IMAGE_SAMPLER), // lock read      (NEW)
           compute_binding(8, vk::DescriptorType::STORAGE_IMAGE),          // lock write     (NEW)
       ];
       // …unchanged create + checked(…)
   }
   ```

   The lock read is a **sampler** (it is reprojected through the motion vector at `histUv`, like
   `history`); the lock write is a **storage image** (in-place at `tid.xy`, like `outHistory`).

### Descriptor set writes (`view_target.rs`, `write_aa_sets`)

**File `engine/crates/rendering/src/view_target.rs`, `write_aa_sets`.**

7. Hoist the new views next to the existing `motion` / `scene_input` fetches, using the same
   placeholder contract (`aa_view` returns the offscreen when unbuilt — valid because the set is unused
   until TAA turns on and rebinds), and add three writes inside the `for p in 0..2usize` parity loop
   after the existing slot-5 (`motionDepth`) write. The lock read/write use the parity exactly as
   `history` does — read `lock[1 - p]`, write `lock[p]`:

   ```rust
   // hoisted next to `let motion = self.aa_view(&self.motion);`
   let reactive = self.aa_view(&self.reactive);
   // …inside the parity loop, after the slot-5 motionDepth write:
   plan.push(Binding::sampled(taa, 6, linear, reactive));
   plan.push(Binding::sampled(taa, 7, linear, self.lock_view(1 - p)));
   plan.push(Binding::storage(taa, 8, self.lock_view(p)));
   ```

   Add a `lock_view(&self, i: usize) -> vk::ImageView` helper mirroring `taa_history_view` (returns
   `self.lock[i]`'s view, or the offscreen placeholder when TAA is off). The `linear` sampler already in
   scope is correct: the reactive mask reads bilinearly at input UVs, and the lock reprojects at `histUv`
   (a point/near read is also acceptable — the lock stores per-pixel state, so keeping `linear` matches
   the history read and avoids a second sampler).

### Renderer wiring (`renderer.rs`)

**File `engine/crates/rendering/src/renderer.rs`.**

8. **Reactive mask production.** The translucent draws are already recorded via
   `record_transparent_draw_list` (`draw_list.rs` `transparent_batches`; the `translucent` PSO
   permutation in `gpu_types.rs`). Make the reactive mask a **second color attachment** on the
   translucent scope's frame-graph pass: the pass writes `offscreen` (blended color, as today) at
   location 0 and `reactive` (R8) at location 1, where the translucent fragment shader outputs its blend
   coverage (its output alpha, or `1.0` for a fully reactive layer) into location 1. Clear `reactive` to
   `0` before the scene pass (a `RgUsage::ColorWrite` clear, or fold the clear into the pass's `LoadOp`)
   so opaque pixels stay `0` and only translucent fragments raise it. Ground this on the existing scene
   / translucent pass build (where `record_scene_draw_list` / `record_transparent_draw_list` are invoked
   from the frame-graph assembly) — add the `reactive` attachment there; opaque draws never touch it.

   > If wiring the second attachment onto the translucent scope proves to entangle the opaque scene pass
   > (shared dynamic-rendering scope), the fallback the task allows is to **supply** the mask as an input:
   > a tiny dedicated pass that re-draws only `transparent_batches` writing R8 coverage after the scene
   > pass. Prefer the MRT attachment (one draw); the dedicated pass is the documented fallback, not a
   > second parallel path kept beside it.

9. **`add_taa_pass`** gains the three new graph resources and threads the new push fields. Import the
   reactive mask and the lock read/write like the existing `hist_read` / `hist_write` (the lock pair
   carries its layout across frames exactly like `history`, so each rides an `alloc_external_layout`
   slot and is written back after execute — extend the returned `TaaHistorySlots` shape, or add a
   parallel `lock` slot pair, so `writeback_history_layout` restores the lock layouts too). Add the
   accesses to the `add_compute_pass` `accesses` array, `SampledReadCompute` for reactive + lock-read and
   `StorageImageRwCompute` for lock-write, ordered after the existing entries:

   ```rust
   (scene_output, RgUsage::SampledReadCompute),
   (motion,        RgUsage::SampledReadCompute),
   (motion_depth,  RgUsage::SampledReadCompute),
   (reactive,      RgUsage::SampledReadCompute),   // NEW slot 6
   (lock_read,     RgUsage::SampledReadCompute),   // NEW slot 7
   (hist_read,     RgUsage::SampledReadCompute),
   (color,         RgUsage::StorageImageRwCompute),
   (hist_write,    RgUsage::StorageImageRwCompute),
   (lock_write,    RgUsage::StorageImageRwCompute), // NEW slot 8
   ```

   Extend the push build (which core phase 3 rewrote to read `self.taa_params`) with the new fields:

   ```rust
   lock_reactive: Vec2::new(params.lock_lifetime, params.reactive_scale),
   disoccl:       Vec2::new(params.disocclusion_threshold, params.lock_break_luma),
   depth_params:  Vec2::new(camera_near, camera_far),
   ```

   The dispatch grid stays the **display** extent (Phase 1); the lock/history reproject reads use the
   Phase-2 `screen_size` seam (input extent) for velocity-in-pixels. Nothing about extents is re-derived
   here — this phase only adds bindings + push fields onto the Phase-2 pass.

10. **Consistent exposure domain.** The resolve already runs before `add_tonemap_pass` (unchanged). Today
    no pre-exposure scale is applied before the resolve (`TonemapPush::new(self.exposure_ev, …)` bakes
    `exp2(exposure_ev)` only at tonemap, `overlay.rs`). Keep it that way: **do not** introduce a
    pre-exposure into the resolve. Add an assertion-in-comment at the push build that `current` and
    `history` are both raw linear-HDR at the same scale, and that any future pre-exposure must be applied
    to *both* `current` and the value written to `outHistory` (never only one), so the accumulation stays
    in a single domain. This is a guard, not new code — the point is that Phase 3 does not silently open a
    tonemapped/exposure-mismatched blend.

## Shader: `engine/assets/shaders/taa.slang`

**File `engine/assets/shaders/taa.slang`.** This phase adds bindings 6/7/8, extends `Push`, and inserts
the lock update / reactive weight / disocclusion gate around the core phase-3 blend. The core helpers
(`SampleHistoryCatmullRom`, `RGB_to_YCoCg`/`YCoCg_to_RGB`, `clip_aabb`, `DilatedMotion`, `Luma`) and the
Phase-2 Lanczos accumulate are already present and reused.

11. Add the bindings + push fields mirroring `TaaPush`:

    ```hlsl
    [[vk::binding(6, 0)]] Sampler2D reactive;        // [0..1] reactive mask (input extent)
    [[vk::binding(7, 0)]] Sampler2D lockHistory;     // reconstruction state, reprojected at histUv
    [[vk::image_format("rgba16f")]]
    [[vk::binding(8, 0)]] RWTexture2D<float4> outLock; // reconstruction state, written at tid.xy

    // …appended to Push (after the phase-2 fields):
    float2 lockReactive; // x = lockLifetime (frames), y = reactiveScale
    float2 disoccl;      // x = disocclusionThreshold (relative), y = lockBreakLuma
    float2 depthParams;  // x = near, y = far
    ```

12. **Disocclusion test** (composes with, does not replace, the core rejection). Reproject the stored
    previous linear depth and compare against this frame's dilated closest depth. `motionDepth` is device
    depth (D32); linearize with `depthParams` (respect the projection's depth sense — the core phase-3
    note establishes near = min under `perspective_rh_gl` + Y-flip):

    ```hlsl
    float LinearizeDepth(float d, float2 nf) {
        // perspective_rh_gl device depth -> positive view-space linear depth.
        return (nf.x * nf.y) / max(nf.y - d * (nf.y - nf.x), 1e-6);
    }
    // curDepth from the dilation's closest-depth tap (reuse the value DilatedMotion already found);
    // prevDepth reprojected from the lock's stored depth channel.
    float4 lockPrev = lockHistory.SampleLevel(histUv, 0.0);   // .r life .g luma .b depth
    float curLin  = LinearizeDepth(closestDepth, push.depthParams);
    float prevLin = lockPrev.b;
    bool disoccluded = abs(curLin - prevLin) > push.disoccl.x * max(curLin, 1e-3);
    ```

    A newly revealed background pixel behind a moving foreground shows a large relative depth jump →
    `disoccluded`; ordinary camera translation stays under the relative threshold. This is a coarse gate
    that catches the gross parallax reveals the variance clip can miss — the variance clip still does the
    fine work.

13. **Lock update rule.** Read the previous lock, decrement its lifetime, break it on shading change or
    disocclusion, and (re)create it where the current pixel resolves as a stable high-frequency detail
    (a strong local luma contrast the neighbourhood variance flags as thin). A locked pixel widens the
    variance-clip `gamma` and lifts the feedback floor so history is kept:

    ```hlsl
    float lockLife = max(lockPrev.r - 1.0, 0.0);          // age the lock
    float lockLuma = lockPrev.g;

    // shading-change: current resolved luma disagrees with the luma captured when the lock was made.
    float shadingChange = abs(lumaCur - lockLuma) / max(lumaCur, max(lockLuma, 0.2));
    bool  breakLock = disoccluded || shadingChange > push.disoccl.y;
    if (breakLock) { lockLife = 0.0; }

    // (re)create a lock on a stable thin feature: strong neighbourhood luma contrast + low velocity.
    // `sigmaY` is the phase-2-core YCoCg luma std-dev already computed for the clip.
    bool thinFeature = (sigmaY > 0.15) && (velPx < 2.0);
    if (lockLife <= 0.0 && thinFeature) { lockLife = push.lockReactive.x; lockLuma = lumaCur; }

    bool locked = lockLife > 0.0;
    ```

14. **Apply the lock to the core blend** — do not add a new blend, scale the core inputs. Widen the
    variance clip and lift the feedback floor while locked (both already exist from core phase 3 / phase-2
    clip):

    ```hlsl
    // widen the YCoCg clip half-extent when locked so the thin feature is not clipped away.
    float gamma = push.gammaValid.x * (locked ? 1.75 : 1.0);   // feed this gamma into clip_aabb bounds
    // …build aabbMin/aabbMax with the widened gamma, clip history as core phase 2 does…

    // core phase-3 adaptive feedback is computed as before; lift its floor while locked.
    float feedbackFloor = locked ? max(push.feedback.x, 0.95) : push.feedback.x;
    feedback = max(feedback, valid ? feedbackFloor : 0.0);
    ```

15. **Reactive current-weight boost.** Sample the mask and pull the blend toward the current frame where
    translucent content lives (its motion vector is unreliable, so trusting history would ghost it). This
    modulates the core phase-3 `feedback` — a higher reactive value lowers history weight:

    ```hlsl
    float react = saturate(reactive.SampleLevel(uv, 0.0).r * push.lockReactive.y);
    feedback *= (1.0 - react);           // react=1 -> take current whole; react=0 -> unchanged
    // (reactive also suppresses lock creation implicitly: a reactive pixel's feedback drops, so
    //  the accumulation naturally favours current — no separate branch needed.)
    ```

16. **Disocclusion + gate into feedback**, then the core luma-weighted blend (unchanged), then the dual
    write **plus** the lock write:

    ```hlsl
    bool onScreen = all(histUv >= 0.0) && all(histUv <= 1.0);
    bool validHist = push.gammaValid.y >= 0.5 && onScreen && !disoccluded;
    feedback = validHist ? feedback : 0.0;   // disocclusion / first frame / off-screen -> current whole

    // …core phase-3 luma-weighted blend of (cur, clippedHist) with `feedback` → result…

    outColor[tid.xy]   = float4(result, 1.0);
    outHistory[tid.xy] = float4(result, 1.0);              // (Phase 2 owns history.a = accumulated weight)
    outLock[tid.xy]    = float4(lockLife, lockLuma, curLin, 0.0);  // roll the reconstruction state
    ```

    Update the file header comment to name the full spine (dilated reproject → Lanczos accumulate →
    lock-scaled YCoCg clip → disocclusion/shading gate → reactive luma-weighted blend → dual color/history
    + lock write) and bindings 6/7/8. No change-journey wording — describe what it does now.

## Edge cases & risks

- **Lock parity / write-back.** The lock pair ping-pongs on `history_index`; its two images carry layout
  across frames exactly like `history`, so each must ride an `alloc_external_layout` slot and be restored
  by the same write-back path (`writeback_history_layout`). A lock image left in `GENERAL` when the next
  frame samples it as `SHADER_READ_ONLY` is a validation error — the headless smoke catches it.
- **`history_valid` / resize.** `build_aa_targets` resets `history_valid = false` and rebuilds the lock
  pair fresh (lifetime 0 everywhere). The first post-resize / post-toggle frame takes current whole
  (`feedback = 0` via `gammaValid.y`), and locks re-accrue over the next few frames — no stale lock smear.
- **Lock over-hold (sticky ghosting).** Too long a `lock_lifetime` or too wide a locked `gamma` makes thin
  features *stick* through a real change. The shading-change break (`lock_break_luma`) and the
  disocclusion break are the safety valves; keep the default lifetime small (≈4 frames) and verify a thin
  feature that genuinely changes colour updates within a frame or two (no frozen speckle).
- **Reactive mask extent mismatch.** `reactive` is at **input** extent while the resolve dispatches at
  **display** extent; sampling it by `uv` (normalized) is correct across the ratio. Do not index it by
  `tid.xy`. Opaque regions must read `0` — the frame-start clear is load-bearing; a missing clear leaves
  last frame's coverage and makes opaque geometry falsely reactive (soft, under-accumulated look).
- **Reactive vs. lock interaction.** A translucent pixel should not also lock (its motion is unreliable).
  Because the reactive boost drives `feedback` down, `thinFeature` rarely triggers there anyway; if a
  glass edge still shimmers, gate lock creation on `react < 0.5` explicitly.
- **Depth sense (load-bearing).** `LinearizeDepth` and the `closestDepth` tap assume the motion-prepass
  `motion_depth` convention (near = min per core phase 3). A flipped sense inverts the disocclusion test —
  it would drop history on approach and keep it on reveal, the opposite of correct. Confirm against
  `motion.slang` / the projection when implementing.
- **Pre-tonemap domain (NO-COMPAT).** The lock luma, the shading-change test, and the blend all operate on
  linear-HDR values. Do not sneak a tonemap/exposure curve into any of them; the `1/(1+luma)` core
  weighting is the anti-flicker for this domain. Blending tonemapped values (or exposure-scaling only one
  of `current`/`history`) breaks accumulation.
- **Descriptor/layout atomicity.** Slots 6/7/8 must land in `create_taa_layout` (step 6),
  `write_aa_sets` (step 7), and the `add_taa_pass` accesses (step 9) together; a set written with a
  binding the layout lacks — or a declared access with no matching image — is a validation error.

## Ordering / dependencies

- **Depends on Phase 2** (`phase-2-resolution-aware-reconstruction.md`): the lock and disocclusion gate
  operate on the display-extent accumulator and the Phase-2 `screen_size`/upscale-ratio push seam; there
  is nothing to protect until reconstruction runs at display extent.
- **Depends on `plans/modern-taa-core/` phase 3**: the adaptive `feedback`, the `DilatedMotion` +
  `motionDepth` binding (slot 5), the YCoCg variance clip (`push.gammaValid.x`), and the `Luma` helper are
  reused — this phase scales and gates them, it does not redefine them.
- **Prerequisite for Phase 4** (`phase-4-sharpen-dynres-and-tooling.md`): the RCAS sharpen folds onto the
  now-robust display-extent result, and the `set-upscale` / editor-quality tooling exposes the tuning
  knobs (including `lock_lifetime`, `reactive_scale`, `disocclusion_threshold`, `lock_break_luma`) this
  phase adds to `TaaParams`. Phase 4 owns the control-plane exposure and the docs update; Phase 3 lands
  only the renderer-side state + seam so Phase 4's control work is a thin protocol change.

## Verification

The milestone gate plus a runtime proof appropriate to a rendering change (there is no pixel-diff harness
— verification is validation-clean smoke, green e2e, and a described visual check):

1. **Build + lint clean.** `just engine` (runs `xtask shaders`, recompiling `taa.slang` to SPIR-V) then
   `just prepare-for-commit` — `cargo clippy -D warnings` + `cargo fmt --check` + oxlint pass. A Slang
   error (bad binding, push size) fails the build. Confirm `push_layouts_match_shaders` asserts the new
   `size_of::<TaaPush>()` (phase-2 size + 24 bytes) and that the layout/set/accesses agree on slots 6–8.

2. **No wire type changed → no protocol regen.** This phase touches no DTO; the control-plane exposure of
   the new `TaaParams` knobs is Phase 4. `cargo run -p xtask -- gen-protocol` is **not** needed here. (If
   an implementer chooses to land the control field early, that is Phase 4's scope — do not split it.)

3. **`just e2e` stays green.** The suite boots a headless host and drives it over the control plane; a
   validation error from the new binding/push/attachment would surface as a dirty log the harness rejects.

4. **Validation-clean headless smoke at render scale < 1.** `just run-engine-headless 30` with the view's
   `render_scale` below 1 (sub-native input, display-extent resolve) boots and exits with **zero**
   validation-layer errors — specifically no descriptor set/layout mismatch on slots 6/7/8, no
   push-constant-range warning for the enlarged push, no layout-transition error on the lock ping-pong,
   and no attachment mismatch from the reactive MRT. The 30-frame count lets locks accrue and the
   disocclusion history settle so the new paths actually execute.

5. **Visual check via `just run-engine` (or the editor with TAA + render scale < 1).**
   - **Translucent content does not ghost.** Pan the camera past alpha-blended geometry (glass, foliage
     cards, particles): the trailing smear a motion-vector-only resolve leaves is gone (confirms the
     reactive mask raises current weight where motion is unreliable).
   - **Thin features stay stable under motion.** Slowly orbit a scene with thin high-frequency detail
     (wires, railings, foliage edges): they hold steady instead of shimmering/dropping out (confirms the
     lock widens the clip + lifts the feedback floor), and they do **not** freeze through a real colour
     change (confirms the shading-change break).
   - **Disocclusions resolve cleanly.** Move a foreground object across a busy background: the newly
     revealed background converges within a couple of frames with no dragged-history halo (confirms the
     parallax disocclusion gate forcing `feedback = 0`), while static regions stay rock-steady (the gate
     does not fire on camera-only motion).
   - **No exposure/flicker regression.** Bright specular highlights in motion do not flicker and overall
     brightness is unchanged versus Phase 2 (confirms the resolve stayed pre-tonemap in one linear domain).

6. **Milestone gate at the phase boundary.** `just engine` + `just prepare-for-commit`; leave all changes
   unstaged and report — git is read-only by default; the user stages and commits.

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` (+ `xtask shaders`) + `just prepare-for-commit`
  (cargo fmt + clippy `-D warnings` + oxlint); `just e2e` stays green; a validation-clean headless smoke at
  render scale < 1.
- **NO-LEGACY:** one resolve path. The lock lives only in `lock[2]`, the reactive mask has one producer
  (the translucent scope) and one binding, and the disocclusion/shading gate feeds the single core
  `feedback` — no parallel blend, no "ratio==1 skips it" fork, no duplicate rejection of the variance-clip
  kind. Slots 6/7/8 land atomically across layout, set writes, and pass accesses.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits.
