# Phase 5 — Volumetric cloud lighting: energy-conserving raymarch and temporal reconstruction

**Status:** COMPLETED

Part of `plans/sky-and-volume/` (dynamic sky, time-of-day, and volumetric clouds). This is the marquee, risky phase of the cloud track: it **lights and marches** the density field Phase 4 authored, decoupled from the dynamic-sky wins of Phases 1–3. A cloud raymarch compute pass marches the Phase-4 density field into a reduced-res, device/view-owned buffer with SDF / adaptive empty-space skipping and an opacity early-out; each sample is lit with energy-conserving multi-octave multiple scattering, a physically-fit Mie phase, a cone-sampled sun-transmittance march, and ambient from the live sky-light diffuse; the march uses the Hillaire analytic slice so lighting is step-count-independent, and a per-pixel blue-noise/Halton ray-start offset so the low sample count reads as noise the temporal pass can resolve. Quarter-res temporal reconstruction (1-of-16 pixels/frame, 4×4) reprojects history by the previous/current camera matrices — **reusing the built `ViewTarget` `prev_view_proj`/`jitter`/`history` and the motion target** — with an EMA blend, disocclusion fallback, and neighborhood clamp; a depth-aware bilateral upscale to full res emits a transmittance-weighted mean cloud front depth. Clouds composite **premultiplied into scene-linear HDR before bloom**.

It depends on **Phase 1** (the sun `DirectionalLight` color/intensity read from the Transmittance LUT, plus the moon light — the cloud march reads that one physically-coupled key light and the same Transmittance LUT for its atmospheric sun transmittance) and **Phase 4** (the `CloudSettings` scene block + `set-clouds` command, the tiling noise volumes as persistent `Image3D`, the weather map, and the density-sampler shader seam this phase calls into). It does **NOT** rebuild the Hillaire sky LUT chain, the aerial-perspective froxel, the froxel-fog stage, the 3D-transient infra, the motion-vector prepass, or the TAA history — all shipped. It reconciles with those; it does not duplicate them.

## What this phase does NOT do (the seam left for Phase 6)

The one thing this phase deliberately leaves clean is the **transmittance-ledger reconciliation**. Aerial perspective already tints scene geometry through the 32³ `AerialPerspective` froxel on the shared `(inscatter, transmittance)` ledger, and the froxel/analytic fog folds onto that same ledger in `height_fog.slang`. Clouds sit far *beyond* the ~32 km AP froxel, so making them receive AP by sampling the shared LUTs at the cloud hit distance — plus the top-down cloud shadow map, the god-rays through `fog_inject`, and the single `T_total = T_fog · T_aerial · T_cloud` fold — is **Phase 6**. This phase composites clouds premultiplied into `color` **after** the existing fog/AP composite and **before** bloom, an honest interim that never double-darkens: fog/AP shade the scene, then clouds land over the sky on top of the already-fogged HDR, and Phase 6 *relocates* that composite into the `height_fog.slang` ledger fold (moving it, not adding a second one). The composite is written to make that relocation a move, not a rewrite: it already emits the transmittance-weighted mean cloud front depth Phase 6's depth compositing needs.

## Goal

Populate the atmosphere Phases 1–3 made dynamic with lit, temporally-stable volumetric clouds that sit in the sky and shade correctly against a moving, physically-coupled sun — at a real-time budget (Horizon-class clouds are ~2 ms; the sky LUTs are microseconds). Concretely:

- **A cloud raymarch compute pass (`cloud_raymarch.slang`, new)** — one thread per reduced-res pixel: build the view ray, intersect the cloud shell (Phase-4 layer altitude/height), sphere-trace a coarse cloud SDF / conservative-density gate to skip empty space, and adaptively march the density field, backing off to short lit steps inside the isosurface and returning to long steps after several empty samples, with an opacity (accumulated-alpha) early-out. Bounded at the near end by the scene depth so geometry occludes clouds.
- **Per-sample lighting** — energy-conserving multi-octave multiple scattering (N = 2 octaves, `σ_s·aⁿ / σ_e·bⁿ / phase(θ·cⁿ)`, `a ≤ b`) replacing any powder hack; a physically-fit Mie phase (the Jendersie & d'Eon 2023 HG-Draine fit, one droplet-diameter parameter, analytic — the modern-correct choice over a hand-tuned dual-g pair); a ~6-tap cone-sampled sun-transmittance march with Beer-Lambert extinction; and ambient from the live sky-light diffuse with a vertical gradient + ground-bounce bias; combined with the Hillaire analytic slice `S_int = (L − L·T)/σ_t` so total lighting is step-count-independent.
- **Blue-noise / Halton ray-start jitter + quarter-res temporal reconstruction** — a per-pixel 4×4 blue-noise / Halton ray-start offset (reusing the frame's TAA `jitter_index`), the reduced-res buffer marched 1-of-16 pixels per frame, and a reconstruction pass reprojecting the previous accumulation by the prev/cur camera matrices with an EMA blend, disocclusion fallback, and neighborhood clamp — reusing `ViewTarget.prev_view_proj` / `jitter` / `history` (the parity + `history_valid`) and the motion target.
- **Depth-aware bilateral upscale to full res** — reject reduced-res taps whose depth disagrees with the full-res depth, and output a transmittance-weighted mean cloud front depth `Σ Tr·z / Σ Tr` (skipping fully-transparent samples) for Phase-6 depth compositing.
- **Premultiplied composite into scene-linear HDR before bloom** — `color.rgb = color.rgb · T_cloud + scatter_cloud` (scatter premultiplied, front-to-back), depth-gated against the scene, landing before the bloom pyramid so distant highlights bloom through the already-clouded HDR.
- **A `submit_clouds` seam + the cloud lighting/temporal settings** — `SceneRenderer::submit_clouds(&CloudRenderSettings)` (the mock too), the lighting/march/temporal knobs added to the Phase-4 `CloudSettings` + `SetCloudsParams` (primary/light steps, droplet diameter, temporal factor), scriptable via the existing `set-clouds` command from `sa`, surfaced in the Environment panel, with the docs page + hub row updated.

## Design stance (grounded in current engine practice)

The engine already ships every seam this phase rides. The temporal + motion pipeline the reconstruction reuses is in `ViewTarget`: `motion`/`motion_depth`, the `history` ping-pong keyed by `history_index` parity with `history_valid`, `prev_view_proj`/`prev_view_proj_valid`, and `jitter`/`jitter_index`/`prev_jitter` — all written by `add_motion_pass`/`add_taa_pass` and already effectively always allocated in interactive views. The 3D volume + import machinery the density and noise reads need is `Image3D` + `RenderGraph::import_image_3d` + `alloc_external_layout`, exercised by `AerialPerspective` (`volume_import` → `import_image_3d` → a `StorageImageRwCompute` fill pass → a barrier-only `SampledReadCompute` read pass → layout write-back). The physically-coupled sun and the atmosphere LUTs the cloud lighting reads are the same ones the AP fill binds: `Ibl::transmittance_view`, `baked_sun`, `atmosphere_live` (and `AerialPerspective::bind_luts` is the exact bind template). And the "composite a reduced buffer into scene-linear HDR in place before bloom" shape is `add_fog_pass` verbatim. So this phase is *assembly plus one genuinely new capability* — a lit cloud raymarch and its temporal reconstruction — not novel plumbing.

**Marching — SDF / adaptive empty-space skipping, not a uniform fixed-step march.** Empty-space skipping does not work out of the box on a continuously-varying medium, so a naïve fixed-step march burns almost all samples on empty sky. The modern-correct destination sphere-traces a coarse cloud SDF (or gates on a conservative low-detail density) and only pays the expensive up-rez density + lit sample inside the isosurface, returning long steps after several empty samples and early-outing once accumulated alpha saturates (Nubis Cubed 2023, https://advances.realtimerendering.com/s2023/Nubis%20Cubed%20(Advances%202023).pdf; UE5 Conservative Density, https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-material-in-unreal-engine). The cheap uniform march is not offered beside it.

**Lighting — energy-conserving multi-octave multiple scattering + the Hillaire analytic slice, not single-scatter Beer × powder.** Thick clouds are bright and white *because* of multiple scattering; single-scatter clouds look like dark smoke, and the "powder-sugar" dark-edge hack double-darkens when the sun is behind the camera (Beer and the phase already model attenuation and directionality). The modern-correct replacement runs the single-scatter integrand N times with progressively lower extinction, lower scattering contribution, and a flatter phase — `L_ms = Σₙ scatter(σ_s·aⁿ, σ_e·bⁿ, phase(θ·cⁿ))`, energy-conserving iff `a ≤ b` — and Frostbite ships N = 2 (Wrenninge 2013, https://history.siggraph.org/learning/art-directable-multiple-volumetric-scattering-by-wrenninge/; Hillaire, Frostbite 2016, https://media.contentapi.ea.com/content/dam/eacom/frostbite/files/s2016-pbs-frostbite-sky-clouds-new.pdf). The slice add uses the closed-form `integScatt = (L − L·T)/max(σ_t, ε)` so brightness is step-count-independent — the *identical* energy-conserving slice `fog_integrate.slang` already uses for froxel fog (Hillaire 2015). This is why the march can drop toward ~21 view samples and still converge.

**Phase — the Jendersie & d'Eon 2023 HG-Draine Mie fit, one droplet-diameter parameter.** A single forward HG lobe leaves the anti-sun side dull and reproduces silver lining poorly at extreme `g`; the classic fix is a dual-lobe `lerp(HG(θ,g₀), HG(θ,g₁), w)`. The modern-correct choice supersedes that hand-tuned pair with the NVIDIA HG-Draine analytic Mie fit: blend an HG forward peak with a Draine bulk lobe to match ~95 % of tabulated Mie for water droplets, with a single intuitive parameter (droplet diameter `d` in µm, valid 5 < d < 50) and analytic evaluation + importance sampling — no lookup tables (https://research.nvidia.com/labs/rtr/approximate-mie/publications/approximate-mie.pdf). One knob replaces two; the dual-g pair is the fallback it retires, not a parallel path.

**Temporal — quarter-res 4×4 reconstruction riding the built motion + TAA rig, not a full-res per-frame march.** Full-res per-frame marching is 100–300 ms class; the shippable form renders the clouds into a reduced buffer, marches 1-of-16 pixels per frame in a 4×4 pattern, reconstructs the full reduced frame over ~16 frames by reprojecting the previous accumulation through the prev/cur camera matrices with an EMA blend, and depth-aware upsamples to full res (bitsquid, http://bitsquid.blogspot.com/2016/07/volumetric-clouds.html; Vertex Fragment, https://www.vertexfragment.com/ramblings/volumetric-cloud-upsampling/). Because clouds are far, their motion is essentially camera-rotation, so they slot into the existing motion + history state directly; the per-pixel blue-noise/Halton ray-start jitter (HZD 2015, https://advances.realtimerendering.com/s2015/The%20Real-time%20Volumetric%20Cloudscapes%20of%20Horizon%20-%20Zero%20Dawn%20-%20ARTR.pdf; Studio Gobo 2016, https://arxiv.org/pdf/1609.05344) is what lets the low step count read as resolvable noise. Disoccluded pixels fall back to the fresh march; a neighborhood clamp suppresses ghosting on fast turns.

> **scene resolve (`color` = scene-linear HDR incl. sky) → cloud SDF/adaptive raymarch (fresh 1/16 pixels, lit + energy-conserving-integrated into the reduced buffer) → temporal reconstruct (reproject history by prev/cur camera, EMA blend, disocclusion + clamp) → depth-aware bilateral upscale (+ mean front depth) → premultiplied cloud composite into `color` → bloom → tonemap.** The reduced buffers are per-view `ViewTarget` images (like `motion`/`history`); the noise/density/LUT/SH reads are device-shared. Everything stays scene-linear `rgba16f` before display space is ever touched.

## NO-LEGACY checklist for this phase

- **The cloud phase is one parameter.** The Jendersie & d'Eon HG-Draine fit (one `droplet_diameter`) is *the* phase function — there is no second dual-g pair, no separate silver-lining slider, no powder term. Multiple scattering is the energy-conserving octave sum, not a powder darkening hack.
- **One composite, one write path.** Clouds composite in exactly one place (`cloud_upscale.slang`'s in-place premultiplied blend into `color`), and Phase 6 *relocates* it into the `height_fog.slang` ledger — it does not add a second composite. There is no parallel screen-space cloud path.
- **The temporal state is reused, not duplicated.** The reconstruction reads the existing `ViewTarget.prev_view_proj`/`jitter`/`history`/`history_index`/`history_valid` and the `motion` target; it does not stand up a second history ping-pong or a second jitter sequence. The reduced-res cloud buffers are new per-view `ViewTarget` images allocated alongside the screen-space sets, imported per frame like every other target.
- **The lighting inputs are the shipped ones.** The sun key light is the Phase-1 `baked_sun()` (Transmittance-LUT-coupled) directional; the atmospheric sun transmittance is the same `Ibl::transmittance_view` the AP fill binds; the ambient is the live sky-light diffuse (the Phase-2 SH set when that phase has landed, otherwise `Ibl::irradiance_cube_view`) — one ambient source, one binding, never a flat authored cloud color.
- **The settings extend the Phase-4 block; no new command.** `set-clouds` already exists (Phase 4). This phase only adds fields to `CloudSettings` + `SetCloudsParams` (the narrower field-addition tripwire set), never a second cloud command, never a knob smuggled onto `set-environment`/`set-atmosphere`/`set-fog`.

## 0 — Foundation: the reduced-res cloud targets, the device-shared `Clouds` resource, and the settings

Build the resource surfaces before the shaders — they are the shaders' inputs.

- **Per-view `ViewTarget` cloud targets.** Add a reduced-res (quarter-res: half × half of the display extent) cloud accumulation to `ViewTarget`, allocated in `build_screen_space` and its set wired in `allocate_screen_space_sets` alongside the fog/SSGI sets: a `cloud_reduced: [Option<Image>; 2]` ping-pong (`OFFSCREEN_COLOR_FORMAT` rgba16f — `rgb` premultiplied in-scatter, `a` transmittance) keyed by the existing `history_index` parity, a `cloud_reduced_depth: Option<Image>` (R16F reduced-res mean front depth), and a full-res `cloud_full: Option<Image>` (R32F mean front depth for Phase-6 depth compositing — the color composites in place into `offscreen`). Reset `history_valid`-style on resize the same way the resolve chain already does. A per-view `CloudParams` dynamic-offset UBO + set mirrors the `fog_set`/`fog_ubo`/`fog_ubo_stride`/`fog_ubo_offset(frame)` pattern exactly.
- **A device-shared `Clouds` resource (`engine/crates/rendering/src/clouds.rs`, new).** Holds the three compute PSOs, a linear-clamp sampler + a linear-repeat sampler (for the tiling noise), and the descriptor bindings the march needs that are viewport-independent: the Phase-4 noise volumes + density sampler inputs (`Image3D`), the weather map, the atmosphere `transmittance` LUT (bound once via a `bind_luts`-style call, the `AerialPerspective::bind_luts` template), and the sky-light diffuse set. This is the froxel/AP split applied to clouds: fixed device-shared reads live here; per-view resolution-dependent targets live on `ViewTarget`.
- **The lighting/march/temporal settings on the Phase-4 block.** Extend `CloudSettings` (Phase 4) with `primary_steps` (view-ray march budget, default ~64), `light_steps` (cone sun-transmittance taps, default 6), `droplet_diameter` (µm, HG-Draine phase, default ~20, clamped 5..50), and `temporal_factor` (EMA fresh-sample weight, default ~0.1); add the matching `Option<>` fields to `SetCloudsParams` and the `CloudRenderSettings` the renderer receives.

| What | File | Symbols |
|---|---|---|
| Per-view reduced-res targets + dynamic-offset UBO (the model) | `engine/crates/rendering/src/view_target.rs` | `ViewTarget` (`motion`, `history`, `history_index`, `history_valid`, `scratch`, `fog_set`, `fog_ubo`, `fog_ubo_stride`), `build_screen_space`, `allocate_screen_space_sets`, `fog_ubo_offset` |
| Device-shared resource shape + LUT bind template | `engine/crates/rendering/src/froxel_fog.rs` | `AerialPerspective` (`new`, `bind_luts`, `volume_import`, `fill_set`, `fill_layout`), `Image3D` |
| Atmosphere LUTs + coupled sun the lighting reads | `engine/crates/rendering/src/ibl.rs` | `Ibl::transmittance_view`, `multi_scatter_view`, `baked_sun`, `atmosphere_live`, `irradiance_cube_view`, `sky_view_lut_view` |
| Phase-4 cloud state to extend | `engine/crates/scene/src/environment.rs` | `CloudSettings` (Phase 4), `FogSettings` (template), `SceneEnvironment` |

## 1 — The cloud raymarch shader

**File `engine/assets/shaders/cloud_raymarch.slang` (new).**

One thread per reduced-res pixel. This frame writes only the 1-of-16 pixels selected by the 4×4 pattern (`jitter_index & 15`); the reconstruction pass (§3) fills the rest. Build the view ray from the un-jittered camera (`scene_view_proj_unjittered`), offset the ray start by a per-pixel blue-noise / Halton value keyed to `jitter_index` so the coarse step spacing reads as resolvable noise, intersect the Phase-4 cloud shell (layer bottom altitude + height), and clamp the far bound to the scene depth so opaque geometry occludes clouds.

March with SDF / adaptive empty-space skipping: sphere-trace a coarse cloud SDF (or gate on a conservative low-detail density from the Phase-4 sampler), take long steps through empty space, back off to short lit steps once inside the isosurface, return to long steps after several empty samples, and early-out once accumulated alpha saturates. At each lit sample, call the Phase-4 density sampler (dimensional profile × coverage, eroded by detail noise + curl), then light it (§2) and integrate with the energy-conserving slice. Write premultiplied `(scatter.rgb, transmittance.a)` into `cloud_reduced[write]` and the transmittance-weighted running mean front depth into `cloud_reduced_depth`.

```hlsl
// cloud_raymarch.slang — one thread per reduced-res pixel; fresh-marches this frame's 4×4 phase
[[vk::binding(0, 0)]] RWTexture2D<float4> cloudReduced;   // rgb premultiplied scatter, a transmittance
[[vk::binding(1, 0)]] RWTexture2D<float>  cloudDepth;     // transmittance-weighted mean front depth

float4 marchCloud(float2 uv, uint2 px) {
    float3 ro, rd; buildViewRay(uv, ro, rd);                 // un-jittered camera
    float  t0, t1; if (!intersectCloudShell(ro, rd, t0, t1)) return SKY;   // Phase-4 layer altitude/height
    t1 = min(t1, sceneDepthDistance(uv));                    // geometry occludes clouds
    float  t = t0 + rayStartJitter(px, cloud.jitterIndex) * stepLen;       // blue-noise/Halton offset

    float3 scatter = 0.0; float transmittance = 1.0;
    float  depthNum = 0.0, depthDen = 0.0;
    int    empties = 0;
    while (t < t1 && transmittance > 0.01) {                  // opacity early-out
        float sdf = cloudSDF(ro + rd * t);                    // sphere-trace / conservative-density gate
        if (sdf > 0.0) { t += max(sdf, coarseStep); empties = 0; continue; }   // skip empty space
        float d = sampleCloudDensity(ro + rd * t);            // Phase-4 sampler (up-rez inside isosurface)
        if (d <= 0.0) { if (++empties > 8) t += coarseStep; else t += fineStep; continue; }
        float  sigma_t = d * cloud.extinction;
        float3 L       = sampleLighting(ro + rd * t, rd, d, sigma_t);         // §2
        float  sliceT  = exp(-sigma_t * fineStep);
        float3 S_int   = (L - L * sliceT) / max(sigma_t, 1e-7);               // Hillaire analytic slice
        scatter       += transmittance * S_int;              // premultiplied, front-to-back
        depthNum      += transmittance * t; depthDen += transmittance;
        transmittance *= sliceT;
        t += fineStep;
    }
    cloudDepth[px] = depthDen > 0.0 ? depthNum / depthDen : sceneDepthDistance(uv);
    return float4(scatter, transmittance);                   // premultiplied scatter + transmittance
}
```

> Decision to record when building: **the march is bounded by the scene depth so geometry occludes clouds, and it emits a transmittance-weighted mean front depth even in Phase 5.** The composite this phase writes is a plain premultiplied blend, but the front depth costs nothing extra to accumulate and is exactly what Phase 6 needs for depth-correct AP/fog folding — emitting it now makes Phase 6 a relocation, not a re-plumb.

## 2 — Per-sample lighting

**File `engine/assets/shaders/cloud_raymarch.slang` (the `sampleLighting` block) + a shared `cloud_lighting.slang` include.**

Each lit sample sums direct sun scattering, multiple-scattering octaves, and ambient:

- **Sun transmittance via a cone-sampled light march.** ~`light_steps` (default 6) Beer-Lambert samples along the sun direction (`baked_sun()`), the offsets spread into a widening cone to approximate in-scattered/multiple-scattered light and soften the shadow, with the last sample kicked far out for distant self-shadow: `T_sun = exp(-Σ σ_t(sample) · ds)`. The sun radiance is the Phase-1 coupled key-light color/intensity, times the atmospheric sun transmittance `T_atmos` read from `Ibl::transmittance_view` at the sample (the same LUT the AP fill binds), so low cloud bases redden at sunset in lockstep with the sky.
- **Energy-conserving multi-octave multiple scattering.** N = 2 octaves: `L_direct = Σₙ σ_s·aⁿ · T_sun^{bⁿ-ish} · phase(θ·cⁿ) · L_sun`, with per-octave scattering `a`, extinction `b`, and phase-eccentricity `c` scalings, `a ≤ b` enforced (Wrenninge/Frostbite). This *replaces* the powder darkening hack for the bright interior.
- **Phase — HG-Draine (Jendersie & d'Eon 2023).** `phase(θ)` is the analytic HG-Draine blend parameterized by `droplet_diameter` (the fit coefficients `g_HG(d)`, `g_D(d)`, α(d), `w_D(d)` from the paper), evaluated inline — one droplet-diameter knob, no table.
- **Ambient from the live sky-light diffuse.** Sample the sky-light diffuse (the Phase-2 SH set if landed, else `Ibl::irradiance_cube_view`) toward the view/up, weighted by a vertical gradient (cloud bottom darker, top brighter) with a `[a,1]` ground-bounce bias so bottoms aren't fully black, times `σ_s`.

```hlsl
// cloud_lighting.slang (shared include)
float3 sampleLighting(float3 p, float3 viewDir, float d, float sigma_t) {
    float  T_sun   = coneLightMarch(p, cloud.sunDir, cloud.lightSteps);       // ~6 Beer-Lambert cone taps
    float3 T_atmos = sampleTransmittance(p, cloud.sunDir);                    // Ibl::transmittance_view
    float3 L_sun   = cloud.sunColor * cloud.sunIntensity * T_atmos;
    float  cosT    = dot(viewDir, cloud.sunDir);

    float3 direct = 0.0;                                                      // N=2 energy-conserving octaves
    float a = 1.0, b = 1.0, c = 1.0;
    [unroll] for (int n = 0; n < 2; ++n) {
        direct += (sigma_t * cloud.albedo * a)
                * pow(T_sun, b)
                * phaseHGDraine(cosT * c, cloud.dropletDiameter)             // Jendersie & d'Eon 2023
                * L_sun;
        a *= 0.5; b *= 0.5; c *= 0.5;                                        // a ≤ b preserved
    }
    float3 ambient = skyDiffuse(viewDir) * sigma_t * cloud.albedo
                   * heightGradient(p);                                       // vertical + ground-bounce bias
    return direct + ambient;
}
```

> Decision to record when building: **the sun transmittance is fetched from the shipped `transmittance` LUT per sample, not a single global directional-light transmittance.** A uniform tint loses the reddened cloud base at sunset; the per-sample LUT fetch is the physically correct twilight look for one texture read, and it is the exact LUT the AP fill already binds — so cloud atmosphere and scene AP read one source (the coherence Phase 6 relies on).

## 3 — Blue-noise / Halton jitter + quarter-res temporal reconstruction

**File `engine/assets/shaders/cloud_reconstruct.slang` (new).**

The reduced buffer is marched 1-of-16 pixels per frame; this pass reconstructs the full reduced frame. For each reduced-res pixel, reproject its world position (from the reduced front depth) into the previous frame with `ViewTarget.prev_view_proj`, sample the previous accumulation `cloud_reduced[write ^ 1]`, and blend: freshly-marched pixels this frame take a high fresh weight (`temporal_factor`), reprojected pixels carry history. Fall back to the fresh march on disocclusion (reprojected UV off-screen, or the motion target's velocity/depth disagreeing), and optionally clamp the reprojected value to the min/max of the fresh 3×3 neighborhood to suppress ghosting on fast turns. The ray-start jitter itself reuses the frame's `jitter_index` (advanced by the same `advance_jitter` the TAA rig steps) so the noise sequence and TAA are phase-locked.

| What | File | Symbols |
|---|---|---|
| Temporal history + parity + validity to reuse | `engine/crates/rendering/src/view_target.rs` | `prev_view_proj`, `prev_view_proj_valid`, `history`, `history_index`, `history_valid`, `jitter`, `jitter_index`, `prev_jitter` |
| Motion target + jitter offset + un-jittered VP | `engine/crates/rendering/src/renderer.rs` | `add_motion_pass`, `MOTION_FORMAT`, `scene_view_proj_unjittered`, `sun_direction`; `SceneRenderer::jitter_offset` |
| TAA reprojection precedent (reconstruction shape) | `engine/crates/rendering/src/renderer.rs`, `engine/assets/shaders/taa.slang` | `add_taa_pass`, `add_reactive_coverage_pass` |

> Decision to record when building: **the reconstruction reprojects the *reduced accumulation*, not the upscaled full-res result, and reads the built `prev_view_proj`/`history_index` parity — no second history ping-pong.** Reprojecting the low-res linear accumulation (before the depth-aware upscale) is what keeps the 16-frame reconstruction stable; a second history buffer would duplicate the motion+TAA state the engine already carries per view.

## 4 — Depth-aware bilateral upscale + mean front depth

**File `engine/assets/shaders/cloud_upscale.slang` (new).**

Upsample the reconstructed reduced buffer to full res with a depth-aware bilateral filter: for each full-res pixel, weight the reduced-res taps by how well their front depth agrees with the full-res scene depth, rejecting taps across a depth discontinuity so clouds don't halo over opaque geometry edges. Emit the transmittance-weighted mean cloud front depth at full res into `cloud_full` (Phase 6 samples it for depth-correct AP/fog compositing). This pass then does the premultiplied composite (§5) in place — one pass, upscale + composite, mirroring how `add_fog_pass` composites its sampled volume into `color`.

## 5 — Premultiplied composite into scene-linear HDR before bloom

**File `engine/assets/shaders/cloud_upscale.slang` (the composite tail) + `engine/crates/rendering/src/renderer.rs`.**

Composite clouds over the scene HDR premultiplied, front-to-back, depth-gated against the scene: `color.rgb = color.rgb · T_cloud + scatter_cloud` (where `scatter_cloud` is already premultiplied by `1 − T_cloud` during the march). The composite runs on the offscreen `color` while it is still unbounded scene-linear `rgba16f`, **after** the existing fog/AP composite and **immediately before** the bloom pyramid, so distant highlights bloom through the already-clouded HDR.

Wire three compute passes into `record_scene_graph`'s post block using `add_compute_pass` (its `groups_z` path stays 1 here — these are 2D dispatches) and the `import_image`/`alloc_external_layout` pattern for the per-view cloud targets:

- **cloud-raymarch** — access: `cloud_reduced[write]` `StorageImageRwCompute`, `cloud_reduced_depth` `StorageImageRwCompute`, scene `depth` `SampledReadCompute`; the device-shared set (noise/density/weather/transmittance LUT/sky diffuse) bound at set 0, the per-view `CloudParams` UBO dynamic-offset. Dispatch over the reduced extent (8×8 per group).
- **cloud-reconstruct** — access: `cloud_reduced[write]` `StorageImageRwCompute`, `cloud_reduced[write^1]` `SampledReadCompute` (history), `motion` `SampledReadCompute`. Dispatch over the reduced extent.
- **cloud-upscale + composite** — access: `color` `StorageImageRwCompute` (in-place blend), `cloud_reduced[write]` + `cloud_reduced_depth` `SampledReadCompute`, scene `depth` `SampledReadCompute`, `cloud_full` `StorageImageRwCompute` (front-depth out). Dispatch over the full display extent.

Insert them after the `ssgi-history` restore and after `add_fog_pass`/`add_aerial_perspective_pass`, immediately before `add_bloom_pass`:

```rust
// renderer.rs record_scene_graph post block — after add_fog_pass, before add_bloom_pass
self.add_cloud_passes(&mut graph, &pipelines, color, depth, frame);   // march → reconstruct → upscale+composite
// ... add_bloom_pass(...) ; add_tonemap_pass(...)
```

| What | File | Symbols |
|---|---|---|
| Post-block ordering + in-place composite template | `engine/crates/rendering/src/renderer.rs` | `record_scene_graph` post block (`add_scene_resolve_pass` → `ssgi-history` → `add_froxel_fog_passes` → `add_aerial_perspective_pass` → `add_fog_pass` → `add_bloom_pass` → `add_tonemap_pass`), `add_fog_pass`, `FogParams` |
| Compute pass + 3D import + external layout | `engine/crates/rendering/src/renderer.rs`, `engine/crates/rendering/src/render_graph.rs` | `add_compute_pass` (`groups_z`), `import_image`, `import_image_3d`, `alloc_external_layout`, `RgUsage::StorageImageRwCompute`/`SampledReadCompute`, `RgPass::compute` |

> Decision to record when building: **the Phase-5 cloud composite runs after fog/AP and before bloom, and Phase 6 relocates it into the `height_fog.slang` ledger fold.** In the interim, clouds land over the already-fogged scene (unreconciled), which never double-darkens because the atmosphere between camera and cloud is applied zero times here (Phase 6 adds it exactly once). This is the clean seam the planset's ground rules require — one composite, moved not duplicated when the ledger fold lands.

## 6 — The `submit_clouds` seam, settings, and control surface

- **`SceneRenderer::submit_clouds(&CloudRenderSettings)`** (`engine/crates/assets/src/render_scene.rs`) — a new trait method mirroring `submit_fog`, forwarded by `RendererScene` to a `Renderer::set_clouds`, and stubbed in the `RecordingRenderer` mock so the render-scene tests compile and exercise the path. `render_scene` resolves `scene.environment.clouds` into `CloudRenderSettings` each frame right beside the existing `submit_sky`/`submit_fog` resolve, driving the three cloud passes when clouds are enabled.
- **Settings on the Phase-4 block** (`engine/crates/scene/src/environment.rs`, `serde.rs`) — the `primary_steps`/`light_steps`/`droplet_diameter`/`temporal_factor` fields added to `CloudSettings` (with `Default`) and round-tripped in the existing `clouds_to_json`/`clouds_from_json` halves (Phase 4).
- **Protocol** — add the matching `Option<>` fields to `SetCloudsParams` (`engine/crates/protocol/src/dto.rs`); this is the *field-addition* tripwire set (no new DTO type): extend the hand-authored `CloudSettingsDto` schema block + its `required` array in `schema.rs`, and regenerate the committed `schemas/control/{openrpc,command-manifest}.generated.json` via `cargo run -p xtask -- gen-protocol`. `DTO_TYPE_NAMES`/`codegen`/`inventory` are unchanged (no new type).
- **Control handler** (`engine/crates/control/src/commands_scene.rs`) — extend the Phase-4 `set-clouds` merge with the four new fields (each `if let Some(v) = params.x { clouds["x"] = json!(v) }`, with range validation like `set-fog` does — `dropletDiameter` clamped to `[5,50]`, steps ≥ 1, `temporalFactor` in `[0,1]`), returning `EnvironmentDto`. The `sa` CLI needs no per-command code: `sa set-clouds --dropletDiameter 20 --primarySteps 96 --temporalFactor 0.1` flows through the generated manifest.
- **Editor** (`editor/src/panels/EnvironmentPanel.tsx`, `editor/src/control/client.ts`) — add the four `<Row>`s to the Phase-4 Clouds section via the existing `patchClouds`/`recordCloudsEdit` (the `patchFog`/`setFog` template); `bun run check` regenerates `@saffron/protocol` so `setClouds`'s TS param type gains the fields (never hand-edit `sa-types.ts`).

| What | File | Symbols |
|---|---|---|
| Submit-seam trait + resolve + mock | `engine/crates/assets/src/render_scene.rs` | `SceneRenderer` (`submit_sky`, `submit_fog`), `RendererScene`, `render_scene` (submit ordering), `RecordingRenderer` |
| Renderer settings struct (template) | `engine/crates/rendering/src/renderer.rs` | `FogRenderSettings`, `Renderer::set_fog`, `submit_fog` |
| Scene state + serde | `engine/crates/scene/src/environment.rs`, `serde.rs` | `CloudSettings` (Phase 4), `SceneEnvironment`, `clouds_to_json`/`clouds_from_json` |
| Protocol DTO + schema | `engine/crates/protocol/src/dto.rs`, `schema.rs` | `SetCloudsParams` (Phase 4), `CloudSettingsDto`, `EnvironmentDto` |
| Control merge + editor | `engine/crates/control/src/commands_scene.rs`, `editor/src/panels/EnvironmentPanel.tsx`, `editor/src/control/client.ts` | `set-clouds` handler (Phase 4), `patchClouds`, `setClouds` |

## Out of scope (Phase 6)

- **Aerial-perspective reconciliation.** Clouds receiving AP by sampling the shared Transmittance/sky-view LUTs at the cloud hit distance — Phase 6. This phase applies the camera→cloud atmosphere zero times (its composite is a plain premultiplied blend); Phase 6 applies it exactly once and folds `T_cloud` onto the `height_fog.slang` ledger (`T_total = T_fog · T_aerial · T_cloud`), passing the mean front depth this phase already emits. No second AP volume, ever.
- **Cloud shadow map + god-rays.** The top-down density-integrated Beer/ESM cloud shadow map (feeding cloud self-shadow onto the scene and the sun light) and its per-froxel sampling in `fog_inject` for crepuscular rays are Phase 6. This phase's self-shadow is the in-march cone light-march only; it casts nothing onto scene meshes or the froxel fog.
- **Wind / weather animation.** The shared global wind field + curl advection scrolling the weather map + noise (animate detail, not structure) and weather cycles keyed to the Phase-3 time-of-day are Phase 6; this phase marches a static density field.
- **Night-cloud moon lighting.** The Phase-1 moon as a second key light for night clouds is folded in Phase 6; this phase lights from the sun only.

## Known interactions (note, don't over-engineer)

- **Fog/AP tint clouds in the interim.** Because the Phase-5 composite lands *after* `add_fog_pass`, clouds are *not* fogged/AP-tinted this phase (they sit on top). That is correct for the interim and is exactly what Phase 6 fixes when it moves the fold onto the ledger — do not try to pre-apply AP to clouds here.
- **Reduced-res halos at geometry edges.** The depth-aware bilateral upscale (§4) is what prevents cloud bleed over opaque edges; if it is skipped or the depth-agreement weight is too loose, thin fast-moving occluders halo. Use a low-LOD max-depth tap, as the digest notes.
- **Jitter phase-lock with TAA.** The ray-start jitter reuses the frame's `jitter_index`; if the cloud jitter advances on a different cadence than `advance_jitter`, the reconstruction and the scene TAA fight. Read the one jitter sequence.
- **Reduced buffers are viewport-dependent, unlike the froxel/AP volumes.** The cloud targets resize with the display extent (they live on `ViewTarget`, allocated in `build_screen_space`); the device-shared `Clouds` reads (noise/density/LUT) are fixed. Reset the cloud history on a resize the same way the resolve chain resets `history_valid`.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a fixture scene with an active atmosphere + a shadow-casting sun headless (`just run-engine-headless`, on the NVIDIA GPU) with a validation-clean log and confirm lit, temporally-stable clouds render over the sky: `sa set-clouds --enabled true --coverage 0.6 --dropletDiameter 20 --primarySteps 96 --temporalFactor 0.1` must change the frame relative to `--enabled false`, and adjusting `--dropletDiameter` or `--lightSteps` must visibly change the silver-lining / self-shadow. Because a wire type changed (the new `SetCloudsParams` fields), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — the `setClouds` TS param type gains the fields; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-clouds` with the lighting fields over the control plane and asserts the `EnvironmentDto` echo carries them plus a validation-clean log. Add the `docs/content/` cloud-lighting concept page (the SDF/adaptive march, energy-conserving multi-octave scatter, the HG-Draine phase, cone sun transmittance, the analytic slice, quarter-res temporal reconstruction, and the premultiplied pre-bloom composite with the Phase-6 ledger seam called out) sibling to Phase 4's cloud-shape page, and update its hub `_index.md` row (`docs/content/explanations/image-based-lighting/_index.md`), using the slim `What | File | Symbols` table: `cloud_raymarch.slang`/`cloud_reconstruct.slang`/`cloud_upscale.slang`, `engine/crates/rendering/src/clouds.rs` (`Clouds`), `engine/crates/rendering/src/renderer.rs` (the three cloud passes + `set_clouds`), `engine/crates/rendering/src/view_target.rs` (the reduced-res cloud targets), `engine/crates/scene/src/environment.rs` (`CloudSettings`), and `SetCloudsParams` — in the same change.

## References

Cloud lighting + energy-conserving multiple scattering:

- Wrenninge, Kulla, Lejdfors — *Art-Directable Multiple Volumetric Scattering* (SIGGRAPH 2013): https://history.siggraph.org/learning/art-directable-multiple-volumetric-scattering-by-wrenninge/
- Sébastien Hillaire (Frostbite) — *Physically Based Sky, Atmosphere and Cloud Rendering* (SIGGRAPH 2016; N=2 octaves, analytic slice `integScatt = (L − L·T)/σ_t`): https://media.contentapi.ea.com/content/dam/eacom/frostbite/files/s2016-pbs-frostbite-sky-clouds-new.pdf
- Schneider & Vos — *The Real-Time Volumetric Cloudscapes of Horizon: Zero Dawn* (SIGGRAPH 2015; 6 cone light samples, 4×4 Bayer + Halton, 16-frame reconstruction): https://advances.realtimerendering.com/s2015/The%20Real-time%20Volumetric%20Cloudscapes%20of%20Horizon%20-%20Zero%20Dawn%20-%20ARTR.pdf

Phase function (Mie):

- Jendersie & d'Eon — *An Approximate Mie Scattering Function for Fog and Cloud Rendering* (NVIDIA, SIGGRAPH 2023; HG-Draine, one droplet-diameter parameter, analytic sampling): https://research.nvidia.com/labs/rtr/approximate-mie/publications/approximate-mie.pdf

Raymarch optimization + temporal reconstruction:

- Toft, Bowles, Zimmermann (Studio Gobo) — *Optimisations for Real-Time Volumetric Cloudscapes* (2016; blue-noise ray-start jitter, ~16× step reduction): https://arxiv.org/pdf/1609.05344
- bitsquid — *Volumetric Clouds* (4×4 Bayer, 1-of-16 pixels/frame, EMA reprojection): http://bitsquid.blogspot.com/2016/07/volumetric-clouds.html
- Vertex Fragment — *Upsampling to Improve Volumetric Cloud Render Performance* (quarter-res + depth-aware upscale + jitter reconstruction): https://www.vertexfragment.com/ramblings/volumetric-cloud-upsampling/
- clayjohn/realtime_clouds — HZD-faithful cloud shader (dual/triple HG, dual-Beer, step counts): https://github.com/clayjohn/realtime_clouds

Empty-space skipping:

- Andrew Schneider — *Nubis, Cubed: voxel-based clouds* (SIGGRAPH 2023; SDF sphere-trace + adaptive + jittered empty-space skip): https://advances.realtimerendering.com/s2023/Nubis%20Cubed%20(Advances%202023).pdf
- Unreal Engine — *Volumetric Cloud Material* (Conservative Density empty-space gate, multiscatter octaves): https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-material-in-unreal-engine
