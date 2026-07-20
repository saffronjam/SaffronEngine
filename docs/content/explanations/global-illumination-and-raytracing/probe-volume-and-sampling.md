+++
title = 'Probe sampling'
weight = 3
math = true
+++

# Probe sampling

Probe sampling reconstructs the diffuse indirect light at a surface point from the
[DDGI](../ddgi-overview/) probe cage: the eight probes of the grid cell containing the point are
blended, weighted so a probe behind a wall or facing away from the surface cannot leak light in.
The weighting follows
[Majercik et al. (JCGT 2019)](https://jcgt.org/published/0008/02/01/paper-lowres.pdf), with the
surface bias and low-weight crush from the same authors' production paper,
[Majercik et al. (JCGT 2021)](https://jcgt.org/published/0010/02/01/paper-lowres.pdf).

One function does the whole blend: `ddgiSampleIrradiance` in the `giprobe` Slang module. The
module declares no bindings of its own; each consumer passes the two probe atlases plus a
`DdgiVolume` struct describing this frame's cage placement. The half-res `gi_resolve` pass fills
that struct from its `GiParams` UBO, and the mesh fragment in `lighting.slang` fills it from the
light globals via `ddgiVolumeFromGlobals`, with the atlases bound at descriptor set 5.

## The camera-centered cage

The grid holds `DDGI_PROBES_X × DDGI_PROBES_Y × DDGI_PROBES_Z` = 16×8×16 = 2048 probes at a fixed
1.5 m spacing (`DDGI_PROBE_SPACING`), a 24×12×24 m box that follows the camera rather than a fit
to the scene. Each frame `Ddgi::set_scene` snaps the box to the probe grid:

$$
\mathbf{b} = \operatorname{round}\!\big(\mathbf{x}_\text{cam} / s\big) - \tfrac{\mathbf{N}}{2},
\qquad
\mathbf{v}_\text{min} = \mathbf{b}\, s, \qquad \mathbf{v}_\text{ext} = \mathbf{N} s
$$

where $s$ is the spacing, $\mathbf{N}$ the per-axis probe count, and $\mathbf{b}$ the snapped base
cell (`snap_base`). Snapping to whole cells keeps the field from shimmering as the camera creeps.
Probes sit at cell centers: logical probe $\mathbf{p}$ is at
$\mathbf{v}_\text{min} + \frac{\mathbf{p} + 1/2}{\mathbf{N}}\,\mathbf{v}_\text{ext}$.

The sampler maps the query point to the probe-space coordinate
$\mathbf{g} = \mathbf{t}\,\mathbf{N} - \tfrac12$, where $\mathbf{t}$ is the point's normalized
position inside the volume. The $-\tfrac12$ matches the cell-centered layout: a point at a probe's
center lands on an integer coordinate. The floor of $\mathbf{g}$ picks the cell's base corner and
the fraction $\mathbf{f}$ drives the trilinear weights.

## Surface bias

The lookup does not use the raw surface point. The query is nudged off the surface along a blend
of the normal $\mathbf{n}$ and the view direction $\mathbf{v}$:

$$
\mathbf{x}_b = \mathbf{x} + (0.2\,\mathbf{n} + 0.8\,\mathbf{v}) \cdot 0.75\, s_\text{min} \cdot 0.3
$$

With the 1.5 m spacing that is at most ≈ 0.34 m. A raw surface point sits exactly on the probes'
self-visibility boundary, where the Chebyshev test has its highest variance, and the interpolation
shows cell-sized sliding lobes on flat floors. The 2021 paper calls this the self-shadow bias.

## The toroidal tile fold

Because the cage scrolls with the camera, a probe's logical index is not where its data lives in
the atlas. The physical tile of logical probe $\mathbf{p}$ is
$\operatorname{wrapMod}(\mathbf{p} + \mathbf{b}_s, \mathbf{N})$, where
$\mathbf{b}_s = \operatorname{wrapMod}(\text{snapBase}, \mathbf{N})$ is the scroll base
(`Ddgi::scroll_base_ubo`, carried in `DdgiVolume::scrollBase`):

```hlsl
uint3 physTile = uint3((pi + int3(vol.scrollBase.xyz)) % int3(pc));
```

A probe that stays inside the volume as it recenters keeps its physical tile, and with it its
converged temporal history; only probes that scroll in are reset and re-rayed. The sampler applies
the fold before every atlas read, so the irradiance and moment tiles it fetches belong to the
logical cell the surface falls in.

## Octahedral atlas reads

A probe's directional data lives on a 2D tile, so a unit direction must map to $[0,1]^2$. The
octahedral map projects the sphere onto an octahedron, unfolds the top half to a square, and folds
the corners over for the lower hemisphere
([Cigolle et al., JCGT 2014](https://jcgt.org/published/0003/02/01/paper-lowres.pdf)).
`ddgiOctEncode` computes

$$
\mathbf{d}' = \frac{\mathbf{d}}{|d_x| + |d_y| + |d_z|}, \qquad
\mathbf{o} =
\begin{cases}
\mathbf{d}'_{xy} & d_z \ge 0 \\[4pt]
\big(1 - |\mathbf{d}'_{yx}|\big)\operatorname{sign}(\mathbf{d}'_{xy}) & d_z < 0
\end{cases}
$$

remapped to $[0,1]^2$ by $\mathbf{o} \cdot 0.5 + 0.5$. `ddgiAtlasUv` then places the sample in the
probe's tile: column $p_x + p_y \cdot 16$, row $p_z$, each tile `interior + 2` texels wide to
leave a one-texel gutter (see [probe atlases](../irradiance-and-moment-atlases/)). Irradiance is
read in the surface-normal direction from the 8×8-interior atlas; the moments are read in the
probe-to-surface direction from the 16×16-interior atlas.

## The three weights

Each corner probe's weight is a product of three terms. Trilinear interpolation alone is smooth
but leaks: a probe inside a wall blends its wall-side irradiance onto surfaces in the next room.
The backface term removes probes the surface faces away from; the Chebyshev term removes probes
that face the surface but only across an occluder.

**Trilinear.** Corner $c$ with per-axis offset $\mathbf{o}_c \in \{0,1\}^3$ gets the standard
partition-of-unity weight from the cell fraction $\mathbf{f}$:

$$
w_\text{tri} = \prod_{k \in \{x,y,z\}}
\big( (1 - o_{c,k})(1 - f_k) + o_{c,k}\, f_k \big)
$$

**Soft backface.** The weight is scaled by a squared wrap-cosine of the angle between the surface
normal and the direction to the probe, with an additive 0.2 floor:

$$
w \mathrel{*}= \Big(\tfrac12 (\hat{\mathbf{d}}_p \cdot \mathbf{n}) + \tfrac12\Big)^{2} + 0.2
$$

**Chebyshev visibility.** The moment atlas stores the mean and mean-squared ray hit distance per
direction. For a surface at distance $d$ from the probe with $d > \overline{r}$, the contribution
is attenuated by the one-tailed variance bound that
[variance shadow maps](https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-8-summed-area-variance-shadow-maps)
use, cubed to sharpen the falloff:

$$
\sigma^2 = \big|\, \overline{r}^{\,2} - \overline{r^2} \,\big|, \qquad
w \mathrel{*}= \left( \frac{\sigma^2}{\sigma^2 + (d - \overline{r})^2} \right)^{3}
$$

A surface closer than the mean hit distance is fully visible and skips the test. Finally a
low-weight crush squares any weight below 0.2 (as $w \cdot (w/0.2)^2$), so a barely-contributing
corner cannot smear a soft lobe across the cell.

## Irradiance and coverage

The returned color is the weighted average of the eight corners' irradiance tiles, each sampled in
the surface-normal direction:

$$
E(\mathbf{x}, \mathbf{n}) = \frac{\sum_c w_c \, E_c(\mathbf{n})}{\sum_c w_c}
$$

The alpha channel carries coverage, a separate sum: the trilinear-only mass of the corners whose
cell genuinely lies inside the cage. A corner clamped to the volume edge still feeds the color
average, so the irradiance stays defined up to the boundary, but it does not count toward
coverage.

Deep inside the cage the trilinear weights sum to 1; at the boundary coverage ramps to 0, and
consumers lerp the DDGI result over the analytic IBL diffuse by it, so surfaces outside the
camera-centered cage fall back cleanly. Folding the backface or Chebyshev terms into coverage
would pull it below 1 indoors and leak the skybox back in.

## In the code

| What | File | Symbols |
|---|---|---|
| Eight-probe blend, bias, coverage | `giprobe.slang` | `ddgiSampleIrradiance`, `DdgiVolume` |
| Octahedral encode + atlas UV | `giprobe.slang` | `ddgiOctEncode`, `ddgiAtlasUv` |
| Volume snap, counts, scroll base | `rendering/src/ddgi.rs` | `Ddgi::set_scene`, `Ddgi::probe_count_ubo`, `Ddgi::scroll_base_ubo`, `DDGI_PROBES_X/Y/Z`, `DDGI_PROBE_SPACING` |
| Mesh consumer (set 5 atlases) | `lighting.slang` | `ddgiVolumeFromGlobals`, `ddgiIrradiance`, `ddgiDistance` |
| Half-res consumer | `gi_resolve.slang` | `GiParams`, `computeMain` |

## Related

- [Probe atlases](../irradiance-and-moment-atlases/) — what the sampled tiles store and how they blend
- [DDGI overview](../ddgi-overview/) — where the sample sits in the frame and what coverage gates
- [Software ray trace](../software-ray-trace/) — the trace that produces the hit distances behind the moments
