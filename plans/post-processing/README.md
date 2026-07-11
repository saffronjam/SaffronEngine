# Post-processing (bloom & color grading)

**Status:** COMPLETED

Emissive materials, speculars, and the sun already write unbounded radiance into the linear-HDR scene target, but nothing spreads that energy — a bright pixel stays a pinpoint, and the only tone control the image gets is a single exposure multiply plus a fixed display operator. This planset adds a **post-processing subsystem** — an energy-conserving bloom mip-pyramid and a scene-referred color grade — inserted into the existing post chain on the display-extent `color` target. Bloom runs as new compute passes strictly *before* the tonemap pass so the glow lives in scene-linear radiance; the grade folds *into* the existing tonemap compute pass ahead of the display transform, with a post-tonemap creative `.cube` LUT slot for looks. The anti-pattern being removed is "the tonemap operator is the whole tone stage": the operator selector is reframed as a **view/display transform**, and look decisions move to a proper grade in front of it — one scene-linear grade feeds SDR and any future HDR view identically.

## Why

- **Scene color is linear HDR with the headroom bloom needs, but no pass reads it for glow.** The per-view scene target is `ViewTarget.offscreen` (the graph `color` resource), format `OFFSCREEN_COLOR_FORMAT = R16G16B16A16_SFLOAT`, created `COLOR_ATTACHMENT|SAMPLED|TRANSFER_SRC|STORAGE` at display extent (`engine/crates/rendering/src/view_target.rs`, `ViewTarget::offscreen`; `engine/crates/rendering/src/pipelines.rs`, `OFFSCREEN_COLOR_FORMAT`) → emissive/HDR-bright pixels never bleed into their neighbours.
- **The post chain has an open, correctly-placed seam and nothing occupies it.** In `Renderer::render` the tail runs resolve/AA → an optional `ssgi-history` copy (the last linear-HDR reader) → the mandatory in-place tonemap compute pass → grid/overlay (`engine/crates/rendering/src/renderer.rs`, final-post block; `add_tonemap_pass`) — the gap *between* the resolve/`ssgi-history` block and `add_tonemap_pass` is exactly where a scene-linear bloom belongs, and today it is empty.
- **The only tone control is exposure + a display operator.** The tonemap compute pass carries `TonemapPush { exposure = exp2(ev), mode }` and calls `tonemapAndEncode(color.rgb * exposure, mode)` (`engine/crates/rendering/src/overlay.rs`, `TonemapPush`; `engine/assets/shaders/tonemap.slang`, `computeMain`) — no white balance, contrast, saturation, or CDL anywhere in the linear stage.
- **No bloom pass, PSO, descriptor set, or DTO exists.** There is no `add_bloom_pass`, no `request_bloom`, no `create_bloom_layout`, and no `SetBloomParams`/`SetColorGradingParams` on the wire (`engine/crates/rendering/src/renderer.rs`; `engine/crates/rendering/src/descriptors.rs`; `engine/crates/protocol/src/dto.rs`) — the whole subsystem is net-new plumbing plus the two real GPU stages.
- **The operator selector is presented as if it were a look knob.** `TonemapMode` (`Reinhard`/`Aces`/`Agx`/`PbrNeutral`) is a display-image-formation transform, not a place to inject grade (`engine/crates/rendering/src/overlay.rs`, `TonemapMode::as_str`/`from_name`; the RenderPanel `Select`) → grading a scene today can only be faked by abusing exposure and the operator choice.

## Design stance (grounded in current engine practice)

The two stages are built the modern, technically-correct way, not the cheaper legacy shapes.

**Bloom — thresholdless energy-conserving mip pyramid, scene-linear, pre-tonemap.** The default is the Call-of-Duty:Advanced-Warfare / Jimenez method: a 13-tap Karis-averaged bilinear downsample chain and a progressive 9-tap tent upsample, composited by an energy-conserving `lerp(hdr, bloom, intensity)` (Jimenez ~0.04) rather than an additive bright-pass blur. There is **no bright-pass threshold by default** — emissive and HDR-bright pixels bloom automatically in luminance proportion, matching Unity HDRP (threshold 0, Karis prefilter, scatter) and the LearnOpenGL/Froyok physically-based pyramid. The Karis average is applied *only* on the first (mip0→mip1) downsample where single-texel HDR fireflies live, which is the firefly/TAA-stability win. Bloom composites into `color` **before** the tonemap pass, so it operates on unbounded scene radiance exactly as physically-plausible glow requires, and the chosen view transform (AgX's graceful highlight desaturation especially) rolls the bloomed highlights off for free — the ordering is itself a quality feature. This retires the classic "threshold → Gaussian → add" bloom outright; there is no parallel additive path.

**Color grade — scene-referred grade before the display transform, creative LUT after.** This is the industry-standard split: color correction happens in scene-referred linear *before* tone mapping, confirmed by Unreal's filmic-tonemapper documentation and matched by Blender's grade-in-scene-linear + AgX view transform. The grade folds into the existing tonemap compute pass immediately before `tonemapAndEncode` and runs the ASC-CDL SOP+Sat order (exposure → white-balance chromatic adaptation → contrast around a 0.18 log2 pivot → saturation → global CDL, then per-range shadows/mids/highlights, channel mixer, split-tone). The canonical on-disk form is ASC-CDL slope/offset/power + saturation so grades round-trip to and from Resolve as `.cdl`/`.ccc`. A **creative `.cube` 3D-LUT** look slot runs *after* the display transform on bounded `[0,1]` display code values (the classic game color LUT), because the DRT is a fixed invertible image-formation step, not a place to inject look. The grade helper is a pure function in the shared `tonemap_ops` module so viewport and thumbnails encode bit-identically.

> **Scene shading → linear-HDR `color` → bloom pyramid (lerp-composite) → { grade (scene-linear, ASC-CDL) → view/display transform → creative `.cube` LUT (display-referred) } → overlays → present.** One scene-linear grade feeds SDR and any future HDR view; only the final view+encode branches per output.

## Goal

- **Bloom is a real render-graph subsystem.** A `bloom.slang` compute shader (downsample + tent-upsample entries), a `request_bloom` PSO, a `create_bloom_layout` descriptor set, per-view sets, and transient mip scratch, driven by `Renderer::add_bloom_pass` called *between* the resolve/`ssgi-history` block and `add_tonemap_pass` — energy-conserving, thresholdless, scene-linear.
- **The grade folds into the one tonemap pass.** A pure `grade()` helper added to `tonemap_ops.slang`, driven by a per-view grade uniform buffer bound to the tonemap set (chosen from the start so the later per-range work needs no push-size rewrite); `tonemap.slang` calls `grade()` then `tonemapAndEncode()`. No second grade pass, no duplicated exposure multiply.
- **Every setting is a first-class control setting.** `set-bloom` and `set-color-grading` are the *only* commands for their state — one `CommandSpec` row each, read back through `RenderStatsDto`, persisted in the project `renderSettings` block, and scriptable from the `sa` CLI with no per-command CLI code.
- **The operator selector is relabeled, not duplicated.** The existing `TonemapMode` wire pair stays the one path; its editor label becomes "View transform". No parallel "look mode" enum is introduced.
- **No parallel UI path survives.** Controls land in `RenderPanel` through Phases 1–5; Phase 6 builds the dedicated Post panel and the temporary RenderPanel rows are **deleted in the same change** (NO-LEGACY) — bloom/grade live in exactly one panel when the planset is done.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-bloom-core.md` | The default bloom: a thresholdless 13-tap Karis-averaged downsample + 9-tap tent upsample mip pyramid composited via an energy-conserving lerp into scene-linear `color` before the tonemap pass. Full vertical slice — `bloom.slang`, `request_bloom` PSO, `create_bloom_layout` + per-view set, transient mip chain, `add_bloom_pass`, `SetBloomParams`/`Result` DTOs, `set-bloom` command + `sa`, persistence, RenderPanel sliders, docs. Independently shippable. | — |
| 2 | `phase-2-bloom-lens-dirt-anamorphic.md` | Art-direction layers over the same pre/post compositing: a lens-dirt mask texture (intensity + tint), horizontally-squeezed anamorphic streaks (Wronski), and an optional per-mip tint/size stack. Extends `SetBloomParams`, persistence, and RenderPanel. FFT convolution bloom is explicitly deferred to Future — the blur seam is left clean. | 1 |
| 3 | `phase-3-grade-core-folded-tonemap.md` | A scene-linear grade folded into the tonemap pass before `tonemapAndEncode`: exposure (kept as grade op #1), white-balance chromatic adaptation, contrast-around-pivot in log2, saturation, and global ASC-CDL (surfaced as Lift/Gamma/Gain). Driven by a per-view grade UBO on the tonemap set. Reframes the operator selector as a view/display transform. `SetColorGradingParams`/`Result`, `set-color-grading`, persistence, RenderPanel grade rows, docs. Independently shippable. | — |
| 4 | `phase-4-grade-tonal-ranges-cdl.md` | Per-range Shadows/Midtones/Highlights CDL blended by smooth luma masks with `ShadowsMax`/`HighlightsMin` knobs (UE model), a 3×3 channel mixer, and split-toning — all inside the same grade stage and grade UBO. Extends the DTO, persistence, and RenderPanel. | 3 |
| 5 | `phase-5-creative-lut-import-bake.md` | A display-space creative `.cube` 3D-LUT look slot applied post-tonemap on `[0,1]` with an intensity dial (tetrahedral, 17/33/65), imported into the asset catalog; plus a bake path folding view-transform + creative LUT + a frozen grade into one 33³ log2-shaper LUT for the exported `saffron-player` and for ingesting external `.cube` looks. | 3 |
| 6 | `phase-6-post-panel-wheels-curves.md` | The dedicated editor **Post** panel (registry row + dock id) sectioning Bloom and Grading, built from net-new `GradingWheel` (Resolve-style trackball) and `ToneCurve` widgets, migrating every bloom/grade control off `RenderPanel` and **deleting the temporary rows in the same change** (NO-LEGACY). Optional pre-encode histogram/waveform scopes. | 2, 4, 5 |

The chain: Phase 1 (bloom) and Phase 3 (grade core) each stand alone and ship independently — bloom is a self-contained set of new passes, and the grade folds into the tonemap pass that already exists. Phase 2 refines bloom; Phases 4–5 refine the grade; every one lands controls in the shared `RenderPanel`. Phase 6 is the cutover: it builds the real Post panel and the two widgets the codebase lacks, then removes the RenderPanel bloom/grade rows outright — no old UI path survives alongside the new (NO-LEGACY).

## Future (unscheduled)

- **FFT convolution bloom.** An authorable `.exr` PSF kernel (aperture diffraction / star / anamorphic for free) swaps *only* the blur stage behind the shared pre/post compositing (Hermitian-packed spectrum, ~0.5–1 ms @ 720p in Nabla). Phase 2 leaves that seam clean, but it needs an FFT compute infrastructure this planset does not build.
- **ACEScg / AP1 working space.** The grade stays linear Rec.709/sRGB to match the current operator input fits; moving the working space to ACEScg requires re-deriving the ACES/AgX/PBR-Neutral input matrices in `tonemap_ops.slang` and is deliberately not scheduled.
- **OpenColorIO.** When OCIO lands it *replaces* this switch — operators become OCIO views and the grade becomes `GradingPrimary`/`GradingTone` — it is not built beside this stage.

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| Scene HDR target (the `color` resource) | `engine/crates/rendering/src/view_target.rs` | `ViewTarget::offscreen`, `published_extent`, `write_aa_sets`, `Binding::sampled`/`storage` |
| HDR color format | `engine/crates/rendering/src/pipelines.rs` | `OFFSCREEN_COLOR_FORMAT`, `build_compute`, `build_compute_multi`, `request_tonemap`, `load_shader_module` |
| Post-chain insertion + compute helper | `engine/crates/rendering/src/renderer.rs` | final-post block, `add_tonemap_pass`, `add_compute_pass`, `ssgi-history` copy, `FramePipelines` |
| Render-graph usages / barriers | `engine/crates/rendering/src/render_graph.rs` | `RgUsage::StorageImageRwCompute`, `SampledReadCompute`, `RgPass::compute`, `import_image` |
| Transient mip scratch | `engine/crates/rendering/src/transient.rs` | `TransientResources::acquire_image`, `begin_frame` |
| Descriptor factory + pool budget | `engine/crates/rendering/src/descriptors.rs` | `create_fxaa_layout` (template), `create_tonemap_layout`, `linear_sampler`, `allocate_set`, STORAGE_IMAGE budget |
| Tonemap push + operator wire pair | `engine/crates/rendering/src/overlay.rs` | `TonemapPush`, `TonemapMode::as_str`/`from_name`, `final_post_pass_names` |
| Tonemap compute entry + shared ops | `engine/assets/shaders/tonemap.slang`, `engine/assets/shaders/tonemap_ops.slang` | `computeMain`, `Push`, `tonemapAndEncode`, `tonemapReinhard/Aces/Agx/PbrNeutral` |
| Shader auto-registration / module exclusion | `engine/xtask/src/shaders.rs` | entry-point loop, `TONEMAP_OPS_STEM`, module exclusion list |
| Wire DTOs + command table | `engine/crates/protocol/src/dto.rs`, `engine/crates/protocol/src/command.rs` | `SetExposureParams`/`SetTonemapParams` (templates), `RenderStatsDto`, `COMMANDS`, `COMMAND_FIXTURES`, `DTO_TYPE_NAMES`, `render_domain` |
| Codegen dispatch + tripwires | `engine/crates/protocol/src/codegen.rs`, `tests/inventory.rs`, `tests/schema_fragments.rs` | `ts_decls`, `fragment_decls`, `inventory!`, `check!` |
| Control handler + renderer trait | `engine/crates/control/src/commands_render.rs`, `registry.rs`, `engine/crates/host/src/control_renderer.rs` | `register_render_commands`, `ControlRenderer` (getters/setters), `render_stats_dto` |
| Persistence | `engine/crates/rendering/src/render_settings.rs` | `RenderSettings`, `settings_to_json`, `parse_render_settings`, `render_settings_to_json`, `apply_render_settings` |
| Editor control + panels | `editor/src/control/client.ts`, `editor/src/panels/RenderPanel.tsx` | `setExposure`/`setTonemap` (templates), optimistic fold, `pushEdit(...,'scene')`, `NumberDrag`/`SliderField`/`ColorField` |
| Contract test fixtures | `tools/check-control-schema/check.ts` | `paramsForFixture` switch |

## Ground rules

- **One write path per setting.** `set-bloom` and `set-color-grading` are the sole commands for their state; the operator selector stays the one `TonemapMode` wire pair, relabeled — never a second "look mode" enum, never a duplicate grade pass.
- **One schema.** DTOs live once in `dto.rs`; `@saffron/protocol` (`editor/src/protocol/sa-types.ts`) is regenerated by `xtask gen-protocol` and never hand-edited.
- **Cutover deletes the old path.** Phase 6 removes the temporary RenderPanel bloom/grade rows in the same change that adds the Post panel — no superseded UI survives.
- **Each phase ends green.** `just engine` + `just prepare-for-commit` (format + clippy `-D warnings`), a headless boot with a validation-clean log (`just run-engine-headless`) on an emissive/graded fixture, `bun run check` in `editor/` where a wire type changed (regenerate `@saffron/protocol` via `xtask gen-protocol` — never hand-edit `sa-types.ts`), `just e2e`, and the `docs/content/` post-processing page (Bloom or Color grading) + its hub `_index.md` row updated when the concept changes.

Detail lives in the phase files; this page is the index.

## References

Bloom:

- Jorge Jimenez — *Next Generation Post Processing in Call of Duty: Advanced Warfare* (SIGGRAPH 2014): https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/
- LearnOpenGL — *Physically Based Bloom* (13-tap down, Karis average, 9-tap tent up): https://learnopengl.com/Guest-Articles/2022/Phys.-Based-Bloom
- Froyok / Lena Piquet — *Custom Bloom Post-Process in Unreal Engine*: https://www.froyok.fr/blog/2021-12-ue4-custom-bloom/
- Marius Bjorge — *Bandwidth-Efficient Rendering* (Dual-Filter, SIGGRAPH 2015): https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_notes.pdf
- Bart Wronski — *Anamorphic lens flares and visual effects*: https://bartwronski.com/2015/03/09/anamorphic-lens-flares-and-visual-effects/
- *FFT Bloom Optimized to the Bone in Nabla*: https://graphics-programming.org/blog/fft-bloom-optimized-to-the-bone-in-nabla
- Unreal Engine — *Bloom* (standard + convolution FFT, dirt mask, kernel authoring): https://dev.epicgames.com/documentation/en-us/unreal-engine/bloom-in-unreal-engine
- Unity HDRP — *Bloom* (energy-conserving pyramid, threshold 0, Karis prefilter, scatter): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Bloom.html

Color grading & pipeline:

- Unreal Engine — *Color Grading and the Filmic Tonemapper* (scene-referred grade before tonemap): https://dev.epicgames.com/documentation/en-us/unreal-engine/color-grading-and-the-filmic-tonemapper-in-unreal-engine
- Unity HDRP — *Lift Gamma Gain / Shadows Midtones Highlights / Split Toning / Channel Mixer*: https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Lift-Gamma-Gain.html
- Unity Post Processing — *Color Grading* (LDR/HDR/External, LogC 3D LUT bake): https://docs.unity3d.com/Packages/com.unity.postprocessing@3.0/manual/Color-Grading.html
- Blender Manual — *Glare Node* (Bloom / Fog Glow FFT / Streaks): https://docs.blender.org/manual/en/latest/compositing/types/filter/glare.html
- Blender Manual — *Displays and Views / AgX view transform*: https://docs.blender.org/manual/en/latest/render/color_management/displays_views.html
- Blender Manual — *Color Balance Node* (Lift/Gamma/Gain vs ASC-CDL Offset/Power/Slope): https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/color_balance.html
- Godot source — *tonemap.glsl* (gather_glow, apply_glow blend modes, BCS, LUT): https://github.com/godotengine/godot/blob/master/servers/rendering/renderer_rd/shaders/effects/tonemap.glsl
- ASC CDL — Wikipedia (slope/offset/power SOP + saturation, order of operations): https://en.wikipedia.org/wiki/ASC_CDL
- OpenColorIO — *Overview* (scene-linear working space, view/look split): https://opencolorio.readthedocs.io/en/latest/concepts/overview/overview.html
- OpenColorIO — *Baking LUTs* (shaper/prelut, allocation vars, `.cube`): https://opencolorio.readthedocs.io/en/latest/tutorials/baking_luts.html
- ACES Documentation — *Output Transforms: Tone Mapping* (ACES 2.0 norm-based tonescale): https://docs.acescentral.com/system-components/output-transforms/technical-details/tone-mapping/
- MrLixm/AgXc — *AgX display rendering transform* (log2 encode, inset, tonescale, outset): https://github.com/MrLixm/AgXc
