+++
title = 'IBL overview'
weight = 1
math = true
+++

# IBL overview

Image-based lighting (IBL) turns an environment into indirect diffuse light and view-dependent
reflections. Instead of assigning one ambient color to every surface, it integrates incoming
environment radiance with the [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/).

The full lighting integral is too costly to evaluate for every fragment. Anima precomputes a small
set of textures whenever the environment changes, then combines their samples with the material's
normal, roughness, metallic value, and view direction.

## Split-sum specular

Reflected environment radiance is the BRDF integrated over the hemisphere:

$$
L_o(v) = \int_\Omega f(l,v)\,L_i(l)\,(n\cdot l)\,dl.
$$

The [split-sum approximation](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf)
separates specular environment lighting into an environment-dependent prefilter and a
material-dependent lookup. The engine stores the first term in a roughness-mipped cubemap and the
second in a two-channel BRDF lookup table.

At shading time, the reflection vector selects a cube direction and perceptual roughness selects a
mip. The BRDF lookup uses $n\cdot v$ and roughness to return scale and bias values:

```hlsl
float3 prefiltered = prefilteredMap.SampleLevel(R, prefilterLod(roughness)).rgb;
float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);
```

The fragment shader also applies GGX multi-scatter energy compensation, horizon occlusion, material
occlusion, GTAO, and reflection-cone sky visibility. Local
[reflection probes](../reflection-probes/), screen-space reflections, and ray-traced reflections can
replace the global prefiltered radiance before those terms are applied.

## Diffuse irradiance

Diffuse IBL uses a cosine-weighted convolution of the environment. This produces an irradiance cube
that needs only the shading normal as its lookup direction. The material response multiplies the
sampled irradiance by albedo and the energy-conserving diffuse factor
$k_d=(1-F)(1-metallic)$.

For opaque surfaces, `gi_resolve.slang` samples the global irradiance cube once per half-resolution
pixel. It applies distance-field sky visibility and replaces the result with
[DDGI](../../global-illumination-and-raytracing/ddgi-overview/) according to probe-cage coverage. The
mesh fragment bilinearly samples this resolved irradiance and applies $k_d$, albedo, and contact AO.

Transparent surfaces cannot use the screen-space resolve because it describes the opaque surface
behind them. They sample the irradiance cube and DDGI directly in `evalLighting`.

## Baked resources

The bake first fills a source environment cube from a procedural sky, an equirectangular panorama,
or the [procedural atmosphere](../procedural-atmosphere/). It generates the source mip chain, then
dispatches the diffuse convolution, one specular prefilter dispatch per mip, and the BRDF integration.

| Resource | Extent | Format | Purpose |
|---|---:|---|---|
| Environment cube | 256×256 per face | RGBA16F | Source radiance and visible procedural sky |
| Irradiance cube | 32×32 per face | RGBA16F | Diffuse hemisphere integral |
| Prefiltered cube | 256×256 per face, 5 mips | RGBA16F | Specular radiance by roughness |
| BRDF LUT | 256×256 | RGBA16F | Fresnel scale and bias |

The irradiance cube, prefiltered cube, and BRDF LUT occupy bindings 0 through 2 of mesh descriptor set
3. Reflection-probe arrays and metadata share bindings 3 through 5 of the same set.

## Bake lifetime

Renderer construction performs a procedural-sky bake before the first frame. A change to the
environment source, panorama, sun, or atmosphere parameters arms another bake at the next GPU-idle
frame boundary. The bake overwrites the persistent images, so descriptor set 3 remains valid.

The master IBL switch is enabled by default. `sa set-ibl 0` selects the flat fallback
`albedo * (1 - metallic) * ambientColor`; `sa set-ibl 1` restores the baked diffuse and specular
paths.

## In the code

| What | File | Symbols |
|---|---|---|
| Resources and bake lifetime | `rendering/src/ibl.rs` | `Ibl`, `Ibl::bake`, `Ibl::request_env_bake`, `EnvSource` |
| Bake dimensions | `rendering/src/ibl.rs` | `IBL_ENV_SIZE`, `IBL_IRRADIANCE_SIZE`, `IBL_PREFILTER_SIZE`, `IBL_PREFILTER_MIPS`, `IBL_LUT_SIZE` |
| IBL descriptor layout | `rendering/src/descriptors.rs` | `create_ibl_layout` |
| Opaque diffuse resolve | `assets/shaders/gi_resolve.slang` | `computeMain`, `indirectOut` |
| Specular and transparent diffuse | `assets/shaders/lighting.slang` | `evalLighting`, `prefilterLod`, `fresnelSchlickRoughness` |
| Runtime toggle | `control/src/commands_render.rs` | `set-ibl` |

## Related

- [Diffuse irradiance](../diffuse-irradiance/) covers the cosine-weighted convolution.
- [Specular prefilter](../specular-prefilter/) covers roughness-filtered environment radiance.
- [BRDF LUT](../brdf-lut/) covers the scale-and-bias integration.
- [IBL bake pass](../ibl-bake-pass/) follows the synchronous compute sequence.
- [Distance field reflection occlusion](../../global-illumination-and-raytracing/distance-field-reflection-occlusion/) covers diffuse and specular sky visibility.
