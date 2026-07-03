# Temporal upsampling (TAAU)

**Status:** COMPLETED

Extend the modern TAA resolve into a temporal **upsampler** (TAAU, FSR2/TSR-class): render the scene,
depth, and motion at a lower **input** resolution and reconstruct a sharp **display**-resolution image,
trading a little quality for a large performance win. This set **builds directly on** the
`plans/modern-taa-core/` spine (jitter, Catmull-Rom history reconstruction, YCoCg variance clip,
luma/velocity-adaptive weighting) and assumes it is **COMPLETED first**. It does not re-specify the core
resolve; it specifies only the *upsampling delta* — resolution decoupling, resolution-aware
reconstruction, locks/reactive rejection, RCAS-for-upscale, and the dynamic-resolution hook.

## Why

The core resolve produced by `plans/modern-taa-core/` runs at a single resolution: `add_taa_pass`
(`engine/crates/rendering/src/renderer.rs`) reads the render-extent scene color (`view.scratch`) plus
`view.motion`, and writes the render-extent offscreen color plus its two `history[2]` — every image in
`ViewTarget::build_aa_targets` (`engine/crates/rendering/src/view_target.rs`) is sized from the single
`self.offscreen.extent` surfaced by `ViewTarget::extent()`. The only place input and output extent
already differ is the terminal present/capture blit `Renderer::record_shm_copy`
(`engine/crates/rendering/src/renderer.rs`), which rescales `render_extent` → `published_extent()` with a
dumb `vk::Filter::LINEAR` `vkCmdBlitImage`. Nothing between the scene and that blit knows the display
extent — so a project running below native resolution (`ViewTarget::render_scale` < 1, via
`scaled_render_extent()`) gets a *bilinear stretch* of an aliased low-res frame at the very end.

That is exactly the pattern a temporal upsampler replaces. The core resolve already **is** the TAAU
spine minus the upscale: `computeMain` in `engine/assets/shaders/taa.slang` reprojects (`histUv = uv +
mv`), reconstructs history (Catmull-Rom from `plans/modern-taa-core/` phase 2), rejects via the YCoCg
variance clip (phase 2) and the velocity/luma-adaptive feedback (phase 3), and dual-writes color +
next-frame history. The upsampler is *that same resolve* run on a **display-extent grid** over
**input-extent** scene inputs, with the current-frame samples resampled up as they accumulate — plus the
robustness (locks, reactive mask) and the sharpen (RCAS) that low input resolution demands. The engine
already carries the two extent classes (`scaled_render_extent()` vs `published_extent()`) and a
dynamic-resolution budget hook (`BudgetStep::Scale` → `pending_render_scale` in
`Renderer::render`) — today they only feed the final blit; this set wires them through the resolve.

**NO-COMPAT:** when input extent < display extent, the upsampler **is** the resolve path. The final
LINEAR stretch blit for low render-scale is deleted, not kept beside it; there is no "native resolve for
scale 1.0, upscaler for scale < 1.0 sitting on separate code" — one resolve spine, dispatched at display
extent, that degenerates to the core native-res resolve exactly when input extent == display extent.

## Goal

- **Decoupled extents.** Scene color, depth, and motion render at **input** extent
  (`scaled_render_extent()`); history, the resolve output, tonemap, overlays, and the present source live
  at **display** extent (`published_extent()`). The terminal LINEAR upscale blit is gone — present is 1:1.
- **Resolution-aware reconstruction.** The Halton jitter phase count scales with the upscale ratio
  (`ceil(8·n²)`), and the resolve resamples the input-extent current color into the display-extent
  accumulator with a Lanczos/bicubic kernel (FSR2-style reproject-and-accumulate), tracking accumulated
  sample weight per output pixel.
- **Robust reconstruction.** Pixel **locks** protect stable thin features; a **reactive mask** raises
  current-frame weight on alpha-blended/translucent surfaces; shading-change + disocclusion rejection and
  a consistent linear/exposure accumulation domain keep history clean.
- **Sharpen + dynamic resolution + tooling.** An RCAS contrast-adaptive sharpen tuned for upscale runs at
  display extent after the resolve; the existing frame-budget hook drives input extent to a budget; a
  `set-upscale` / `get-upscale` control command pair, an editor quality control, and docs land with it.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-resolution-decoupling.md` | Split the single per-view extent into **input** vs **display** classes: render scene/depth/motion + the whole G-buffer/SSGI/DFAO chain at `scaled_render_extent()`, size TAA `history[2]` + a new display-extent resolve output + tonemap + overlays at `published_extent()`, and make `add_taa_pass` read input-extent inputs while dispatching over and writing the display grid (`texSize` seam from `plans/modern-taa-core/` phase 2). Recreate the two extent classes independently in `apply_render_extent`; the terminal upscale blit in `record_shm_copy` becomes 1:1. NO-COMPAT: this replaces the LINEAR-stretch path. | `plans/modern-taa-core/` COMPLETED |
| 2 | `phase-2-resolution-aware-reconstruction.md` | Make reconstruction upscale-aware: scale the `TAA_JITTER_PHASES` count by the upscale ratio (`ceil(8·n²)`) in `aa.rs`, resample the input-extent `current` into the display grid with a Lanczos/bicubic kernel in `taa.slang`, and track per-output accumulated sample weight / linear feedback so freshly-covered display pixels converge. Carries the upscale ratio into `TaaPush` (screen_size / input extent). | Phase 1 |
| 3 | `phase-3-robust-reconstruction.md` | Add robustness the low input res demands: FSR2-style **locks** protecting stable thin features (extra history channel), a `[0..1]` **reactive mask** input (new TAA set binding) raising current weight on translucent/alpha content, shading-change + parallax disocclusion rejection layered on the phase-3 core feedback, and consistent linear/exposure handling. | Phase 2 |
| 4 | `phase-4-sharpen-dynres-and-tooling.md` | Fold an RCAS-style contrast-adaptive sharpen (tuned for upscale) into `computeMain` at display extent after the resolve; wire the existing `BudgetStep::Scale` → `pending_render_scale` frame-budget hook to drive input extent; add a `get-upscale`/`set-upscale` control command pair (ratio + dynamic-resolution toggle) across every protocol seam; add the editor quality control; and update docs. | Phase 3 |

The chain is strict: Phase 1 is the architectural prerequisite (nothing reconstructs at display extent
until the extents split); Phase 2 fills the extra display samples the decoupling created; Phase 3 makes
those samples robust; Phase 4 sharpens, drives, and exposes them.

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| The resolve pass that becomes the upsampler (reads input, writes display) | `engine/crates/rendering/src/renderer.rs` | `add_taa_pass`, `add_tonemap_pass`, `add_grid_overlay_passes`, `add_compute_pass` |
| The single per-view extent + the two extent classes already present | `engine/crates/rendering/src/view_target.rs` | `ViewTarget::extent`, `scaled_render_extent`, `published_extent`, `render_scale`, `desired_width`/`desired_height` |
| Per-view target allocation keyed to the one extent (must split) | `engine/crates/rendering/src/view_target.rs` | `ViewTarget::new`, `resize`, `build_screen_space`, `build_aa_targets`, `history[2]`, `history_index`, `flip_history`, `history_valid`, `scratch`, `offscreen` |
| Sizing entry point that recreates all targets from one extent | `engine/crates/rendering/src/renderer.rs` | `apply_render_extent`, `set_viewport_desired_size`, `set_render_scale` |
| The only current input≠output split: terminal LINEAR upscale blit | `engine/crates/rendering/src/renderer.rs` | `record_shm_copy`, `publish_extent`, `vk::Filter::LINEAR` |
| Dynamic-resolution frame-budget hook (feeds render scale today) | `engine/crates/rendering/src/renderer.rs`, `budget.rs` | `BudgetController`, `BudgetStep::Scale`, `pending_render_scale`, `set_render_quality` |
| The resolve shader + its push/bindings | `engine/assets/shaders/taa.slang` | `computeMain`, `Push`, bindings `0..4` (`current`, `history`, `motion`, `outColor`, `outHistory`) |
| Jitter + core push/params (from `plans/modern-taa-core/`) | `engine/crates/rendering/src/aa.rs` | `TAA_JITTER_PHASES`, `TaaPush`, `TaaParams`, `MotionPush`, `MOTION_FORMAT`, `Aa` |
| Descriptor layouts + per-view set writes (new reactive/lock bindings) | `engine/crates/rendering/src/descriptors.rs`, `view_target.rs` | `create_taa_layout`, `view.taa_sets`, `taa_history_view` |
| Jitter phase count / un-jitter seam (phase count scales with ratio) | `engine/crates/assets/src/render_scene.rs`, `view_target.rs` | `render_scene`, `SceneRenderer::jitter_offset`, `ViewTarget::advance_jitter`, `jitter`/`prev_jitter` |
| Control-command registration pattern for `set-upscale` | `engine/crates/control/src/{commands_render.rs,registry.rs}`, `protocol/src/{dto.rs,command.rs,codegen.rs}`, `host/src/control_renderer.rs` | `register_render_commands`, `ControlRenderer`, `DTO_TYPE_NAMES`, `COMMANDS`, `COMMAND_FIXTURES` |
| Docs to extend | `docs/content/explanations/screen-space-and-post/` , `.../anti-aliasing/` , `.../render-quality-tiers.md` | `taa.md`, `_index.md`, `aa-modes.md`, `render-quality-tiers.md` |

## Ground rules

- **Prerequisite.** `plans/modern-taa-core/` must be COMPLETED first. This set reuses its resolve spine
  verbatim — jitter (`TAA_JITTER_PHASES`, `advance_jitter`, un-jittered motion), Catmull-Rom history
  reconstruction, YCoCg variance clip, luma/velocity-adaptive feedback, the expanded `TaaPush`, and the
  `TaaParams`/`set-taa-params` tooling. Do **not** re-specify any of it; specify only the upsampling delta.
- **One resolve path (NO-COMPAT).** The upsampler replaces the native-res resolve as *the* path when
  input extent < display extent, and degenerates to the core native-res resolve when they are equal. Never
  a parallel duplicate, never a "scale==1 uses the old path" fork, never a feature flag toggling
  old-vs-new. The terminal LINEAR upscale blit for low render-scale is **deleted** in Phase 1, not kept
  beside the resolve. A phase is not done while a superseded path survives in the tree.
- **Accumulate in the same linear-HDR domain the scene rendered in.** The resolve stays pre-tonemap; the
  input-extent color is Lanczos-resampled into a linear-HDR display-extent accumulator, and tonemap +
  overlays run *after*, at display extent. Never blend tonemapped values.
- **Keep-current.** Where a phase adds tunable/inspectable engine state (the upscale ratio, the
  dynamic-resolution toggle), it adds a matching `sa` control command (one registration in
  `saffron-control`, threaded through every protocol seam and regenerated with `xtask gen-protocol`); where
  it adds/alters a concept, it updates the matching `docs/` page and its hub `_index.md` row in the same
  change.
- **Each phase ends on the milestone gate.** `just engine` + `just prepare-for-commit` (cargo fmt +
  clippy `-D warnings` + oxlint), protocol regenerated where a wire type changed, and `just e2e` stays
  green. Runtime proof for a rendering change is: a validation-layer-clean headless smoke
  (`just run-engine-headless` with `SAFFRON_EXIT_AFTER_FRAMES`) at a render scale < 1, plus a **described
  visual check** via `just run-engine` / the editor (sub-native input reconstructing to a sharp display
  image, no ghosting on motion, no shimmer on thin features). There is **no** pixel-diff test harness — do
  not invent one.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).

### References / sources

Temporal upsampling primary sources (put alongside the `plans/modern-taa-core/` core-resolve references):
AMD GPUOpen **FSR2/FSR3** manuals (the closest open reference: reproject-and-accumulate, locks, reactive
mask, `ceil(8·n²)` jitter phases, RCAS); Epic **UE5 TSR** docs (display-res history, shading rejection,
history resurrection, screen-percentage); AMD **FidelityFX CAS/RCAS** (contrast-adaptive sharpen);
id Tech 7 TSSAA (Tiago Sousa); Guerrilla **Decima** temporal upsampling (Advances in Lighting and AA);
Bart Wronski *temporal supersampling*; Yang et al. 2020 *Survey of Temporal Antialiasing Techniques*
(CGF). DLSS/XeSS/PSSR are the ML analogues (same jittered-color + motion + depth inputs) and are out of
scope for a from-scratch engine.

Detail lives in the phase files; this page is the index.
