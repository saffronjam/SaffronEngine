# Modern native-resolution TAA

**Status:** COMPLETED

Bring the naive TAA resolve up to a modern, balanced (quality vs performance) native-resolution
temporal anti-aliaser: the state-of-the-art spine of sub-pixel jitter, Catmull-Rom history
reconstruction, YCoCg variance clipping, tonemap/luma sample weighting, velocity-adaptive feedback,
dilated motion vectors, disocclusion rejection, and an optional sharpen. This is the **foundational**
set — nothing precedes it, and it replaces the current resolve in place (there is exactly one resolve
path). The later `taa-upsampling` set builds directly on this spine, so every piece is written to
survive a resolve that later runs at display resolution over an input-resolution source.

## Why

The resolve that ships today (`computeMain` in `engine/assets/shaders/taa.slang`) is the textbook
*naive* TAA, and every naive weakness is present at once:

- **No sub-pixel jitter anywhere.** The projection built in `render_scene`
  (`engine/crates/assets/src/render_scene.rs`) applies only the Vulkan Y-flip
  (`proj.y_axis.y *= -1.0`) — there is no per-frame Halton offset. A grep for camera jitter finds only
  unrelated ray-march/SDF jitter. Consequence: on a **still image TAA anti-aliases nothing** — with no
  jitter the accumulated frames are identical, so the resolve is all cost and no benefit; its only
  visible effect is softening motion.
- **Bilinear history fetch.** `history.SampleLevel(histUv, 0.0)` is a single bilinear tap. Resampling
  the history every frame inside a `~0.9` feedback loop is the dominant source of the motion smear.
- **Raw RGB min/max clamp.** The neighborhood rejection is a loosest-possible `clamp(hist, nmin, nmax)`
  over a 3×3 box in **linear HDR RGB** (TAA runs before tonemap). The box blows up around highlights, so
  rejection is weak, and clamping in RGB produces chroma fringing (the purple-fringe artifact).
- **A fixed feedback weight.** `TAA_HISTORY_WEIGHT = 0.9` in `engine/crates/rendering/src/aa.rs` is a
  compile-time constant pushed through `TaaPush.params.x`, with no adaptation to velocity, luma stability,
  or disocclusion beyond the on-screen bounds check in the shader.
- **Undilated velocity, no shading-change rejection, no sharpen.** Motion is sampled at the pixel center
  only (silhouette ghosting), and history is trusted whenever it reprojects on-screen.

The motion vectors themselves are already **correct** and must not be disturbed: `motion.slang` outputs
`motionUv = (prevNdc - curNdc) * 0.5 = prevUv - curUv`, and the resolve reads history at
`histUv = uv + mv = prevUv`. The per-view previous matrix (`ViewTarget::prev_view_proj` /
`store_prev_view_proj`) and history ping-pong (`history_index` / `history_valid` / `flip_history` in
`engine/crates/rendering/src/view_target.rs`) are the right scaffolding — this set builds on them rather
than replacing them.

## Goal

- **A still image finally anti-aliases.** Sub-pixel Halton jitter feeds the temporal accumulation, and
  velocity is un-jittered so static geometry keeps exact zero motion.
- **Sharp history.** Catmull-Rom history reconstruction replaces the bilinear tap, killing the resample
  smear.
- **Clean rejection.** YCoCg variance clipping (clip toward the neighborhood mean, `μ ± γσ`) replaces
  the raw RGB min/max clamp, removing ghosting and chroma fringing.
- **Stable, robust blend.** Tonemap/luma sample weighting tames fireflies in the pre-tonemap HDR
  resolve; a velocity- and luma-adaptive feedback weight replaces the fixed `0.9`; dilated (closest-depth)
  motion vectors and disocclusion/shading-change rejection cut silhouette ghosting.
- **Optional sharpen + tooling.** An RCAS-style post-resolve sharpen (configurable), a `sa` control
  command to inspect and tune the TAA parameters at runtime, and the docs updated to match.
- **A reusable spine.** The resolve stays structured so `taa-upsampling` can later dispatch it over a
  display-resolution grid reading input-resolution sources — no assumption baked in that input extent ==
  output extent.
- **One resolve path.** No fixed-`0.9`, bilinear, RGB-min/max fallback survives anywhere; no flag toggles
  old-vs-new.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-jitter-and-unjitter.md` | Sub-pixel Halton(2,3) jitter injected into the projection in `render_scene`, a per-view `jitter_index` / `cur_jitter` / `prev_jitter` on `ViewTarget` rolled at the `store_prev_view_proj` site, a `SceneRenderer::jitter_offset` seam, and velocity un-jitter so `motion.slang` velocity stays exact. Gated on `aa_mode() == "taa"`. The enabler: after this a still image anti-aliases. | — |
| 2 | `phase-2-history-reconstruction.md` | In `taa.slang`, replace the bilinear history fetch with an optimized Catmull-Rom reconstruction, and replace the raw RGB 3×3 min/max clamp with YCoCg variance clipping (`clip_aabb` toward `μ ± γσ`, `γ ≈ 1.0`). Kills the motion smear and the purple fringe. | Phase 1 |
| 3 | `phase-3-robust-blend.md` | Tonemap/luma sample weighting (`1/(1+luma)`) to stabilise the pre-tonemap HDR resolve; a velocity- and luma-adaptive feedback weight replacing `TAA_HISTORY_WEIGHT`, threaded through an expanded `TaaPush`; dilated closest-depth motion sampling (needs `motion_depth` bound into the resolve set); and shading-change/disocclusion history rejection beyond the on-screen bounds check. | Phase 2 |
| 4 | `phase-4-sharpen-and-tooling.md` | Optional RCAS-style sharpen folded into the resolve (configurable strength); a `get-taa-params` / `set-taa-params` control command (DTOs, codegen, `ControlRenderer` seam, live impl, `sa` CLI) making the now-runtime TAA parameters inspectable and tunable; and the `docs/` anti-aliasing/TAA pages updated to the modern spine. | Phase 3 |

The chain is strict: Phase 1 makes jitter exist (without it the neighborhood-based rejection in Phase 2
has nothing to converge); Phase 2 gives a sharp, well-bounded history the Phase 3 adaptive blend can
trust; Phase 3 makes the feedback weight a real runtime value that Phase 4's command exposes and tunes.

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| The naive resolve to be rebuilt (reproject → RGB clamp → fixed-weight lerp → dual write) | `engine/assets/shaders/taa.slang` | `computeMain`, `Push.params`, bindings `0..4` (`current`/`history`/`motion`/`outColor`/`outHistory`) |
| Motion vectors (correct; must stay un-jittered) | `engine/assets/shaders/motion.slang` | `vertexMain`, `fragmentMain`, `motionUv`, `Push.curViewProj`/`prevViewProj` |
| Fixed weight + push + format + mode selector | `engine/crates/rendering/src/aa.rs` | `TAA_HISTORY_WEIGHT`, `MOTION_FORMAT`, `TaaPush`, `MotionPush`, `Aa::set`/`set_mode`/`mode`/`taa`/`fxaa`/`msaa` |
| Where the projection is built (jitter insertion point) | `engine/crates/assets/src/render_scene.rs` | `render_scene`, `camera_projection`, `proj.y_axis.y *= -1.0`, `view_projection`, `viewport_width`/`viewport_height`, `trait SceneRenderer`, `set_cluster_camera`/`set_ssao_camera` |
| Per-view temporal state + ping-pong + extents | `engine/crates/rendering/src/view_target.rs` | `prev_view_proj`, `prev_view_proj_valid`, `store_prev_view_proj`, `flip_history`, `history`, `history_index`, `history_valid`, `build_aa_targets`, `extent`, `published_extent`, `scaled_render_extent` |
| Pass wiring, dispatch, PSO select, prev-matrix store | `engine/crates/rendering/src/renderer.rs` | `render`, `add_motion_pass`, `add_taa_pass`, `add_fxaa_pass`, `add_tonemap_pass`, `add_grid_overlay_passes`, `store_prev_view_proj` call site, `aa_mode`, `set_aa`, `reset_view_temporal`, `scene_draw_list.view_proj` |
| TAA descriptor set + binding→image writes | `engine/crates/rendering/src/view_target.rs`, `descriptors.rs` | `taa_sets`, `taa_history_view`, `create_taa_layout` |
| Control-command wiring for a TAA-tune command | `engine/crates/protocol/src/dto.rs`, `command.rs`, `codegen.rs`; `engine/crates/control/src/{commands_render.rs,registry.rs,test_support.rs}`; `engine/crates/host/src/control_renderer.rs` | `SetAaParams`/`SetAaResult` precedent, `COMMANDS`, `DTO_TYPE_NAMES`, `register_render_commands`, `trait ControlRenderer`, `StubRenderer` |
| Docs pages to update | `docs/content/explanations/screen-space-and-post/taa.md`, `.../screen-space-and-post/_index.md`, `.../anti-aliasing/aa-modes.md` | page body + hub rows, "In the code" tables |

## Ground rules

- **One resolve path (NO-LEGACY / NO-COMPAT).** Each phase *replaces* the naive piece in place. No
  fixed-`0.9`, bilinear, or RGB-min/max path survives after its phase lands; no feature flag toggles
  old-vs-new resolve; nothing is left "additive for now". A phase is not done while the superseded piece
  still exists in the tree.
- **Motion stays correct.** `motion.slang`'s `pc.curViewProj` / `pc.prevViewProj` must remain jitter-free
  (Phase 1). Any velocity change (dilation in Phase 3) must preserve the `motionUv = prevUv - curUv`
  convention and static-geometry zero-motion guarantee.
- **Resolve stays pre-tonemap and upsampling-ready.** Accumulate in the linear-HDR domain the scene
  renders in (the resolve runs before `add_tonemap_pass`), and keep the input-extent reads distinct in
  intent from the output-extent write/history so `taa-upsampling` can move the output to display extent
  without a rewrite. Keep the overlay/grid passes running after TAA on post-tonemap color (they stay
  sharp) unchanged.
- **Keep-current.** Where a phase adds runtime-tunable engine state it gets a matching `sa` control
  command (one registration in `saffron-control`, Phase 4); where it alters the TAA concept it updates the
  matching `docs/` page and its hub `_index.md` row in the same change.
- **Milestone gate each phase.** `just engine` then `just prepare-for-commit` (cargo fmt + clippy
  `-D warnings` + oxlint), regenerate protocol where a wire type changed
  (`cargo run -p xtask -- gen-protocol`), and `just e2e` stays green. There is **no pixel-diff harness**:
  the runtime proof of a rendering change is a validation-layer-clean headless smoke
  (`just run-engine-headless [frames]`, i.e. `SAFFRON_EXIT_AFTER_FRAMES`) plus a described visual check in
  `just run-engine` / the editor (e.g. a still hard edge shows anti-aliased steps; a panning camera shows
  no ghost trail). Do not invent a pixel test.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).

## References / sources

The recipe is the union of the canonical modern TAA implementations; consult these when implementing a
phase:

- Brian Karis, *High-Quality Temporal Supersampling* (SIGGRAPH 2014, Advances in Real-Time Rendering) —
  the canonical UE4 TAA: YCoCg variance clip + Catmull-Rom + tonemap-weighted resolve + velocity feedback.
- Playdead / Lasse Jon Fuglsang Pedersen, *Temporal Reprojection AA in INSIDE* (GDC 2016) + the reference
  source at `github.com/playdeadgames/temporal` — origin of `clip_aabb` variance clipping in YCoCg.
- Jorge Jimenez, *Filmic SMAA / SMAA T2x* (Activision R&D) — tonemapped resolve, 5-tap optimized
  Catmull-Rom, dilated velocity, built-in sharpen.
- Alex Tardif, *TAA Starter Pack* (`alextardif.com/TAA.html`) — Halton jitter, velocity un-jitter,
  moment-based variance, `1/(1+luma)` weighting (the concrete snippet source).
- Ángel Ortiz / Code Corsair, *Temporal AA and the Quest for the Holy Trail* (`elopezr.com`) — the
  conceptual walkthrough and un-jitter-in-motion-pass variant.
- Matt Pettineo (TheRealMJP) — the optimized bilinear Catmull-Rom sampler used for history.
- Diligent Engine TAA README — dynamic variance-clip `γ` by velocity.
- AMD GPUOpen FSR2/FSR3 manuals — reproject/lock/reactive-mask/RCAS spine and the `ceil(8n²)` jitter
  phase-count rule (the primary open reference for the follow-on `taa-upsampling` set).
- Epic *Temporal Super Resolution (TSR)* docs; id Tech 7 (Tiago Sousa) TSSAA; Guerrilla Decima
  (Horizon / Death Stranding) AA talks; Yang et al. 2020, *A Survey of Temporal Antialiasing Techniques*
  (CGF); Intel GameTechDev TAA; Bart Wronski, temporal supersampling notes.

Detail lives in the phase files; this page is the index.
