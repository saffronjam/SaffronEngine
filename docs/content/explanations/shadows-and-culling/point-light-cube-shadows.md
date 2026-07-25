+++
title = 'Point shadows'
weight = 3
math = true
+++

# Point shadows

A point light shadows in every direction, so no single projection covers it. Anima gives the
shadowed point light six projective spaces of [virtual shadow pages](../virtual-shadow-maps/) — one
per cube face, each an 8×8 page grid under a 90° perspective frustum. The faces follow the standard
cube order `+X, -X, +Y, -Y, +Z, -Z`.

> [!NOTE]
> Only the first shadow-casting point light samples its face pages. Ray-query shadows override every
> map-based punctual path when enabled.

## Face spaces

`point_shadow_face_matrices` builds six world-to-clip matrices with a 90° vertical field of view,
aspect 1, near plane `0.05`, and far plane at the light's range. The look-at directions and up
vectors follow the cube sampling convention, with no window Y flip, so a world direction and its
rasterized face texel agree. A moved or re-ranged light invalidates all six spaces at once.

## Face select and depth

Both the demand pass and the sampler pick the face by the dominant axis of the light-to-fragment
vector — the classic cube major-axis mapping. With $d$ the dominant-axis distance and $(s, t)$ the
face-local coordinates, the face NDC is $st/d$ and the compared depth follows the projection's
$[0,1]$ mapping:

$$
z_{01} = rac{f\,(d - n)}{d\,(f - n)}, \qquad n = 0.05 .
$$

```hlsl
if (a.x >= a.y && a.x >= a.z) {
    face = toFrag.x > 0.0 ? 0u : 1u;
    axis = a.x;
    st = float2(toFrag.x > 0.0 ? -toFrag.z : toFrag.z, -toFrag.y);
}
```

`vsmSamplePoint` looks the page up in the table's point region (64 entries per face) and takes the
shared [3×3 tile filter](../pcf-filtering/). A missing page reads unshadowed. Dirty face pages
rasterize in per-face page groups, each culling with its face matrix and drawing pages through
`vsm_page_crop(8, x, y)` times the face transform.

## In the code

| What | File | Symbols |
|---|---|---|
| Six face matrices | `crates/rendering/src/lighting.rs` | `point_shadow_face_matrices` |
| Arm + describe the light | `crates/rendering/src/lighting.rs` | `Lighting::set_point_shadow` |
| Invalidate on movement | `crates/rendering/src/renderer.rs` | `prepare_vsm_frame`, `VsmResidency::invalidate_point` |
| Face page space | `crates/rendering/src/vsm.rs` | `VsmPageKey::PointFace`, `VSM_POINT_FACE_PAGES`, `VSM_POINT_TABLE_BASE` |
| Mark receiver pages | `assets/shaders/vsm_demand.slang` | `computeMain` (point block) |
| Sample the face pages | `assets/shaders/lighting_common.slang` | `vsmSamplePoint` |

## Related

- [Virtual shadow maps](../virtual-shadow-maps/) — the atlas and residency behind the faces
- [Spot-light shadows](../spot-light-shadows/) — the single-frustum analogue
- [Shadow bias](../shadow-bias/) — the depth bias applied while pages rasterize
- [Ray-query shadows](../../global-illumination-and-raytracing/ray-query-shadows/) — the per-light alternative on RT hardware
