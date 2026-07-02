+++
title = 'Probe atlases'
weight = 5
math = true
+++

# Probe atlases

A probe atlas is an octahedral texture that stores a DDGI probe's filtered lighting so a mesh
fragment can read it with one lookup. Each probe volume keeps two: an **irradiance** atlas, holding
directional incoming light, and a **moment** atlas, holding mean and mean-squared hit distance for
the Chebyshev visibility test. Both are integrated from the per-frame ray image, blended temporally,
and gutter-fixed so bilinear sampling never reads across a tile edge.

Each probe occupies one tile in each atlas. A tile is an `interior × interior` block of octahedral
texels plus a one-texel gutter on every side, giving a full size of `interior + 2`. The irradiance
atlas uses an 8×8 interior (`DDGI_IRR_INTERIOR`); the moment atlas uses 16×16 (`DDGI_DIST_INTERIOR`),
because distance needs more directional resolution to localize occluders.

## Integrating rays into a texel

`ddgi_blend_irradiance.slang` runs one thread per interior texel. The texel maps back to a
direction $\mathbf{d}_\text{texel}$ via octahedral *decode*, and the shader integrates every ray
weighted by the clamped cosine between the texel direction and the ray direction:

$$
E(\mathbf{d}_\text{texel}) = \frac{\sum_r \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)\; L_r}
{\sum_r \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)}
$$

This cosine-weighted hemispherical gather yields the irradiance a Lambertian surface facing
$\mathbf{d}_\text{texel}$ would receive. The shader rebuilds each ray's direction with the same
Fibonacci formula the trace used.

## The moment atlas

`ddgi_blend_distance.slang` is the same gather, but it stores distance moments with a much sharper
weight. For each texel it accumulates the mean and mean-squared hit distance:

$$
\overline{r} = \frac{\sum_r w_r\, d_r}{\sum_r w_r}, \qquad
\overline{r^2} = \frac{\sum_r w_r\, d_r^2}{\sum_r w_r}, \qquad
w_r = \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)^{50}
$$

The power-50 cosine makes each texel pull almost entirely from rays aligned with it, so a distance
texel localizes the occluder in *its* direction rather than averaging the whole hemisphere. The two
moments ($\overline{r}$, $\overline{r^2}$) are stored in an `rg16f` texel, and
[probe sampling](../probe-volume-and-sampling/) reads them back for the Chebyshev variance bound.

## Temporal hysteresis

Neither atlas is overwritten outright. The new integral is lerped against the previous frame's
value with a high history weight ($\alpha = 0.95$, `DDGI_HYSTERESIS`):

$$
A_t = \operatorname{lerp}(A_\text{new},\ A_{t-1},\ \alpha)
$$

so each frame nudges the atlas 5% toward the new estimate. This suppresses the noise of only 64 rays
per frame: over ~20 frames the volume converges to a smooth, many-sample result. On the first frame
after enable or resize, the blend push's history-reset flag forces $\alpha = 0$ so no stale history
blends in.

Because the cage is a [camera-centered clipmap](../probe-volume-and-sampling/), the blend also forces
$\alpha = 0$ **per probe** for a tile that scrolled into the volume this frame *and* was re-rayed
this frame — it computes the probe's logical cell from the scroll base + delta, and only resets a
freshly-relocated tile once fresh rays for its new position exist (so it never snaps to the
scrolled-out probe's far-away radiance). This is the same one-shot reset, generalized from the whole
volume to the in-scrolled slab, so the rest of the cage keeps converging while the player walks.

## The octahedral border wrap

Hardware bilinear filtering across a tile's interior samples its edge texels, which on an
octahedral map have to wrap to the *opposite* fold of the octahedron, not to the neighbouring tile.
`ddgi_border.slang` runs after the irradiance blend and copies each gutter texel from its mirrored
interior source:

- **Corner** texels copy from the diagonally-opposite interior corner.
- **Edge** texels copy from the same edge with the run reversed (the octahedron's fold mirrors the
  coordinate: $\text{src} = \text{last} - (\ell - 1)$).

Without this wrap, a probe lit from one direction shows a dark seam where bilinear sampling crosses
the tile edge into the gutter. The blend passes early-out on the gutter texels, leaving the border
pass as their sole writer.

## In the code

| What | File | Symbols |
|---|---|---|
| Irradiance integration + blend | `ddgi_blend_irradiance.slang` | `computeMain`, `octDecode` |
| Distance moments + sharp weight | `ddgi_blend_distance.slang` | `computeMain`, the `pow(..., 50.0)` weight |
| Octahedral gutter wrap | `ddgi_border.slang` | `computeMain` (corner / edge cases) |
| Tile sizes + hysteresis | `rendering/src/ddgi.rs` | `DDGI_IRR_INTERIOR`, `DDGI_DIST_INTERIOR`, `DDGI_HYSTERESIS` |
| Blend/border graph passes | `rendering/src/renderer.rs` | `ddgi-blend-irr`, `ddgi-blend-dist`, `ddgi-border` |

> [!NOTE]
> The border pass only fixes the *irradiance* atlas. The moment atlas's Chebyshev read in
> `lighting.slang` samples interior texels directly via `ddgiAtlasUv`, so it tolerates its gutter being
> unwrapped — the distance term is robust to the small bilinear error at tile edges.

## Related

- [Probe sampling](../probe-volume-and-sampling/) — how the atlases are read back, with the Chebyshev math
- [Software ray trace](../software-ray-trace/) — produces the ray radiance + distance these integrate
- [DDGI overview](../ddgi-overview/) — where the blend/border passes sit in the frame
