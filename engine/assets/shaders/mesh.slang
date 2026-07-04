import lighting;

// The unlit permutation stays a consumer spec constant so the PSO can specialize it (constant_id 0);
// keeping it out of the module avoids relying on spec-constant export across the link.
[[vk::constant_id(0)]] const bool kUnlit = false;

// Alpha-to-coverage (a masked material under MSAA): the fragment sharpens the alpha-test cutoff into
// per-sample coverage (written to `SV_Target.a`, which the A2C-enabled PSO reads) instead of the hard
// per-pixel discard — anti-aliased foliage/cutout edges without blending or sorting. Off elsewhere.
[[vk::constant_id(1)]] const bool kAlphaToCoverage = false;

// The translucent (alpha-blended) permutation, baked into the blend PSO. Translucent surfaces are
// drawn forward, after opaque, with depth-write off — so the opaque G-buffer's screen-space terms
// (SSAO, screen-space GI, contact shadows, SSR, SSGI) at a translucent fragment's screen UV belong
// to the OPAQUE geometry behind it, not the fragment. This constant gates those off and substitutes
// world-space indirect (analytic IBL irradiance + DDGI) — see `evalLighting`. Off (opaque) elsewhere.
[[vk::constant_id(2)]] const bool kTranslucent = false;

SurfaceData evalSurface(MaterialInput m)
{
    MaterialParams mat = m.mat;
    uint features = mat.tex1.w;
    float2 uv = m.uv0 * mat.uv.xy + mat.uv.zw;
    float3 n = normalize(m.worldNormal);
    float3x3 tbn = cotangentFrame(n, m.worldPos, uv);

    // Parallax occlusion mapping offsets the UV (in the pre-parallax tangent frame) before every
    // other sample, so the whole surface reads from the displaced texel.
    if ((features & FEATURE_HEIGHT) != 0u)
    {
        float3 vt = mul(tbn, m.viewDir);
        uv = parallaxUv(uv, vt, mat.tex1.x, mat.emissive.w);  // heightScale = emissive.w
    }

    SurfaceData s;
    // Node-graph codegen replaces the block between the markers (see compileMaterialMeshShader); the
    // default below is the fixed glTF metallic-roughness path. `mat`/`uv`/`m`/`albedoTextures` are in scope.
    // @graph-begin
    float4 base = albedoTextures[NonUniformResourceIndex(mat.tex0.x)].Sample(uv);
    s.albedo = base.rgb * mat.baseColor.rgb;
    s.opacity = base.a * mat.baseColor.a;

    // metallic-roughness / ORM (roughness in G, metalness in B); modulates the factors. The
    // default white texture (when a material has none) leaves the factors unchanged.
    float4 mr = albedoTextures[NonUniformResourceIndex(mat.tex0.y)].Sample(uv);
    s.metallic = saturate(mat.pbr.x * mr.b);
    s.roughness = clamp(mat.pbr.y * mr.g, 0.045, 1.0);

    // Ambient occlusion (AO in R), only when a map is present.
    s.occlusion = 1.0;
    if ((features & FEATURE_OCCLUSION) != 0u)
    {
        s.occlusion = albedoTextures[NonUniformResourceIndex(mat.tex1.y)].Sample(uv).r;
    }

    // Tangent-space normal map: perturb the geometric normal with the pre-parallax frame.
    if ((features & FEATURE_NORMAL) != 0u)
    {
        float3 mapN = albedoTextures[NonUniformResourceIndex(mat.tex0.z)].Sample(uv).xyz * 2.0 - 1.0;
        mapN.xy *= mat.pbr.z;  // normalStrength
        n = normalize(mul(mapN, tbn));
    }
    s.normal = n;

    // Emissive: factor radiance, modulated by the emissive map when present.
    s.emissive = mat.emissive.rgb;
    if ((features & FEATURE_EMISSIVE_TEX) != 0u)
    {
        s.emissive *= albedoTextures[NonUniformResourceIndex(mat.tex0.w)].Sample(uv).rgb;
    }
    // @graph-end
    return s;
}

[shader("vertex")]
VertexOutput vertexMain(VertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    return transformVertex(input, instanceIndex);
}

[shader("vertex")]
VertexOutput vertexMainSkinned(SkinnedVertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    return transformVertexSkinned(input, instanceIndex);
}

// Depth-prepass fragment: depth-only (no color output), doing solely the alpha-clip test so masked
// materials (foliage/curtains) do NOT write depth at their cutout texels — otherwise the scene pass,
// which loads this depth and discards at the same texels, would leave holes bleeding the background.
// Reads only material params (set 2) + albedo (set 0), never `globals` (set 1), so the prepass PSO
// binds no light set. The test mirrors `fragmentMain` exactly so the depth matches at edges.
[shader("fragment")]
void depthPrepassFragment(VertexOutput input)
{
    MaterialParams mat = materialParams[input.materialIndex];
    if ((mat.tex1.w & FEATURE_ALPHACLIP) != 0u)
    {
        float2 uv = input.uv0 * mat.uv.xy + mat.uv.zw;
        float alpha = albedoTextures[NonUniformResourceIndex(mat.tex0.x)].Sample(uv).a * mat.baseColor.a;
        if (alpha < mat.pbr.w) { discard; }
    }
}

// Reactive-coverage fragment: the TAA reactive mask's producer. The reactive-coverage pass draws
// only the translucent batches, so every fragment reaching here is alpha-blended content whose
// reprojected history is unreliable — mark it fully reactive (1.0) so the resolve biases the pixel
// toward the current frame. Outputs a single channel into the r8 reactive target and reads no
// material/texture (set-2-only, matching the pass's instance-only bind).
[shader("fragment")]
float reactiveCoverageFragment(VertexOutput input) : SV_Target
{
    return 1.0;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input, bool isFront : SV_IsFrontFace) : SV_Target
{
    // Double-sided materials rasterize both faces (cull NONE); glTF requires the back-face normal be
    // inverted before lighting, else a back-facing fragment reads N·L < 0 (no direct light) and samples
    // IBL along the wrong hemisphere. Single-sided materials cull back faces, so `isFront` is always
    // true for them and this is a no-op. Flip the interpolated normal in this by-value copy so the whole
    // downstream chain (tangent frame, normal-map perturb, the specular horizon term) sees it.
    if (!isFront) { input.worldNormal = -input.worldNormal; }
    MaterialInput mi = makeMaterialInput(input);
    SurfaceData surf = evalSurface(mi);
    bool masked = (mi.mat.tex1.w & FEATURE_ALPHACLIP) != 0u;
    float cutoff = mi.mat.pbr.w;
    // Alpha-to-coverage (masked + MSAA): rescale alpha around the cutoff by its screen-space
    // derivative so the transition spans ~one pixel, giving a crisp anti-aliased edge instead of a
    // hard 1-bit cut. The A2C-enabled PSO reads the returned `.a` as per-sample coverage; a fully
    // outside fragment (coverage <= 0) is still discarded.
    if (masked && kAlphaToCoverage)
    {
        float coverage = saturate((surf.opacity - cutoff) / max(fwidth(surf.opacity), 1e-4) + 0.5);
        if (coverage <= 0.0) { discard; }
        float4 lit = evalViewMode(input, surf, kUnlit, kTranslucent);
        lit.a = coverage;
        return lit;
    }
    // The hard alpha-test cutout (masked without MSAA, or A2C off): a per-pixel discard.
    if (masked && (surf.opacity < cutoff))
    {
        discard;
    }
    // Resolve the active view mode (full shading, a debug channel, or the unlit permutation). Debug
    // channels write into the HDR scene color, so tonemap still applies — values are display-referred.
    return evalViewMode(input, surf, kUnlit, kTranslucent);
}
