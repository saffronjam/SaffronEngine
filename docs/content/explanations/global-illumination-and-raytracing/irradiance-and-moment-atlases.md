+++
title = 'Probe atlases'
weight = 5
math = true
+++

# Probe atlases

A probe atlas is an octahedral texture that stores a [DDGI](../ddgi-overview/) probe's filtered
lighting so a shaded point can read it with one bilinear lookup. The volume keeps two: an
irradiance atlas (`rgba16f`) holding directional incoming light, and a moment atlas (`rg16f`)
holding the mean and mean-squared hit distance for the
[Chebyshev visibility test](../probe-volume-and-sampling/). Both are integrated from the per-frame
ray image and blended temporally, after
[Majercik et al. (JCGT 2019)](https://jcgt.org/published/0008/02/01/paper-lowres.pdf).

## Tile layout

Each probe owns one tile in each atlas. A tile is an `interior × interior` block of octahedral
texels plus a one-texel gutter on every side, so a tile spans `interior + 2` texels. The
irradiance interior is 8×8 (`DDGI_IRR_INTERIOR`); the moment interior is 16×16
(`DDGI_DIST_INTERIOR`), because localizing an occluder needs more directional resolution than a
diffuse integral does.

Tiles pack in probe order: probe `(x, y, z)` sits at tile column `x + y·16` and tile row `z`, 128
tiles per row. At the 16×8×16 probe grid that makes the irradiance atlas 1280×160 texels and the
moment atlas 2304×288. A texel's direction comes from the octahedral map, which unfolds the unit
sphere onto a square with no pole singularity
([Cigolle et al., JCGT 2014](https://jcgt.org/published/0003/02/01/paper-lowres.pdf)).

## Only re-traced probes blend

The trace re-rays a rolling window of a quarter of the probes each frame, so each probe carries
fresh rays once per four-frame cycle ([DDGI overview](../ddgi-overview/)). The blend dispatches
still cover the whole atlas, one thread per interior texel, but each thread first asks
`probeTraced`: is this tile inside the frame's trace window? If not it returns, keeping the last
blended value.

The skip is about direction consistency, not just cost. An untraced probe's stored rays were
traced under an earlier ray-set rotation, and re-integrating them against the current rotation
would wobble the probe's value every frame. Gutter texels also early-out in both blend kernels,
which leaves the border pass as their sole writer.

## Integrating rays into a texel

`ddgi_blend_irradiance.slang` maps each interior texel back to a direction
$\mathbf{d}_\text{texel}$ via octahedral decode, then integrates the probe's row of the ray image
(64×2048 `rgba16f`: one column per ray, one row per probe, radiance in rgb and hit distance in
alpha) weighted by the clamped cosine against each ray direction:

$$
E(\mathbf{d}_\text{texel}) = \frac{\sum_r \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)\; L_r}
{\sum_r \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)}
$$

This cosine-weighted gather yields the irradiance a Lambertian surface facing
$\mathbf{d}_\text{texel}$ would receive. The ray image stores no directions; the kernel rebuilds
$\mathbf{d}_r$ from the ray index with the trace's Fibonacci-sphere formula, including the
per-frame golden-ratio rotation $\rho = \operatorname{frac}(f \cdot 0.618)$ (`Ddgi::ray_rotation`),
so the radiance is integrated against the directions it was actually traced along.

## The moment atlas

`ddgi_blend_distance.slang` runs the same gather over the ray hit distances, with a much sharper
weight, and stores two moments per `rg16f` texel:

$$
\overline{r} = \frac{\sum_r w_r\, d_r}{\sum_r w_r}, \qquad
\overline{r^2} = \frac{\sum_r w_r\, d_r^2}{\sum_r w_r}, \qquad
w_r = \max(0,\ \mathbf{d}_\text{texel}\cdot\mathbf{d}_r)^{50}
$$

The power-50 cosine makes a texel pull almost entirely from rays aligned with it, so each
direction localizes its own occluder instead of averaging the hemisphere. Hit distances are
clamped to the volume diagonal (36 m for the 24×12×24 m cage), and a texel no ray aligns with
falls back to $(d_\text{max},\ d_\text{max}^2)$, an unoccluded direction.
[Probe sampling](../probe-volume-and-sampling/) turns the two moments into the Chebyshev variance
bound that stops light leaking through walls.

## Temporal hysteresis

A blended texel is not overwritten outright. The fresh integral is lerped against the previous
value with a high history weight ($\alpha = 0.95$, `DDGI_HYSTERESIS`):

$$
A_t = \operatorname{lerp}(A_\text{new},\ A_{t-1},\ \alpha)
$$

Each blend moves the texel 5% toward the new estimate, which suppresses the noise of 64 rays per
probe: repeated blends converge the atlas to a many-sample result. A probe blends only when its
tile is re-traced, so one convergence step happens per four-frame round-robin cycle, not per
frame.

The irradiance blend also adapts $\alpha$ per texel from the luminance change between the fresh
integral and history. A relative change above 0.25 drops $\alpha$ by 0.15; above 0.80 it zeroes
it. A probe whose lighting genuinely changed therefore snaps in over a few blends instead of
dragging out the flat 20-blend tail.

## History resets

Two conditions force $\alpha = 0$ so no stale history blends in:

- **Whole volume** — the first frame after an enable or a resize sets the blend push's
  first-frame flag (`Ddgi::reset_history`), and every traced texel blends without history. The
  atlases are cleared to zero at creation, so the untraced remainder of the first cycle reads a
  defined value rather than uninitialized memory.
- **Per probe** — when the [camera-centered cage](../probe-volume-and-sampling/) scrolls, a tile
  that entered the volume still holds a scrolled-out probe's far-away radiance. `probeReset`
  compares the probe's logical cell against the snapped volume base from one full round-robin
  cycle ago (the `snap_base_ring` in `ddgi.rs`) and drops history for any newly exposed probe.
  The cycle-long reference matters: a probe that scrolls in may only be re-rayed up to four
  frames later, and a single-frame delta would miss it.

## The octahedral border wrap

Bilinear filtering near a tile's interior edge samples the gutter texels. On an octahedral map
the correct neighbour is the opposite fold of the octahedron, not the adjacent probe's tile, so
`ddgi_border.slang` runs after the irradiance blend and fills each gutter texel from its mirrored
interior source:

- **Corners** copy the diagonally opposite interior corner.
- **Edges** copy the nearest interior row or column with the run reversed
  ($\text{src} = \text{last} - (\ell - 1)$, the octahedron's fold).

Without the wrap, a probe lit from one side shows a dark seam wherever a bilinear tap crosses
into the gutter.

> [!NOTE]
> The border pass fixes only the irradiance atlas. The moment atlas's gutters keep their
> init-clear zeros; `ddgiAtlasUv` in `giprobe.slang` places every sample inside the tile
> interior, and the Chebyshev term tolerates the residual bilinear error at a tile edge.

## In the code

| What | File | Symbols |
|---|---|---|
| Irradiance integration + blend | `ddgi_blend_irradiance.slang` | `computeMain`, `octDecode`, `probeTraced`, `probeReset` |
| Distance moments + sharp weight | `ddgi_blend_distance.slang` | `computeMain`, the `pow(…, 50.0)` weight |
| Octahedral gutter wrap | `ddgi_border.slang` | `computeMain` (corner / edge cases) |
| Tile sizes, formats, hysteresis | `rendering/src/ddgi.rs` | `DDGI_IRR_INTERIOR`, `DDGI_DIST_INTERIOR`, `DDGI_HYSTERESIS`, `DDGI_IRR_FORMAT`, `DDGI_DIST_FORMAT` |
| Blend pushes + scroll ring | `rendering/src/ddgi.rs` | `BlendPush`, `Ddgi::blend_irradiance_push`, `Ddgi::blend_distance_push`, `Ddgi::advance_frame` |
| Blend/border graph passes | `rendering/src/renderer.rs` | `Renderer::add_ddgi_passes` (`ddgi-blend-irr`, `ddgi-blend-dist`, `ddgi-border`) |
| Atlas read-back | `giprobe.slang` | `ddgiAtlasUv`, `ddgiSampleIrradiance` |

## Related

- [Probe sampling](../probe-volume-and-sampling/) — how the atlases are read back, with the Chebyshev math
- [Software ray trace](../software-ray-trace/) — produces the ray image these passes integrate
- [DDGI overview](../ddgi-overview/) — the four-pass pipeline and the round-robin trace budget
