# Phase 1 — Sub-pixel jitter + velocity un-jitter

**Status:** COMPLETED

Part of the `plans/modern-taa-core/` feature (modern native-resolution TAA). This is the foundational
phase: it makes sub-pixel camera jitter exist and be reprojection-safe. Nothing in the later phases can
actually anti-alias a still image without it — with no jitter the accumulated frames are identical and
the resolve only softens motion.

## Goal

Inject a per-frame Halton(2,3) sub-pixel offset into the projection that reaches
`scene_draw_list.view_proj`, store the current and previous jitter offsets per-view on `ViewTarget`
(rolled in lockstep with the existing previous-matrix state), thread the current offset into
`render_scene` through a `SceneRenderer` seam, and keep the motion prepass **jitter-free** so
`motion.slang` velocity stays exact (static geometry keeps zero motion). Apply jitter only when TAA is
the active AA mode. At the end of this phase a still image with a hard edge shows anti-aliased steps
instead of a raw stair.

## NO-LEGACY checklist for this phase

- The scene projection reaching the vertex push is jittered whenever `aa_mode() == "taa"`; the
  un-jittered projection is *only* what cluster/SSAO cameras and picking rays consume. There is not a
  second "TAA on/off jitter" code path beyond the single `aa_mode()` gate.
- The motion prepass keeps receiving the un-jittered `view_proj` — no jitter leaks into `MotionPush`, so
  `motion.slang` is untouched and velocity stays `prevUv - curUv`.
- Jitter offsets live in exactly one place (`ViewTarget`, beside `prev_view_proj`) and are advanced at
  exactly one site (the `store_prev_view_proj` / `flip_history` tail of the frame). No parallel
  frame-counter is introduced elsewhere.

## Engine crate: `saffron-rendering`

### Halton generator + jitter offset

**File `engine/crates/rendering/src/aa.rs`.**

1. Add a small pure Halton helper and the phase count as module constants next to `TAA_HISTORY_WEIGHT`:

   ```rust
   /// Number of jitter phases in the Halton(2,3) cycle. 8 is the balanced native-res default
   /// (16 is fine; the follow-on upsampling set scales this with the upscale ratio).
   pub const TAA_JITTER_PHASES: u32 = 8;

   /// Radical-inverse Halton sample in [0,1) for index `i` (1-based) in base `b`.
   fn halton(mut i: u32, b: u32) -> f32 { /* f/=b; r += f*(i%b); i/=b; loop */ }

   /// Sub-pixel jitter offset in NDC for a frame index and render extent: `(2*halton-1)/dim`,
   /// range ~±0.5 px (1 px == 2/dim in NDC).
   pub fn jitter_offset(index: u32, width: u32, height: u32) -> glam::Vec2 { /* … */ }
   ```

   `jitter_offset` uses `halton(index + 1, 2)` / `halton(index + 1, 3)` (1-based so phase 0 is not the
   zero sample), maps each to `[-1, 1]`, and divides by the corresponding dimension. Unit-test `halton`
   against the known first few base-2 values (`0.5, 0.25, 0.75, …`) and base-3 (`1/3, 2/3, 1/9, …`).

### Per-view jitter state on `ViewTarget`

**File `engine/crates/rendering/src/view_target.rs`.**

2. Add three fields beside `prev_view_proj` / `prev_view_proj_valid` (which document the per-view
   rationale — a re-activated view reprojects against its own last frame; jitter has the same lifetime):

   ```rust
   /// The Halton phase index advanced once per rendered frame while TAA is active.
   pub jitter_index: u32,
   /// This frame's sub-pixel jitter offset (NDC), baked into the scene projection.
   pub jitter: saffron_geometry::glam::Vec2,
   /// Last frame's jitter offset (NDC); needed to un-jitter the reprojected sample in the resolve.
   pub prev_jitter: saffron_geometry::glam::Vec2,
   ```

   Initialize them to `0` / `Vec2::ZERO` in `ViewTarget::new`.

3. In `build_aa_targets`, reset the jitter state next to the existing
   `self.prev_view_proj_valid = false;` (a mode change / resize restarts the sequence):
   `self.jitter_index = 0; self.jitter = Vec2::ZERO; self.prev_jitter = Vec2::ZERO;`.

4. Add an `advance_jitter` method that mirrors `store_prev_view_proj`: roll `prev_jitter = jitter`,
   increment `jitter_index` modulo `TAA_JITTER_PHASES`, and recompute `jitter` from
   `crate::jitter_offset(self.jitter_index, extent.width, extent.height)` using `self.extent()`:

   ```rust
   pub fn advance_jitter(&mut self) {
       self.prev_jitter = self.jitter;
       self.jitter_index = (self.jitter_index + 1) % crate::TAA_JITTER_PHASES;
       let e = self.extent();
       self.jitter = crate::jitter_offset(self.jitter_index, e.width, e.height);
   }
   ```

   > `advance_jitter` computes the offset for the *next* frame's index at the end of this frame, matching
   > how `store_prev_view_proj` records this frame's matrix as next frame's "previous". The offset
   > `render_scene` reads at the top of a frame is therefore the one produced by the previous frame's
   > `advance_jitter` (or the `build_aa_targets` reset on the first frame). Confirm the phase-0 offset is
   > applied on frame 1 (either seed `jitter` in `build_aa_targets` from index 0, or call `advance_jitter`
   > before the first `render_scene` read — pick one and document it inline).

### Advancing jitter once per frame + gating on TAA

**File `engine/crates/rendering/src/renderer.rs`.**

5. At the frame tail where `view.flip_history()` and `view.store_prev_view_proj(frame_view_proj)` already
   run (the `store_prev_view_proj` call site), call `view.advance_jitter()` **only when TAA is active**
   (`self.aa.taa()`). When TAA is off, jitter stays `Vec2::ZERO` so the scene renders un-jittered — this
   is the single gate; there is no separate jitter toggle.

6. Expose the current offset for the scene driver: add
   `pub fn active_view_jitter(&self) -> glam::Vec2` returning `self.active_view().jitter` when
   `self.aa.taa()` else `Vec2::ZERO` (so a mode flip mid-frame can never leak a stale offset).

## Engine crate: `saffron-assets`

### The `SceneRenderer` seam

**File `engine/crates/assets/src/render_scene.rs`.**

7. Add a getter to `trait SceneRenderer` next to `viewport_width` / `viewport_height`:

   ```rust
   /// This frame's sub-pixel TAA jitter offset in NDC (zero when TAA is inactive).
   fn jitter_offset(&self) -> saffron_geometry::glam::Vec2;
   ```

   Implement it on `RendererScene` (`impl SceneRenderer for RendererScene`) by delegating to
   `self.renderer.active_view_jitter()`, exactly as `viewport_width` delegates to `active_view().extent()`.
   Add a `Vec2::ZERO` stub to the in-file test `SceneRenderer` implementation.

### The jitter insertion point

8. In `render_scene`, immediately after `proj.y_axis.y *= -1.0;` and **before** `let view_projection =
   proj * view;`, build a jittered copy of the projection for the scene draw only:

   ```rust
   let jitter = renderer.jitter_offset();
   let mut jittered = proj;
   // Clip-space translation: adds jitter * w_clip == a sub-pixel shift. `width`/`height` are already
   // the locals from viewport_width()/viewport_height() above; jitter is already in NDC.
   jittered.z_axis.x += jitter.x;
   jittered.z_axis.y += jitter.y;
   let view_projection = jittered * view;
   ```

   > Use exactly one jitter convention. `jitter_offset` returns the NDC offset (`(2*halton-1)/dim`), so
   > the projection edit adds it directly to the frustum-skew terms `z_axis.x/.y`; do **not** also
   > pre-multiply a translation matrix. Verify against the matrix `motion.slang` sees: the scene sees
   > `jittered`, the motion pass must see the plain `proj`.

9. Leave the un-jittered `proj` as the value handed to `renderer.set_cluster_camera(ClusterCamera {
   projection: proj, .. })` and `renderer.set_ssao_camera(view, proj, light_dir)` later in `render_scene`
   — clustered lighting and SSAO must not see jitter. Likewise `viewport_ray` / `pick_entity` rebuild
   their own `camera_projection` and stay un-jittered. Only `view_projection` (the vertex push) carries
   the offset.

## Keeping motion un-jittered (the load-bearing constraint)

**File `engine/crates/rendering/src/renderer.rs`.**

10. `scene_draw_list.view_proj` now carries jitter (it is fed the jittered `view_projection`). The motion
    prepass must **not**. In `add_motion_pass`, `MotionPush.cur_view_proj` and `MotionPush.prev_view_proj`
    are currently both built from `scene_draw_list.view_proj` / `view.prev_view_proj`. Change these to use
    the **un-jittered** matrices:

    - Store the un-jittered projection per-frame so the motion pass can read it. The simplest correct
      wiring given the current code: keep `scene_draw_list.view_proj` jittered for the scene/depth passes,
      and add a sibling `scene_draw_list.view_proj_unjittered` (set in `submit_draw_list_skinned` from a
      new `DrawListInputs` field) that `add_motion_pass` reads for `cur_view_proj`. Its previous-frame
      counterpart is what `store_prev_view_proj` must persist — see step 11.

11. At the `store_prev_view_proj` call site, persist the **un-jittered** matrix as `frame_view_proj` (so
    `view.prev_view_proj` used next frame by `add_motion_pass` is jitter-free). Today `frame_view_proj =
    self.scene_draw_list.view_proj`; change it to the un-jittered sibling from step 10. Net effect:
    `motion.slang`'s `pc.curViewProj` and `pc.prevViewProj` are both jitter-free, `motionUv` is unchanged,
    and static geometry (whose `prevModel == model` and shared static vertex streams) keeps exact zero
    velocity — the shader file `motion.slang` needs no edit.

    > Equivalent alternative to weigh when implementing: keep `scene_draw_list.view_proj` un-jittered and
    > apply the jitter as a separate clip-space push only in the scene vertex path. Do **not** do both.
    > Whichever seam you pick, the invariant is: scene vertices are jittered, `MotionPush` is not, and the
    > stored previous matrix is the un-jittered one. Pick one and delete the other option — no dual path.

## The resolve reads jitter (setup for Phase 2)

The resolve shader `taa.slang` does not need jitter to *anti-alias* (jitter is baked into the samples it
already reads), but it will need `cur_jitter` / `prev_jitter` in Phase 3 to un-jitter the reprojected
sample precisely. This phase does **not** change `taa.slang` or `TaaPush`. It only guarantees the offsets
are stored per-view (`view.jitter` / `view.prev_jitter`) so a later phase can push them. Note this
explicitly so no one adds jitter fields to `TaaPush` prematurely.

## `sa` CLI + docs

- No new engine state worth a dedicated command lands here — jitter is internal and non-tunable in Phase
  1 (the phase count is a constant). The `sa` TAA-tune command arrives in Phase 4. No `sa` change now.
- Docs: the full spine rewrite of `docs/content/explanations/screen-space-and-post/taa.md` is Phase 4;
  do not partially rewrite it now. (If you touch anything, only note in that page's future edit that
  jitter is what makes the resolve anti-alias — but prefer to defer the whole docs pass to Phase 4.)

## Ordering / dependencies

Depends on nothing (see the README phase table — this is the foundational phase). It is the prerequisite
for every later phase: `phase-2-history-reconstruction.md` needs jitter to exist before its Catmull-Rom
reconstruction and YCoCg variance clip have a multi-sample signal to converge; `phase-3-robust-blend.md`
consumes the per-view `jitter` / `prev_jitter` this phase stores to un-jitter the reprojected sample in
its expanded `TaaPush`; and `phase-4-sharpen-and-tooling.md` exposes and documents the parameters that
grow out of the Phase 3 blend. This phase touches no wire type, no descriptor set, and no `taa.slang` —
those changes all live downstream.

## Verification

Run the milestone gate and confirm each item:

1. **Build + lint clean.** `just engine` then `just prepare-for-commit` — clippy `-D warnings` and oxlint
   pass. No protocol change this phase (no wire type touched).

2. **Halton unit tests.** The `halton` base-2/base-3 sequences match the known radical-inverse values,
   and `jitter_offset` stays within `±1/dim` on each axis. Add to the `aa.rs` tests module.

3. **Motion stays exact (the critical proof).** With a static scene and a *still* camera under TAA,
   sample the motion buffer (or add a temporary assert in an e2e/headless run): velocity must be ~zero
   everywhere despite the jittered scene projection — proving the un-jitter in `add_motion_pass` /
   `store_prev_view_proj` holds. `just e2e` stays green (no motion-vector regression).

4. **Headless smoke.** `just run-engine-headless 8` (at least one full jitter cycle) boots
   validation-layer-clean under TAA.

5. **Visual check.** `just run-engine` with TAA selected: a still image with a high-contrast diagonal
   edge shows anti-aliased steps that were absent before (jitter is doing its job). Panning the camera
   still shows the old smear/ghost — that is expected and is what Phases 2–3 fix; note it, do not try to
   fix it here.

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` + `just prepare-for-commit`; `just e2e`.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits.
