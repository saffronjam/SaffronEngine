+++
title = 'Color grading'
weight = 6
math = true
+++

# Color grading

Color grading shapes the look of a scene — its warmth, contrast, and saturation — as a deliberate
artistic decision, separate from the fixed step that maps HDR to the display. The engine splits the two:
a **scene-linear grade** runs on unbounded radiance, then a **view/display transform** forms the final
image. Grading in scene-linear light before the transform is the industry-standard order (Unreal's
filmic tonemapper, Blender's grade-then-AgX): one grade feeds SDR and any future HDR view identically,
and the operator choice stops being the only look knob.

The grade folds into the existing [tonemap](../tonemap-and-exposure/) compute pass, immediately before
`tonemapAndEncode`. It adds no second pass and no second image write — a per-pixel body of exposure →
grade → view transform. A neutral grade is a mathematical identity, so an ungraded scene is unchanged.

## The order of operations

Per pixel, after the existing exposure multiply (grade op #1), the grade applies four steps in ASC-CDL
order:

1. **White balance** — a Bradford chromatic-adaptation `float3×3` matrix multiply in the working
   primaries. Temperature and tint are turned into that matrix on the CPU (a Planckian-locus target
   white → Bradford cone response → adapt from the 6500 K working white), so the shader cost is one
   `mul`. Lowering the temperature warms the image.
2. **Contrast** — a gain around the 0.18 middle-grey pivot in the `log2` domain, for perceptually even
   contrast (matching Unreal's scene-referred order):

$$
c = 2^{\,(\log_2 c - \log_2 p)\,\cdot\,k \; + \; \log_2 p}
$$

   where $p = 0.18$ is the pivot and $k$ the contrast gain.
3. **Saturation** — a lerp toward Rec.709 luma, `lerp(luma, c, saturation)`.
4. **Global ASC-CDL** — the canonical slope/offset/power: $c_\text{out} = (S \cdot c + O)^{P}$, with a
   `max(·, 0)` guard before the power.

After the global ops the grade runs three masked correction ranges, a channel mixer, and split-toning
(below), then `tonemapAndEncode`.

`grade()` is a **pure, resource-free function** in the shared `tonemap_ops` module — the same module the
thumbnail and material-preview shaders import — so those surfaces encode identically; they simply never
bind the grade uniform and never call it.

## Tonal ranges — Shadows / Midtones / Highlights

Colorists correct the dark, mid, and bright parts of an image separately. After the global ops the grade
blends three ranges, each running the same CDL + saturation + contrast trio behind a smooth luma mask
(the Unreal Shadows/Midtones/Highlights model). Two knobs shape the masks:

- **`ShadowsMax`** (`~0.09`) — the luma at which the shadow mask falls to zero.
- **`HighlightsMin`** (`~0.5`) — the luma at which the highlight mask begins to rise.

The weights are `ws = 1 − smoothstep(0, ShadowsMax, l)`, `wh = smoothstep(HighlightsMin, 1, l)`, and the
midtone weight is the smooth remainder `saturate(1 − ws − wh)`. Each range is then a **weighted lerp
toward its corrected result**, never a hard `if luma < ShadowsMax` branch — the `smoothstep` overlap
means a pixel near a boundary receives a blend of the two neighbouring corrections, so gradients never
band. The masks read **pre-tonemap scene-linear luma**, so a very bright highlight sits firmly in the
highlight mask before the view transform rolls it off (the correct scene-referred behaviour, matching
Unreal). A neutral range (slope 1, offset 0, power 1, saturation/contrast 1) is an identity.

## Channel mixer

A row-major 3×3 matrix applied after the ranges, `out = M · rgb`, identity by default. It remaps each
output channel from a weighted sum of the input channels — the classic tool for cross-channel looks
(a teal-shadows push, a sepia collapse) or swapping/attenuating a channel. Each coefficient is bounded
to `[−2, 2]` in the editor.

## Split-toning

A shadow tint and a highlight tint blended by luma, applied last before the view transform. The blend
factor is `saturate(luma + balance)` — the **balance** knob (`−1..1`) biases the neutral pivot toward
shadows or highlights. The tints are stored around a `0.5` neutral and doubled in the shader, so an
untinted split (`0.5, 0.5, 0.5`) multiplies by 1 (an identity).

## The view transform is not a look knob

The operator selector (`Reinhard` / `ACES` / `AgX` / `PBR Neutral`) is a fixed image-formation step, not
a place to inject a look. The RenderPanel labels it **View transform** for exactly this reason: look
decisions live in the grade in front of it, and the transform only forms the display image. AgX's
graceful highlight desaturation rolls the graded highlights off for free.

## Lift / Gamma / Gain

The canonical on-disk and wire form is ASC-CDL slope/offset/power + saturation, so a grade round-trips to
and from a color grade suite as `.cdl` / `.ccc`. The editor surfaces the CDL triplet as the familiar
**Lift / Gamma / Gain** wheels — a display-only reparametrization (Gain ↔ Slope, Lift ↔ Offset,
Gamma ↔ Power). The wire never carries a second Lift/Gamma/Gain field; it is one canonical grade.

## Creative look — a display-space 3D LUT

The grade shapes scene-referred light; a **creative look** is a bounded, display-referred table applied
*after* the view transform, on `[0, 1]` display code values. This is the classic game color LUT (Unity's
`Color Lookup`, Godot's `adjustment_*`, an Unreal color-grading LUT) done in the correct place: the view
transform is a fixed, invertible image-formation step, so a `.cube` look is a code-value grade on top of
it, never a substitute for the grade in front of it.

A `.cube` (`LUT_3D_SIZE 17`/`33`/`65`) imports into the asset catalog as an [`AssetType::Lut`] backed by
an `R16G16B16A16_SFLOAT` **3D** image — half-float, not 8-bit unorm, so smooth skies/skin do not band and
the bake round-trips clean. The tonemap pass samples it **tetrahedrally** (the Resolve / hardware-LUT
blend, six-tetrahedron decomposition of the unit cube) rather than trilinearly, which avoids the green/
magenta hue skew a box interpolation shows along the cube diagonal, so an imported look matches its
source. It rides one op appended to `computeMain` after `tonemapAndEncode`, blended by an intensity dial:
`lerp(display, sampleCubeTetrahedral(lut, saturate(display), size), intensity)`. A size-2 identity ramp
is always bound at binding 2, so intensity `0` is the neutral and there is exactly one tonemap tail — no
"LUT enabled" branch. The slot rides the same `set-color-grading` command (`creativeLutAsset` +
`creativeLutIntensity`), not a second command; `render-stats` reports the resolved asset/intensity/size.

## Baking the look — the log2-shaper `.slut`

A shipped game freezes its look. `bake-look` folds the whole HDR→display tail — grade → view transform →
creative LUT — into a single `33³` table over an OCIO-style **log2 shaper**: because a bounded table
cannot hold unbounded scene radiance, the shaper maps cube coordinate `t ∈ [0, 1]` to scene-linear
radiance across EV `[-14, +11]` anchored at 18% grey, and each node stores the display-referred result of
the full tail. The bake is a **GPU compute pass** (`lut_bake.slang`) over the *same* shared
`tonemap_ops` helpers as the live path — `grade`, `tonemapAndEncode`, `sampleCubeTetrahedral` — read back
through a fenced staging buffer and written as a native `.slut` (`size:33`, `shaperEvMin`, `shaperEvMax`,
`rgb16f`). Baking on the GPU with the shared helpers keeps the frozen look identical to the live editor
look by construction (up to shaper-node quantization) — a CPU re-derivation of the tail would drift on the
next operator tweak.

The exported [`saffron-player`] and any external full-tail `.cube` (a Resolve / OCIO bake) consume the
same `.slut`: the tonemap shader carries a `frozenLook` view-mode flag in the grade UBO — `0` is the live
ALU host (grade → view transform → creative LUT), `1` is the player (a single dependent fetch
`sampleCubeTetrahedral(bakedLut, shaperEncode(exposed), 33)` replacing the ALU tail). One shader file, two
view-modes, one path per binary. A display-domain external `.cube` (the common case) lands in the creative
slot above instead.

### `.cdl`/`.ccc` vs `.cube`/`.slut`

`.cdl`/`.ccc` is the *scene-referred* interchange for the ASC-CDL grade (slope/offset/power + saturation)
— it round-trips the ALU grade to a color suite. `.cube`/`.slut` is the *display-referred* interchange for
the look table: a `.cube` is a bounded creative look, a `.slut` the engine's full-tail shaper bake. They
sit on opposite sides of the view transform and never substitute for one another.

## The Post panel

Bloom and the grade live in a dedicated **Post** panel — a docked Scene panel beside Environment and
Render, not scattered rows on the Render panel (which stays anti-aliasing / quality / resolution / view
transform / FPS / toggles). Two tabs section it: **Bloom** (the [bloom](../bloom/) controls) and
**Grade**. The Grade tab reads its live values from `render-stats.colorGrading`, writes every edit
through the one `set-color-grading` merge, and records one scene-tab undo entry per gesture.

Two widgets give grading its proper affordances:

- **The `GradingWheel` trackball.** A circular hue/saturation pad with a draggable puck plus a vertical
  luma-trim bar — the Resolve/Unreal wheel. It encodes an RGB triplet as a uniform **luma** level (the
  bar) plus a zero-sum **chroma** push (the disc), so a centred disc + a neutral bar is the identity
  correction; the puck's displacement is the CDL delta, not an absolute colour, which is the only
  semantic that composes with the neutral-at-rest grade. The chroma lives in the grey-orthogonal plane
  spanned by two orthonormal RGB basis vectors, so the disc↔triplet map is exact and invertible. The
  **Global** section surfaces Lift/Gamma/Gain as three wheels over the CDL SOP (Lift → `offset`,
  Gamma → `power`, Gain → `slope`), and each tonal range (Shadows/Midtones/Highlights) gets the same
  three wheels plus its saturation/contrast — one widget parameterized by which grade field it patches.
- **The `ToneCurve` spline.** An SVG `[0,1]×[0,1]` display-space curve with draggable control points
  (click to add, drag to move, right-click to remove) and a master + per-channel (R/G/B) selector,
  interpolated by a monotone cubic so a tone curve never overshoots into a contrast reversal. A
  per-channel display-space tone curve *is* a creative LUT, so the editor samples the curve to a small
  `.cube`, imports it with `import-lut`, and assigns the resulting asset to the same creative-LUT slot
  (`creativeLutAsset`) — the curve rides the Phase-5 seam and adds no live grade param, which is why the
  engine tail is unchanged. (A native ALU per-channel `curves` grade op is the more direct long-term
  form; it would only flip the widget's `onChange` target from the bake path to a `set-color-grading`
  `curves` patch, leaving the control-point model the same.)

## The grade uniform

The grade parameters ride a **per-view dynamic-offset uniform buffer** bound at binding 1 of the tonemap
descriptor set (binding 0 is the storage image). One persistent set serves every frame-in-flight: the
renderer writes this frame's `GradeUniform` slice into the mapped buffer, and the dispatch supplies a
`frame · aligned_size` dynamic offset — no per-frame descriptor rewrite, no CPU/GPU race. The transport
is a UBO from the start so later per-range CDL work extends the same layout with no push-size rewrite.

Every knob is one control setting: a single `set-color-grading` command, read back through
`RenderStatsDto.colorGrading`, persisted as the ASC-CDL SOP+Sat block in the project `renderSettings`,
and scriptable from `sa` (`sa set-color-grading --temperature 5000 --contrast 1.2`).

## In the code

| What | File | Symbols |
|---|---|---|
| The grade helper (pure) | `tonemap_ops.slang` | `grade`, `GradeParams`, `GradeBlockParams`, `rangeWeights`, `splitTone` |
| The graded tonemap entry | `tonemap.slang` | `computeMain`, `GradeUniform` (binding 1) |
| CPU grade state + std140 image | `overlay.rs` | `ColorGrade`, `GradeRange`, `GradeUniform`, `bradford_white_balance` |
| Renderer state + per-frame upload | `renderer.rs` | `set_color_grading`, `color_grading`, `add_tonemap_pass` |
| Per-view grade UBO + set write | `view_target.rs` | `grade_ubo`, `write_grade`, `grade_ubo_offset` |
| Descriptor binding + dynamic write | `descriptors.rs` | `create_tonemap_layout`, `write_dynamic_uniform_buffer` |
| Wire DTO + command | `dto.rs`, `commands_render.rs` | `SetColorGradingParams`, `GradeRangeDto`, `SplitToneDto`, `CreativeLutStat`, `BakeLookParams`/`Result`, the `set-color-grading` / `import-lut` / `bake-look` commands |
| Creative LUT sample + shaper | `tonemap_ops.slang` | `sampleCubeTetrahedral`, `lutShaperEncode`/`Decode`, `GradeUniform.look` |
| `.cube` parse + `.slut` codec + import | `cube.rs` | `parse_cube`, `CubeLut`, `BakedLut`, `import_cube_lut`, `load_cube_lut_asset`, `import_baked_lut` |
| GPU LUT resource + upload + bake | `resources.rs`, `upload.rs`, `renderer.rs` | `GpuLut`, `upload_lut_3d`, `upload_identity_lut`, `bake_look_lut`, `set_creative_lut_texture` |
| The bake compute + player branch | `lut_bake.slang`, `tonemap.slang` | `lut_bake` `computeMain`, the `frozenLook` view-mode branch |
| Persistence | `render_settings.rs` | the `colorGrading` block + `creativeLutTexture`/`creativeLutIntensity` in `renderSettings` |
| Editor Post panel + widgets | `PostProcessPanel.tsx`, `GradingWheel.tsx`, `ToneCurve.tsx` | the Bloom/Grade tabs, the Lift/Gamma/Gain + per-range trackballs, the tone-curve → `.cube` → `import-lut` bake |

## Related

- [Tonemapping](../tonemap-and-exposure/) — the view/display transform the grade runs in front of
- [Bloom](../bloom/) — composites into scene-linear `color` before the grade + transform
- [Compute post-process](../compute-post-process-pattern/) — the shared read-modify-write shape
