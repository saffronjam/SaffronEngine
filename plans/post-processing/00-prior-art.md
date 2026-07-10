# Prior art & chosen approach

**Status:** reference

The research record for `plans/post-processing/` — bloom and color grading as one subsystem. This page
surveys how the four reference stacks (Unreal Engine 5, Unity HDRP, Blender's OCIO+AgX, Godot 4) build
both effects, extracts the technique consensus, then states the one modern approach Anima builds and why
it is the correct destination rather than a cheaper fallback. The phase files (`phase-1`…`phase-6`)
implement against these conclusions; this page is the "we researched it properly" record they cite.

## Where Anima sits today

The tail of the frame is a single mandatory tonemap **compute** pass. After opaque/transparent shading
resolves into the per-view display-extent linear-HDR `color` target (`OFFSCREEN_COLOR_FORMAT =
R16G16B16A16_SFLOAT`, `engine/crates/rendering/src/pipelines.rs`), the AA/resolve step reconstructs it up
to `published_extent`, an optional `ssgi-history` copy takes the last linear-HDR read, then
`Renderer::add_tonemap_pass` (`engine/crates/rendering/src/renderer.rs`) runs `tonemap.slang`'s
`computeMain` in-place on that storage image in `GENERAL` layout. The push is
`TonemapPush { exposure: exp2(ev), mode }` (`engine/crates/rendering/src/overlay.rs`), and the four
operators live in the resource-free `tonemap_ops.slang` module (`tonemapAndEncode` → Reinhard / ACES
[Narkowicz fit] / AgX [Godot/Blender minimal fit] / Khronos PBR-Neutral), shared verbatim with the
thumbnail shaders so viewport and thumbnails encode identically. State (`exposure_ev`, `tonemap_mode`)
round-trips through the project `renderSettings` block (`engine/crates/rendering/src/render_settings.rs`),
is driven over the control plane by `set-exposure` / `set-tonemap`
(`engine/crates/control/src/commands_render.rs`), and is typed in `engine/crates/protocol/src/dto.rs`
(`SetExposureParams`, `SetTonemapParams`, `RenderStatsDto`).

There is **no bloom pass and no grade stage** — no white balance, contrast, saturation, CDL/LGG, tonal
ranges, split-toning, channel mixer, or creative LUT. Only exposure + operator + gamma. Both effects are
net-new render work, not just plumbing.

## Bloom — the four stacks

| Stack | Blur method | Threshold | Firefly control | Composite | Extras |
|-------|-------------|-----------|-----------------|-----------|--------|
| **UE5 Standard** | ~6-level Gaussian/dual-filter pyramid (13-tap down / 9-tap tent up in modern builds) | soft threshold, default pushed toward **-1 (none)** since 4.8; shape via per-mip #1–#5 Tint/Size | none built-in (per-mip tints tame it) | **additive** (Froyok's critique: not energy-conserving, brightens globally) | Dirt Mask (tex + Intensity + Tint); separate image-based Lens Flares |
| **UE5 Convolution** | true 2D **FFT convolution** with an authorable `.exr` PSF kernel | n/a (energy-conserving scatter) | inherent | energy-conserving | kernel gives star/aperture-diffraction/anamorphic for free; Scale/Center/Boost/Buffer; cinematic budget |
| **Unity HDRP** | energy-conserving mip pyramid (CoD/Jimenez) | **default 0** — docs warn "a value higher than 0 will break the energy conservation rule" | **High Quality Prefiltering** = 13-tap Karis average | scatter-lerp during upsample | Scatter = mip-combine lerp weight; Anamorphic ratio; Lens Dirt tex + Intensity; bicubic upsample |
| **Blender (compositor Glare)** | pyramid **Bloom** type, or **Fog Glow** = FFT PSF convolution | Threshold + **Smoothness** soft knee | **Clamp > Maximum** caps highlight energy | added (Strength >1 boosts, <1 blends) | three output sockets (Image / isolated Glare / Highlights mask); Streaks / Ghosts / Sun Beams / arbitrary Kernel types |
| **Godot 4** | mip-pyramid **glow**; compute path separable Gaussian, raster path CoD gather/down/up | **smoothstep knee** (`hdr_threshold` → `+hdr_scale`) + `glow_bloom` floor + `luminance_cap` | **Karis** partial-Reinhard weighting before blur | blend-mode enum (Screen default; Add/Softlight/Replace/Mix) — Softlight applied post-tonemap | 7 artist-weighted levels + `glow_normalized`; bicubic upsample; `glow_map` spatial mask/tint |

**Consensus.** The de-facto standard is the energy-conserving mip pyramid from Jimenez's "Next Generation
Post Processing in Call of Duty: Advanced Warfare" (SIGGRAPH 2014): a **13-tap Karis-averaged downsample**
(the 36-effective-sample kernel with weights summing to 1.0, half-texel offsets exploiting hardware
bilinear) building a mip chain, then a **9-tap 3×3 tent upsample** progressively accumulated back up.
Every modern shipping engine converges on it — UE5 "Standard", Unity HDRP, Godot's raster glow, and the
LearnOpenGL/Froyok reference implementations reproduce it tap-for-tap. Three findings are load-bearing:

- **Thresholdless by default.** Dropping the bright-pass is the single biggest quality/stability win —
  the pyramid inherently weights toward bright pixels, so emissive/HDR pixels bloom in luminance
  proportion with no hard cutoff to flicker under TAA. Unity ships Threshold 0 and calls a non-zero value
  a physics violation; UE moved its default to no-threshold; Godot replaces the cutoff with a smoothstep
  knee. A hard threshold is the legacy footgun, not the primary control.
- **Karis average on the first downsample only.** The `1/(1+luma)` partial-Reinhard weighting of the five
  overlapping 2×2 boxes kills single-texel HDR fireflies exactly where they exist (mip0→mip1); later mips
  are already smooth, so it is skipped there for cost and energy fidelity. HDRP, Godot, and LearnOpenGL
  all do this.
- **Energy-conserving lerp composite, not additive.** `lerp(hdr, bloom, intensity)` (Jimenez ~0.04,
  HDRP's Scatter) conserves total energy and preserves contrast; pure additive piles up brightness and
  washes the frame (Froyok's explicit critique of UE Standard). One intensity/scatter knob then reads as
  "how much light bleeds on the lens," not "make bright things blurry."

**The high-end mode is FFT convolution** (UE Convolution, Blender Fog Glow, marty's mods, Nabla): convolve
the HDR image with an authored aperture PSF via forward FFT × precomputed kernel spectrum × inverse FFT,
giving pixel-accurate diffraction spikes, ghosts, and anamorphic streaks a radial pyramid cannot express.
Nabla's optimized path is ~0.5–1.0 ms at 720p (Hermitian-packed spectrum, two real channels per complex
FFT) versus ~0.16 ms for a CoD pyramid — 3–25× costlier, and it needs an FFT compute infrastructure this
planset does not build. It swaps **only** the blur stage behind shared pre/post compositing.

## Color grading — the four stacks

| Stack | Working space & order | Grade model | View transform (DRT) | Creative LUT slot |
|-------|----------------------|-------------|----------------------|-------------------|
| **UE5** | scene-referred **linear (ACEScg if configured)**, grade **before** tonemap | ASC-CDL per tonal range: Global/Shadows/Midtones/Highlights each Saturation/Contrast/Gamma(power)/Gain(slope)/Offset(lift), with Shadows-Max/Highlights-Min band limits; White Balance Temp/Tint; Expand Gamut + Blue Correction | parametric **ACES-shaped filmic** S-curve (Slope/Toe/Shoulder/Black-clip/White-clip), set once project-wide; OCIO plugin for reference output | Color Grading LUT applied **after** tonemap in display/sRGB, scaled by LUT Intensity (Epic recommends the linear controls over it) |
| **Unity HDRP/PPv2** | grade in HDR; **bake everything + tonemap into one LogC-encoded 3D LUT** per frame, sample tetrahedrally | Color Adjustments (Post-Exposure/Contrast-around-ACEScc-mid/Color Filter/Hue/Sat), White Balance (LMS CAT), Lift/Gamma/Gain, Shadows/Midtones/Highlights with limit crossfades, Channel Mixer, Split Toning, **8 spline Color Curves** | Tonemapping stage: None/Neutral/**ACES**/Custom(Hable)/**External** (a `.cube` becomes the display transform through the same slot) | External `.cube` = the tonemap step (HDRP); URP has a dedicated Color Lookup override + Contribution |
| **Blender** | scene-linear (Rec.709 default; ACEScg selectable); **Exposure → White Balance → Curves → Look (in log) → View Transform → Gamma** | Color Balance node (Lift/Gamma/Gain **or** ASC-CDL Offset/Power/Slope `out=(i·s+o)^p` **or** White Point); Color Correction node (per-range Sat/Contrast/Gamma/Gain/Offset); RGB Curves; HSV | **AgX** (default since 4.0): inset+rotate primaries toward achromatic, log2 over ~16.5 stops, per-channel sigmoid, invert inset — graceful highlight desaturation, no Notorious-Six hue skew. Also Filmic/ACES 1.3/2.0/Khronos PBR-Neutral/False Color | Looks are ASC-CDL/curve transforms over the log encoding; view transforms ship as tetrahedral 3D `.cube` |
| **Godot 4** | grade is **display-referred** (BCS + LUT applied **after** the view transform, in perceptual sRGB) | Adjustments: Brightness (linear) / Contrast (perceptual) / Saturation; **no** wheels, no LGG, no curves | Tonemap enum: Linear/Reinhard/Filmic/**ACES**/**AgX** (allenwp sigmoid, contrast anchored at 18% grey) | single Color Correction texture slot auto-detecting GradientTexture1D (1D) vs Texture3D (3D LUT); the fetch doubles as sRGB decode |

**Consensus.** The correct architecture separates three concerns Anima's single operator currently
collapses: (1) a scene-referred **grade** (creative, unbounded HDR), (2) a fixed **display/view transform
= the DRT** (what ACES RRT+ODT, ACES 2.0 Output Transform, AgX, Khronos PBR-Neutral, and Hable filmic each
*are*), and (3) an optional display-referred creative **look** (the `.cube`). UE's filmic-tonemapper doc
states it outright: color correction is done in scene-referred linear "before they are tone mapped." The
grade goes **before** the DRT because the DRT is a fixed invertible image-formation step, not a place to
inject look; the creative `.cube` goes **after** the DRT because it operates on bounded `[0,1]` display
code values (the classic game color LUT). One scene-linear grade then feeds SDR and any future HDR view
identically — only the final view+encode branches per output.

The canonical per-op order (matching ASC-CDL SOP and UE's ColorCorrect): **Exposure → White Balance (a
Bradford/von-Kries chromatic adaptation in the working primaries, not a channel multiply) → Contrast
around a 0.18 pivot in a log2 domain (perceptually even) → Saturation around Rec.709 luma → ASC-CDL global
`out=(Slope·x+Offset)^Power` → per-range Shadows/Mid/Highlights (three masked CDL instances with
ShadowsMax/HighlightsMin band knobs) → Channel Mixer → Split Toning**. Store the grade as **ASC-CDL
SOP+Saturation** on disk so it round-trips to/from Resolve as `.cdl`/`.ccc`; surface it *also* as
Lift/Gamma/Gain (Gain≈Slope, Lift≈Offset, Gamma≈Power) so artists use either mental model over one math.

**View-transform selection guidance from the survey.** AgX is the modern default for cinematic/general use
(graceful highlight desaturation, no Notorious-Six hue skew; Blender and Godot both adopted it); Khronos
PBR-Neutral is right for asset thumbnails/product (1:1 hue+sat up to a threshold — already Anima's
thumbnail choice); the Narkowicz ACES fit gives the "filmic punch" many expect but has per-channel hue
skews (blue→purple, red→orange) and clips wide gamut, so a *correct* ACES adopts the ACES 2.0 Output
Transform (Hellwig-2022 JMh, norm-based ratio-preserving tonescale + invertible gamut compression);
Reinhard stays reference/debug only. Anima already carries all four fits in `tonemap_ops.slang`, so the
plan **reframes the operator selector as a "view/display transform"** rather than adding operators.

## Chosen approach — bloom

**Energy-conserving thresholdless mip pyramid (CoD:AW / Jimenez), scene-linear, before the tonemap pass.**
This is the modern-correct baseline every shipping engine converges on, chosen on merit, not cost:

- **13-tap Karis-averaged downsample + 9-tap tent upsample.** The exact kernels above; Karis average
  applied only on the first (mip0→mip1) downsample. Thresholdless by default; a soft-knee threshold is
  offered only as a clearly-labeled non-physical stylistic override, never the primary control.
- **Lerp composite in scene-linear** (`lerp(hdr, bloom, intensity)`, intensity ~0.05 default), one
  `scatter`/`radius` knob driving both the upsample filterRadius and the internal upsample lerp weight
  (~0.85). `rgba16f`, linear sampler, clamp-to-edge (border on downsample), input clamp ~1e-4. Mip count
  `floor(log2(min(w,h))) - 3` (6 at 1080p, 7 at 1440p+), mip0 at full res for TAA stability.
- **Inserted as new compute passes on the `color` target between the AA/resolve (or `ssgi-history` copy)
  and `add_tonemap_pass`.** It reuses `Renderer::add_compute_pass` (declaring
  `StorageImageRwCompute`/`SampledReadCompute` so the render graph derives every barrier), acquires per-mip
  scratch from the transient pool (`transient.rs`, `acquire_image`, keys `bloom-mip-0`…`N`, one keyed image
  per mip view), a new `bloom.slang` (auto-compiles — no xtask edit) with a downsample entry (Karis flag
  via push) and a tent-upsample entry, and a `create_bloom_layout` + `request_bloom` PSO.

**Why pre-tonemap in scene-linear is the point, not a detail.** Bloom composites onto unbounded scene
radiance (emissives + speculars + sun) exactly as physically-plausible glow requires, and the chosen view
transform — AgX's highlight desaturation especially — rolls off the bloomed highlights for free. Legacy
threshold+Gaussian bloom composited post-tonemap is precisely what produces flat, hue-clipped glow. The
ordering itself is a quality feature.

**Exposure interaction, decided.** Exposure currently lives as the first op inside the tonemap pass, so
bloom reads pre-exposure radiance. Because the composite is an energy-conserving *relative-fraction* lerp
this is acceptable and kept (exposure stays grade op #1) — documented rather than duplicating an exposure
multiply into the bloom stage.

Phase 2 adds the art-direction layers that share the same pre/post compositing: a lens-dirt mask
(`mask·bloom`, Intensity + Tint), anamorphic horizontal streaks (2× horizontally-squeezed blur buffer,
cool tint — Wronski), and an optional per-mip tint/size stack (UE #1–#5) for warm-core/cool-halo looks.

## Chosen approach — color grading

**A scene-linear grade folded into the existing tonemap compute pass, immediately before
`tonemapAndEncode`, plus a post-tonemap creative `.cube` slot.** The grade is a `grade()` pure helper added
to the shared `tonemap_ops` seam (or a sibling module registered in xtask's exclusion list), so thumbnails
and preview encode bit-identically — extend the shared module, never fork it. `tonemap.slang` calls
`grade()` then `tonemapAndEncode()`; there is one scene→display point and it stays shared.

- **Working space:** linear Rec.709/sRGB, to match the current operator fits. (ACEScg/AP1 is a Future note
  requiring re-derived operator input matrices in `tonemap_ops.slang`.)
- **Grade order (Phase 3 core, then Phase 4):** exposure (op #1, kept) → white-balance Bradford CAT →
  contrast (log2, pivot 0.18) → saturation → ASC-CDL global (surfaced also as Lift/Gamma/Gain) → per-range
  Shadows/Mid/Highlights masked CDL+Sat+Contrast with ShadowsMax≈0.09 / HighlightsMin≈0.5 → 3×3 channel
  mixer → split-toning.
- **Transport:** a **per-view grade uniform buffer** bound to the tonemap descriptor set, chosen from
  Phase 3 up front — the grade exceeds the 128-byte push minimum once per-range CDL lands, so a UBO avoids
  a Phase-4 push rewrite. The view-transform selector stays the existing `TonemapMode` `as_str`/`from_name`
  wire pair, relabeled "view transform."
- **Creative look (Phase 5):** a display-space `.cube` 3D-LUT applied on the `[0,1]` post-tonemap result
  with an intensity dial (tetrahedral, 17/33/65), imported into the asset catalog. Grade math runs **live
  as ALU** while authoring (instant, no bake latency, ALU beats a dependent 3D-texture fetch); a **bake
  path** folds view-transform + creative LUT + a frozen grade into one 33³ log2-shaper LUT (input EV
  ~[-14,+11]) for the exported `saffron-player` and for ingesting external `.cube` looks.

**NO-LEGACY.** The grade is folded into the one tonemap pass (no second pass, no compat shim). When OCIO
eventually lands as the color backend it **replaces** this hand-rolled switch — operators become OCIO
views, the grade becomes OCIO `GradingPrimary`/`GradingTone` — it does not run beside it. `set-bloom` and
`set-color-grading` are *the* way to drive these; there is no parallel command.

## Deliberately deferred (Future)

- **FFT convolution bloom** (authorable `.exr` PSF → aperture diffraction/star/anamorphic for free). It
  swaps only the blur stage behind the shared pre/post compositing, so Phase 1–2 leave that seam clean, but
  it needs an FFT compute infrastructure this planset does not build.
- **OCIO 2.5 as the color backend** (via a cxx bridge mirroring the `saffron-physics-sys`/Jolt seam),
  consuming `GpuShaderDesc`-emitted Vulkan shader + LUT textures and driving grading dynamic params — gives
  Blender/Resolve/Nuke config parity. It replaces the operator switch when it lands.
- **ACEScg/AP1 working space** and the **ACES 2.0 Output Transform** as the correct-ACES replacement for the
  Narkowicz fit — both require re-derived input matrices; noted, not scheduled.
- **HDR present** (scRGB FP16 or HDR10/PQ). One scene-linear grade already feeds it; only the final
  view+encode branches per output.

## Decisions to record when building

- **Working space:** stay linear Rec.709 for this planset (least-surprise, matches the fits); ACEScg is
  Future.
- **Grade transport:** per-view grade UBO from Phase 3, not a growing push — confirm the tonemap descriptor
  set takes an extra UBO binding cleanly alongside the set-0/binding-0 storage image.
- **Bloom vs exposure:** bloom composites into pre-exposure `color` (exposure is grade op #1); acceptable
  for an energy-conserving lerp — flip to exposure-before-bloom only if an artist expectation that bloom
  tracks exposure surfaces.
- **`.cube` is post-tonemap:** display-referred creative LUT on bounded `[0,1]`, not a scene-linear look
  LUT (which would need a log/allocation shaper on its input).

## Phase map (where this lands)

| Phase | Builds |
|-------|--------|
| 1 | Bloom core — the energy-conserving pyramid + full vertical slice (`bloom.slang`, PSO, transient mips, graph passes, `SetBloomParams`, `set-bloom`, persistence, RenderPanel sliders, docs). |
| 2 | Bloom polish — lens dirt, anamorphic streaks, per-mip tint (FFT convolution left as Future). |
| 3 | Grade core — scene-linear exposure/WB-CAT/contrast/saturation/global CDL folded into the tonemap pass via a grade UBO; operator selector relabeled "view transform." |
| 4 | Grade tonal ranges — per-range Shadows/Mid/Highlights CDL, channel mixer, split-toning. |
| 5 | Creative `.cube` 3D-LUT — import, post-tonemap apply, bake for the player. |
| 6 | Editor **Post** panel — net-new color-wheel + tone-curve widgets, migrating every bloom/grade row off RenderPanel (deleting the temporary rows in the same change — NO-LEGACY). |

## References

**Bloom technique & lens effects**

- Jorge Jimenez — Next Generation Post Processing in Call of Duty: Advanced Warfare (SIGGRAPH 2014) — https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/
- Advances in Real-Time Rendering in 3D Graphics and Games — SIGGRAPH 2014 (course index) — https://advances.realtimerendering.com/s2014/
- LearnOpenGL — Physically Based Bloom (13-tap down, Karis average, 9-tap tent up) — https://learnopengl.com/Guest-Articles/2022/Phys.-Based-Bloom
- Froyok / Lena Piquet — Custom Bloom Post-Process in Unreal Engine — https://www.froyok.fr/blog/2021-12-ue4-custom-bloom/
- Froyok / Lena Piquet — Custom Lens-Flare Post-Process in Unreal Engine — https://www.froyok.fr/blog/2021-09-ue4-custom-lens-flare/
- Marius Bjorge — Bandwidth-Efficient Rendering (Dual-Filter / Dual Kawase, SIGGRAPH 2015, notes) — https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_notes.pdf
- Marius Bjorge — Bandwidth-Efficient Rendering (SIGGRAPH 2015, slides) — https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_slides.pdf
- FFT Bloom Optimized to the Bone in Nabla — https://graphics-programming.org/blog/fft-bloom-optimized-to-the-bone-in-nabla
- marty's mods — Convolution Bloom (FFT convolution, aperture PSF kernels) — https://www.martysmods.com/convolutionbloom/
- Bart Wronski — Anamorphic lens flares and visual effects — https://bartwronski.com/2015/03/09/anamorphic-lens-flares-and-visual-effects/
- expenses/bloom — Vulkan implementation of the CoD:AW bloom method — https://github.com/expenses/bloom

**Unreal Engine 5**

- Bloom in Unreal Engine (Standard + Convolution FFT, dirt mask, kernel authoring) — https://dev.epicgames.com/documentation/en-us/unreal-engine/bloom-in-unreal-engine
- Color Grading and the Filmic Tonemapper (scene-referred grade before tonemap) — https://dev.epicgames.com/documentation/en-us/unreal-engine/color-grading-and-the-filmic-tonemapper-in-unreal-engine
- Color Grading Panel in Unreal Engine — https://dev.epicgames.com/documentation/en-us/unreal-engine/color-grading-panel-in-unreal-engine
- Post Process Effects in Unreal Engine — https://dev.epicgames.com/documentation/en-us/unreal-engine/post-process-effects-in-unreal-engine
- Add Post Process Volumes (Priority / Blend Weight / Blend Radius / Unbound) — https://dev.epicgames.com/documentation/en-us/unreal-engine/add-post-process-volumes
- Auto Exposure in Unreal Engine (Local Exposure) — https://dev.epicgames.com/documentation/en-us/unreal-engine/auto-exposure-in-unreal-engine
- Using the Emissive Material Input in Unreal Engine — https://dev.epicgames.com/documentation/en-us/unreal-engine/using-the-emissive-material-input-in-unreal-engine
- The new FFT convolution bloom — Epic Developer Community Forums — https://forums.unrealengine.com/t/the-new-fft-convolution-bloom/91848
- Blend Radius and Weight — UE 4.27 Content Examples — https://docs.unrealengine.com/4.27/en-US/Resources/ContentExamples/PostProcessing/1_17
- SColorGradingWheel — Unreal Engine Slate API — https://docs.unrealengine.com/4.26/en-US/API/Runtime/Slate/Widgets/Colors/SColorGradingWheel/

**Unity (HDRP / URP / Post Processing v2)**

- Bloom | High Definition RP 16.0 (energy-conserving pyramid, threshold 0, Karis prefilter, scatter) — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Bloom.html
- Bloom Volume Override reference for URP (Unity 6) — https://docs.unity3d.com/6000.0/Documentation/Manual/urp/post-processing-bloom.html
- Tonemapping | High Definition RP 14.0 (None / Neutral / ACES / Custom / External) — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@14.0/manual/Post-Processing-Tonemapping.html
- HDR tonemapping properties | High Definition RP 17.0 (paper white, nits, BT.2390) — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/reference-hdr-tonemapping.html
- Color Adjustments | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Color-Adjustments.html
- White Balance | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-White-Balance.html
- Lift Gamma Gain | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Lift-Gamma-Gain.html
- Shadows Midtones Highlights | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Shadows-Midtones-Highlights.html
- Channel Mixer | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Channel-Mixer.html
- Split Toning | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Split-Toning.html
- Color Curves | High Definition RP 16.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/Post-Processing-Color-Curves.html
- Volumes | High Definition RP 14.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@14.0/manual/Volumes.html
- Volumes | Universal RP 12.0 — https://docs.unity3d.com/Packages/com.unity.render-pipelines.universal@12.0/manual/Volumes.html
- Color Lookup Override reference for URP (Unity 6.6) — https://docs.unity3d.com/6000.6/Documentation/Manual/urp/post-processing-color-lookup.html
- Color Grading | Post Processing 3.0 (LDR / HDR / External, LogC 3D LUT bake) — https://docs.unity3d.com/Packages/com.unity.postprocessing@3.0/manual/Color-Grading.html
- Create an LUT in DaVinci Resolve | High Definition RP 16.0 (Grading LUT Size 33) — https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@16.0/manual/LUT-Authoring-Resolve.html

**Blender (compositor + OCIO/AgX color management)**

- Glare Node — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/filter/glare.html
- Displays and Views (Color Management) — Blender Manual — https://docs.blender.org/manual/en/latest/render/color_management/displays_views.html
- Color Spaces — Blender Manual — https://docs.blender.org/manual/en/latest/render/color_management/color_spaces.html
- OpenColorIO — Blender Manual — https://docs.blender.org/manual/en/latest/render/color_management/opencolorio.html
- Color Balance Node (Lift/Gamma/Gain vs ASC-CDL Offset/Power/Slope) — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/color_balance.html
- RGB Curves Node — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/rgb_curves.html
- Hue/Saturation/Value Node — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/hue_saturation.html
- Tone Map Node (legacy) — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/tone_map.html
- Color Correction Node — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/color_correction.html
- Exposure Node — Blender Manual — https://docs.blender.org/manual/en/latest/compositing/types/color/adjust/exposure.html
- Blender 4.0: Color Management (AgX view transform) — Developer Release Notes — https://developer.blender.org/docs/release_notes/4.0/color_management/
- Blender OCIO configuration (config.ocio: roles, views, looks) — https://github.com/blender/blender/blob/main/release/datafiles/colormanagement/config.ocio
- PR #106355 — Replace Default OCIO config with AgX (Filmic v2) — https://projects.blender.org/blender/blender/pulls/106355
- AgX (Troy Sobotka) — original view-transform repository — https://github.com/sobotka/AgX
- Enabling the Bloom effect in Blender 4.2 (EEVEE bloom → compositor Glare) — https://b3d.interplanety.org/en/enabling-the-bloom-effect-in-blender-4-2/
- Regression: EEVEE — Bring back the Bloom panel (#126277) — https://projects.blender.org/blender/blender/issues/126277
- AgX, Color Shifts, and The Notorious 6 — Avid Andrew — https://avidandrew.com/agx-color.html
- Qualities of Good Tonemappers — CG Meerkat — https://cgmeerkat.github.io/blog/who-needs-a-tonemapper/
- Implement AgX as a tonemapper — Godot proposals #7545 — https://github.com/godotengine/godot-proposals/discussions/7545
- AgX module — darktable user manual — https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/agx/

**Godot 4**

- Environment and post-processing (Glow, Tonemap, Adjustments tutorial) — https://docs.godotengine.org/en/stable/tutorials/3d/environment_and_post_processing.html
- Environment class reference (glow_*, tonemap_*, adjustment_* 3D LUT) — https://docs.godotengine.org/en/stable/classes/class_environment.html
- Source — tonemap.glsl (gather_glow, apply_glow blend modes, BCS, LUT) — https://github.com/godotengine/godot/blob/master/servers/rendering/renderer_rd/shaders/effects/tonemap.glsl
- Source — copy.glsl (MODE_GLOW bright-pass: threshold knee, bloom floor, luminance cap, Karis, Gaussian mip blur) — https://github.com/godotengine/godot/blob/master/servers/rendering/renderer_rd/shaders/effects/copy.glsl
- Source — copy_effects.cpp (gaussian_glow / downsample / upsample mip pyramid driver) — https://github.com/godotengine/godot/blob/master/servers/rendering/renderer_rd/effects/copy_effects.cpp
- PR #87260 — Add AgX tonemapper option to Environment (iolite / Minimal-AgX origin) — https://github.com/godotengine/godot/pull/87260
- PR #106940 — Add white, contrast and HDR support to the AgX tonemapper (18% grey anchor) — https://github.com/godotengine/godot/pull/106940
- allenwp — The allenwp tonemapping curve (parametric sigmoid, SDR/HDR/EDR stable) — https://allenwp.com/blog/2025/05/29/allenwp-tonemapping-curve/
- DeepWiki — Environment & Post-Processing (post-process ordering) — https://deepwiki.com/godotengine/godot/7.7-environment-and-post-processing
- iolite-engine — Minimal AgX Implementation (Troy Sobotka reference) — https://iolite-engine.com/blog_posts/minimal_agx_implementation
- PR #42761 — Environment brightness/contrast/saturation restore with 3D LUT — https://github.com/godotengine/godot/pull/42761

**Color management, tone mapping & interchange standards**

- OpenColorIO — Overview (scene-linear working space, view/look split) — https://opencolorio.readthedocs.io/en/latest/concepts/overview/overview.html
- OpenColorIO — Displays & Views — https://opencolorio.readthedocs.io/en/latest/guides/authoring/displays_views.html
- OpenColorIO — GPU Shaders API (GpuShaderDesc, 3D-LUT textures, Vulkan bindings) — https://opencolorio.readthedocs.io/en/latest/api/shaders.html
- OpenColorIO — Baking LUTs (shaper/prelut, allocation vars, .cube) — https://opencolorio.readthedocs.io/en/latest/tutorials/baking_luts.html
- OpenColorIO — 2.5 Release notes (Vulkan GPU renderer support) — https://opencolorio.readthedocs.io/en/latest/releases/ocio_2_5.html
- ACES Documentation — Output Transforms — https://docs.acescentral.com/system-components/output-transforms/
- ACES Documentation — Output Transforms: Tone Mapping (ACES 2.0 norm-based tonescale) — https://docs.acescentral.com/system-components/output-transforms/technical-details/tone-mapping/
- ACES Documentation — Reference Gamut Compression Specification — https://docs.acescentral.com/rgc/specification/
- Chris Brejon — Academy Color Encoding System (ACES 1.x hue skews, Abney effect, DRT) — https://chrisbrejon.com/cg-cinematography/chapter-1-5-academy-color-encoding-system-aces/
- Leveraging ACES 2.0 in DaVinci Resolve (JMh, gamut mapping, tonescale) — https://www.cubiecolor.com/post/aces-2-0-davinci-resolve-color-grading
- Khronos — PBR Neutral Tone Mapper Released — https://www.khronos.org/news/press/khronos-pbr-neutral-tone-mapper-released-for-true-to-life-color-rendering-of-3d-products
- model-viewer — PBR Neutral Tone Mapping — https://modelviewer.dev/examples/tone-mapping
- MrLixm/AgXc — AgX display rendering transform (log2 encode, inset, tonescale, outset) — https://github.com/MrLixm/AgXc
- ASC CDL — Wikipedia (slope/offset/power SOP + saturation, order of operations) — https://en.wikipedia.org/wiki/ASC_CDL
- Pomfort — An in-depth look at ASC-CDL based color controls — https://pomfort.com/article/an-in-depth-look-at-asc-cdl-based-color-controls/
- Brompton — What is a 3D LUT? (33³ sizes, tetrahedral interpolation) — https://www.bromptontech.com/what-is-a-3d-lut/
- Krzysztof Narkowicz — HDR Display First Steps (scRGB vs HDR10/PQ, nits) — https://knarkowicz.wordpress.com/2016/08/31/hdr-display-first-steps/
- Microsoft — Use DirectX with Advanced Color / HDR (scRGB CCCS, ST.2084 swapchain, SetColorSpace1) — https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range
