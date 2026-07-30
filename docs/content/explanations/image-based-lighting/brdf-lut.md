+++
title = 'BRDF LUT'
weight = 6
math = true
+++

# BRDF LUT

A BRDF LUT is a precomputed 2D table holding the second factor of the [split-sum approximation](../ibl-overview/): the specular BRDF integrated over the hemisphere with the environment factored out. Each texel stores two values, a **scale** and a **bias**, applied to the prefiltered specular at runtime.

The integral depends only on the viewing angle $n\cdot v$ and the roughness, never on the environment or $F_0$. One bake therefore serves every material in every scene, and the runtime work reduces to a single texture fetch and a multiply-add.

## The two channels

The split sum writes the environment specular as

$$
\text{specularIBL} = \text{prefiltered} \cdot \big(F_0 \cdot \text{scale} + \text{bias}\big)
$$

Scale and bias come from splitting the Fresnel-Schlick term out of the BRDF integral so $F_0$ can be pulled outside:

$$
\int_\Omega f(l,v)\,(n\cdot l)\,dl
= F_0\!\int_\Omega \frac{f}{F}(1-(1-v\cdot h)^5)(n\cdot l)\,dl
+ \int_\Omega \frac{f}{F}(1-v\cdot h)^5(n\cdot l)\,dl
$$

The first integral is the scale (channel R), the second the bias (channel G).

## Integration

`integrateBRDF(ndotv, roughness)` performs the Monte-Carlo integration. It fixes $n = (0,0,1)$, reconstructs a view vector at the given $n\cdot v$, then importance-samples GGX exactly as the [prefilter](../specular-prefilter/) does:

```hlsl
float3 l    = normalize(2.0 * dot(v, h) * h - v);
float g     = geometrySchlickGGX(ndotl, roughness) * geometrySchlickGGX(ndotv, roughness);
float gVis  = g * vdoth / (ndoth * ndotv);
float fc    = pow(1.0 - vdoth, 5.0);   // the Schlick (1 - v·h)^5 factor
a += (1.0 - fc) * gVis;                // scale
b += fc * gVis;                        // bias
```

`fc` is the Schlick exponent term. The scale `a` accumulates `(1 - fc) * gVis` and the bias `b` accumulates `fc * gVis`, exactly the two split integrals above. `gVis` folds the Smith geometry term together with the importance-sampling weight $\tfrac{v\cdot h}{(n\cdot h)(n\cdot v)}$, so no separate PDF division is needed.

The geometry term uses the IBL-specific Smith remap $k = \alpha^2/2$, distinct from the direct-lighting $k$ and the standard choice for environment integration.

## Hammersley sampling

Both the LUT and the prefilter sample with a Hammersley sequence, a quasi-random 2D point set that fills the unit square far more evenly than independent random numbers. As a result 512 samples converge as well as thousands of pseudo-random ones. The first coordinate is $i/N$; the second is the radical inverse of $i$ in base 2, computed by bit-reversal:

```hlsl
float2 hammersley(uint i, uint n) { return float2(float(i) / float(n), radicalInverseVdC(i)); }
```

## The output table

The LUT is a `256²` `R16G16B16A16_SFLOAT` 2D image with only R and G written. The compute shader maps each texel to $(n\cdot v, \text{roughness})$ over $[0, 1]$, both floored to avoid the degenerate edge, and stores the integral into `outLut`. The mesh fragment samples it with `brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg`.

## In the code

| What | File | Symbols |
|---|---|---|
| Integral + geometry term | `engine/assets/shaders/ibl_brdf.slang` | `integrateBRDF` (scale `a`, bias `b`), `geometrySchlickGGX` (IBL $k = \alpha^2/2$) |
| Sampling + output | `engine/assets/shaders/ibl_brdf.slang` | `importanceSampleGGX`, `hammersley`, `radicalInverseVdC`, `computeMain`, `outLut` |
| LUT format + size | `engine/crates/rendering/src/ibl/` | `IBL_LUT_SIZE` (256), `IBL_COLOR_FORMAT` |
| Applied at runtime | `engine/assets/shaders/lighting.slang` | ambient block — `F0 * ab.x + ab.y` |

## Related

- [IBL overview](../ibl-overview/) — where scale/bias enter the split-sum
- [Specular prefilter](../specular-prefilter/) — the other split-sum factor, shares the GGX/Hammersley helpers
- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — the BRDF this LUT integrates
