+++
title = 'Probe sampling'
weight = 3
math = true
+++

# Probe sampling

Probe sampling reconstructs the indirect light at a surface point from a regular grid of irradiance
probes. DDGI stores that grid as a 16×8×16 cage of probes centered on the camera, each probe holding
a full sphere of irradiance. To shade a fragment, the eight probes of the cell containing the point
are blended, weighted so probes behind walls or behind the surface do not leak in.

The blend combines three weights: a trilinear interpolation, a backface term, and a Chebyshev
visibility test. Together they keep indirect light from bleeding through geometry, which is the
classic DDGI failure mode. `ddgiSampleIrradiance` in `lighting.slang` performs the blend.

## The camera-centered cage

The grid holds `DDGI_PROBES_X × DDGI_PROBES_Y × DDGI_PROBES_Z` = 16×8×16 = 2048 probes at a fixed
1.5 m spacing (`DDGI_PROBE_SPACING`), so the volume is a fixed-size box that follows the camera, not
a fit to the scene. The logical probe $p$ sits at

$$
\mathbf{x}_p = \mathbf{v}_\text{min} + \frac{\mathbf{p} + \tfrac12}{\mathbf{N}}\,\mathbf{v}_\text{ext}
$$

where $\mathbf{v}_\text{min} = \text{snapBase}\cdot\text{spacing}$ snaps the box's min corner to the
probe grid (so the cage centres on the camera without shimmering sub-cell), $\mathbf{v}_\text{ext} =
\mathbf{N}\cdot\text{spacing}$, and $\mathbf{N}$ is the per-axis probe count. Each probe stores its
full sphere of irradiance octahedral-encoded into a small atlas tile.

## The toroidal tile fold

Because the cage scrolls with the camera, a probe's *logical* index within the volume is not where
its data lives in the atlas. A probe is addressed **toroidally**: the physical atlas tile of logical
probe $p$ is $\operatorname{wrapMod}(p + \text{scrollBase}, \mathbf{N})$, with $\text{scrollBase} =
\operatorname{wrapMod}(\text{snapBase}, \mathbf{N})$ folded into the light UBO
(`ddgiScrollBase`). This is what lets a probe that stays in view keep its converged history as the
volume recenters — only the slab that scrolls in is re-rayed and reset. `ddgiSampleIrradiance`
applies the fold before reading each corner probe's irradiance + moment tiles, so the sampling reads
the right physical tile for the logical cell the surface falls in.

## Octahedral encoding

A probe's directional data lives on a 2D tile, so a unit direction must map to $[0,1]^2$. The
octahedral map projects the sphere onto an octahedron, unfolds it to a square, and folds the
corners for the lower hemisphere. `ddgiOctEncode` is

$$
\mathbf{d}' = \frac{\mathbf{d}}{|d_x| + |d_y| + |d_z|}, \qquad
\mathbf{o} =
\begin{cases}
\mathbf{d}'_{xy} & d_z \ge 0 \\[4pt]
\big(1 - |\mathbf{d}'_{yx}|\big)\,\operatorname{sign}(\mathbf{d}'_{xy}) & d_z < 0
\end{cases}
$$

then remapped to $[0,1]^2$ by $\mathbf{o}\cdot 0.5 + 0.5$. Every direction lands in the square with
no pole singularity, and the mapping is cheap, so every DDGI shader uses it. The atlas UV also
steps over the per-tile gutter — see [the atlases](../irradiance-and-moment-atlases/).

## Trilinear cell blend

The surface point maps to a fractional probe-space coordinate, whose integer floor is the base
corner of the cell. The eight corners are weighted trilinearly: corner $c$ with per-axis offset
$\mathbf{o}_c \in \{0,1\}^3$ gets

$$
w_\text{tri} = \prod_{k \in \{x,y,z\}}
\big( (1 - o_{c,k})(1 - f_k) + o_{c,k}\, f_k \big)
$$

where $\mathbf{f}$ is the fractional position in the cell. This is the standard trilinear weight,
the smooth base of the blend.

## Backface weight

A probe on the far side of the surface should not contribute. Each corner's weight is scaled by a
softened cosine between the surface normal $\mathbf{n}$ and the direction to the probe, floored so
a probe is never fully killed:

$$
w \mathrel{*}= \max\!\Big(0.05,\; \tfrac12\,(\hat{\mathbf{d}}_p \cdot \mathbf{n}) + \tfrac12\Big)
$$

## Chebyshev visibility

The Chebyshev term is the leak-killer. Each probe stores, per direction, the mean and mean-squared
hit distance in the moment atlas. For a surface at distance $d$ from the probe, a $d$ that exceeds
the stored mean distance means the surface is probably behind an occluder, so its contribution is
attenuated by a Chebyshev variance bound:

$$
\sigma^2 = \big| \,\overline{r}^{\,2} - \overline{r^2}\, \big|, \qquad
p(\text{visible}) = \frac{\sigma^2}{\sigma^2 + (d - \overline{r})^2}
$$

This is the same one-tailed Chebyshev inequality variance shadow maps use. The result is cubed to
sharpen the falloff, and applied only when $d > \overline{r}$, since a surface closer than the mean
is fully visible. The final irradiance is the weighted sum of each corner probe's irradiance,
sampled in the surface-normal direction and divided by the total weight:

$$
E(\mathbf{x}, \mathbf{n}) = \frac{\sum_c w_c \, E_c(\mathbf{n})}{\sum_c w_c}
$$

## Why all three weights

Trilinear interpolation alone is smooth but leaks: a probe inside a wall blends its wall-side
irradiance onto a surface in the next room. Backface culling removes probes the surface cannot
face. Chebyshev removes probes that *can* face the surface but only across an occluder. Together
they keep indirect light from bleeding through geometry, the failure mode the moment atlas exists
to fix.

## In the code

| What | File | Symbols |
|---|---|---|
| Eight-probe blend + toroidal fold | `lighting.slang` | `ddgiSampleIrradiance` (the `physTile` wrap) |
| Octahedral encode | `lighting.slang` | `ddgiOctEncode` |
| Atlas UV (interior + gutter) | `lighting.slang` | `ddgiAtlasUv` |
| Probe count / spacing / scroll base | `rendering/src/ddgi.rs` | `DDGI_PROBES_X/Y/Z`, `DDGI_PROBE_SPACING`, `Ddgi::probe_count_ubo`, `Ddgi::scroll_base_ubo` |
| Moment atlas read | `lighting.slang` | the `distAtlas` sample + Chebyshev block in `ddgiSampleIrradiance` |

## Related

- [Probe atlases](../irradiance-and-moment-atlases/) — what the moments are built from
- [DDGI overview](../ddgi-overview/) — where this sampling sits in the frame
- [Directional shadows](../../shadows-and-culling/directional-shadows/) — the variance-bound idea, in a shadow map
