+++
title = 'Bloom'
weight = 7
math = true
+++

# Bloom

Bloom spreads the energy of bright pixels into their neighbours, the way a real lens and sensor bleed
light around an intense highlight. Emissive materials, speculars, and the sun already write unbounded
radiance into the linear-HDR scene target, but without bloom a bright pixel stays a pinpoint. Bloom
gathers that energy into a soft glow whose size and brightness track the source's luminance.

It is an energy-conserving mip pyramid — the Call-of-Duty: Advanced Warfare / Jimenez method, not a
bright-pass plus Gaussian blur. It runs as compute passes on the display-extent `rgba16f` offscreen
**before** the [tonemap](../tonemap-and-exposure/) pass, so the glow lives in unbounded scene-linear
radiance and the view transform rolls the bloomed highlights off for free.

## The pyramid

Three pass kinds, all driven by one shader (`bloom.slang`) and one PSO — a push `pass`/`karis` field
selects the branch per dispatch:

1. **Downsample** — a 13-tap bilinear kernel (36 effective taps) halves the image each level, building
   a half-resolution-first mip chain (≈6 levels at 1080p, 7 at 1440p+). Each level is a distinct keyed
   image from the transient pool, so its barriers stay independent and nothing outlives the frame.
2. **Upsample** — a progressive 9-tap 3×3 tent, offsets scaled by one `filterRadius` UV (the scatter
   dial), adds each coarse level back into the next-finer one on the way down to `mip0`.
3. **Composite** — an energy-conserving `lerp` of the accumulated `mip0` glow back into the scene:

$$
c_\text{out} = \operatorname{lerp}(c_\text{hdr},\; c_\text{bloom} \cdot \text{tint},\; \text{intensity})
$$

The composite is a **relative-fraction mix, never additive**. `intensity` reads as the glow's share of
the final pixel (Jimenez uses ≈0.04), so bloom never piles brightness onto an already-bright pixel the
way an additive blur does. There is one composite path — additive bloom is not offered as an alternate.

## Thresholdless, with a Karis firefly guard

Bloom is **thresholdless by default**: there is no bright-pass cutoff, so emissive and HDR-bright pixels
bloom automatically in luminance proportion (matching Unity HDRP and the physically-based pyramid). The
stability mechanism is instead a **Karis luma average** — each of the five overlapping 2×2 boxes on the
`color → mip0` downsample is weighted by $1/(1 + \text{luma})$, so a lone single-texel HDR firefly
contributes far less than its raw value. That is exactly where fireflies live, so the average is applied
**only** on the first downsample (gated by the push `karis` flag); deeper mips are already smooth.

A `threshold` knob exposes a non-physical soft-knee prefilter for art direction. It is off (`0.0`) by
default and documented as an escape hatch, not the recommended path — do not treat it as a bright-pass
stage.

## Art direction: lens dirt, anamorphic streaks, per-mip tint

Three layers ride the *same* pre/post compositing — one `set-bloom` command, one `BloomPush`, one
composite path — so they never fork a second bloom.

**Lens dirt.** A mask texture models the scatter of a dirty lens or sensor cover *on the glow that
already exists*, so it **multiplies** the accumulated pyramid, clamped `≤ 1` — it only attenuates,
never adds energy (matching Unreal's dirt-mask semantics). An `intensity` lerps the mask in and a
`tint` colours it. An absent mask binds the renderer's 1×1 white texture (mask = 1 ⇒ identity), so
there is no `HAS_DIRT` branch — the shader always samples `dirtMask`. The mask is sampled in screen
UV (the standard game approximation), so it does not track camera roll or FOV.

**Anamorphic streaks.** A horizontally-squeezed blur of the bright pyramid (Bart Wronski), added
over the radial bloom before the composite lerp — the streak *is* extra glow energy, so unlike dirt
it is additive. Two ping-pong passes widen a cool-tinted horizontal gaussian whose reach is
`scatter × ratio` (a `ratio` of ~2 stretches it into a lens streak); `intensity` scales the add. Off
by default, and when off the streak binding is the white fallback with `intensity = 0`, so no energy
enters the composite.

**Per-mip tint.** An optional stack tints each mip's contribution during the progressive tent
upsample (a warm-core / cool-halo look). The tent is already one dispatch per mip, so the renderer
fans the stack out on the CPU — one tint per upsample pass — and the push carries a single `mipTint`
at a time (identity `1,1,1` when a level is absent), keeping the push tiny.

All three attenuate/add on the pre-exposure radiance the composite already reads (exposure is grade
op #1 *inside* the tonemap pass, after bloom); because the composite is an energy-conserving relative
fraction, the dirt/streak intensities track the rest of the bloom.

## Why before the tonemap

Bloom composites into `color` while it is still unbounded scene radiance, in the seam between the
resolve / SSGI-history block and the mandatory tonemap. Running in scene-linear is what makes the glow
physically plausible: the chosen view transform (AgX's highlight desaturation especially) then rolls the
bloomed highlights off gracefully, so the ordering itself is a quality feature. Exposure is applied
*inside* the tonemap pass, after bloom — because the composite is an energy-conserving fraction, this is
correct and no duplicate exposure multiply is injected into the bloom stage.

Each pass declares its `(resource, usage)` and the [render graph](../../frame-and-render-graph/render-graph-overview/)
derives every `GENERAL ↔ SHADER_READ_ONLY` transition — the same
[compute post-process](../compute-post-process-pattern/) shape the tonemap uses, one step earlier.

## Driving it

`set-bloom` is the one control command for all bloom state — the core (`enabled`, `intensity`,
`scatter`, `tint`, default-off `threshold`) plus the art-direction patch (`dirtTexture`,
`dirtIntensity`, `dirtTint`, the `anamorphic` block, and `perMipTint`). It is scriptable from the `sa`
CLI, read back through `render-stats`, persisted in the project `renderSettings` block (the dirt mask
asset is rebound by the asset-aware project loader), and surfaced in the **Bloom** tab of the editor's
[Post panel](../color-grading/#the-post-panel) alongside the color grade.

## In the code

| What | File | Symbols |
|---|---|---|
| The shader | `bloom.slang` | `computeMain`, `downsample13`, `upsampleTent`, `streakBlur`, `Push` |
| Pyramid + streak passes + state | `renderer.rs` | `Renderer::add_bloom_pass`, `acquire_bloom_mips`, `acquire_bloom_streak`, `set_bloom`, `set_bloom_dirt_texture`, `set_bloom_anamorphic` |
| Push struct | `overlay.rs` | `BloomPush` |
| Transient mip + streak chains | `transient.rs` | `BLOOM_MIP_KEYS`, `BLOOM_STREAK_KEYS` |
| PSO + descriptor set | `pipelines.rs`, `descriptors.rs` | `Pipelines::request_bloom`, `create_bloom_layout`, `MAX_BLOOM_MIPS`, `BLOOM_PASSES_PER_FRAME` |
| Composite bindings + white fallback | `view_target.rs` | `write_bloom_sets`, `BloomCompositeBindings` |
| Wire DTO + command | `dto.rs`, `commands_render.rs` | `SetBloomParams`, `AnamorphicParams`, the `set-bloom` command |
| Persistence | `render_settings.rs` | the `bloom*` keys in `renderSettings` |

## Related

- [Tonemapping](../tonemap-and-exposure/) — the display transform bloom composites in front of
- [Compute post-process](../compute-post-process-pattern/) — the shared read-modify-write shape
- [Render graph](../../frame-and-render-graph/render-graph-overview/) — how the layout moves are derived

> [!NOTE]
> FFT-convolution bloom — an authorable `.exr` point-spread kernel for aperture-diffraction and
> anamorphic streaks for free — is a future extension that swaps only the blur stage behind this shared
> composite. The dirt multiply, streak add, and per-mip tint all live in the composite/upsample stage,
> so a future blur swap does not touch them — the seam is left clean.
