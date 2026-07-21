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
    bool thinSheet = (features & FEATURE_THIN_SHEET) != 0u;
    s.surfaceModel = thinSheet ? SURFACE_THIN_SHEET : SURFACE_STANDARD;
    s.frontFace = 1.0;
    s.frontResponse = thinSheet ? mat.thinReflection.x : 1.0;
    s.backResponse = thinSheet ? mat.thinReflection.y : 1.0;
    s.thickness = thinSheet ? mat.thinReflection.w : 0.0;
    s.absorption = thinSheet ? mat.thinAbsorption.rgb : float3(0.0);
    s.transmission = thinSheet ? mat.thinTransmission.rgb : float3(0.0);
    s.energyLimit = thinSheet ? mat.thinAbsorption.w : 1.0;
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
    float authoredRoughness = thinSheet ? mat.thinReflection.z : mat.pbr.y;
    s.roughness = clamp(authoredRoughness * mr.g, 0.045, 1.0);

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
    else if ((features & (FEATURE_DISPLACE | FEATURE_HEIGHT_BUMP)) != 0u)
    {
        // Height → shading normal: derive the fine normal from the height gradient in the tangent
        // frame (a height-to-normal bump). FEATURE_HEIGHT_BUMP stops here (flat silhouette, no
        // geometry); FEATURE_DISPLACE additionally had its vertices moved by the `displace` compute
        // pre-pass, so this bump matches the already-displaced microsurface.
        const float eps = 1.0 / 512.0;
        uint hi = mat.tex1.x;
        float h0 = albedoTextures[NonUniformResourceIndex(hi)].SampleLevel(uv, 0.0).r;
        float hu = albedoTextures[NonUniformResourceIndex(hi)].SampleLevel(uv + float2(eps, 0.0), 0.0).r;
        float hv = albedoTextures[NonUniformResourceIndex(hi)].SampleLevel(uv + float2(0.0, eps), 0.0).r;
        float2 grad = float2(hu - h0, hv - h0) / eps * mat.emissive.w;  // heightScale = emissive.w
        float3 bumpN = normalize(float3(-grad, 1.0));
        n = normalize(mul(bumpN, tbn));
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
float depthPrepassFragment(VertexOutput input) : SV_Target
{
    MaterialParams mat = materialParams[input.materialIndex];
    bool thinSheet = (mat.tex1.w & FEATURE_THIN_SHEET) != 0u;
    if (thinSheet || (mat.tex1.w & FEATURE_ALPHACLIP) != 0u)
    {
        float2 uv = input.uv0 * mat.uv.xy + mat.uv.zw;
        uint sourceIndex = thinSheet ? mat.coverage.x : mat.tex0.x;
        CoverageSample coverage = sampleCanonicalCoverage(
            albedoTextures[NonUniformResourceIndex(sourceIndex)],
            uv,
            input.coverageAnchor,
            thinSheet ? mat.coverage.y : COVERAGE_SOURCE_ALBEDO_ALPHA,
            thinSheet ? mat.coverage.z : COVERAGE_CLASS_MASKED,
            mat.baseColor.a,
            thinSheet ? mat.coverageHash.zw : uint2(1u),
            thinSheet ? mat.coverageHash.xy : uint2(0u),
            mat.tex1.z,
            mat.pbr.w,
            thinSheet,
            kAlphaToCoverage);
        if (!coverage.covered) { discard; }
        return coverage.alpha;
    }
    return 1.0;
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
    MaterialParams coverageMaterial = materialParams[input.materialIndex];
    bool thinSheet = (coverageMaterial.tex1.w & FEATURE_THIN_SHEET) != 0u;
    uint normalMode = coverageMaterial.coverage.w;
    float3 viewDirection = makeMaterialInput(input).viewDir;
    if ((!isFront && (!thinSheet || normalMode == THIN_NORMAL_FACE_FORWARD_BACK))
        || (thinSheet
            && normalMode == THIN_NORMAL_SYMMETRIC
            && dot(input.worldNormal, viewDirection) < 0.0))
    {
        input.worldNormal = -input.worldNormal;
    }

    bool standardMasked = (coverageMaterial.tex1.w & FEATURE_ALPHACLIP) != 0u;
    CoverageSample canonicalCoverage;
    canonicalCoverage.alpha = 1.0;
    canonicalCoverage.covered = true;
    if (thinSheet || standardMasked)
    {
        float2 coverageUv = input.uv0 * coverageMaterial.uv.xy + coverageMaterial.uv.zw;
        uint sourceIndex = thinSheet ? coverageMaterial.coverage.x : coverageMaterial.tex0.x;
        uint sourceKind = thinSheet
            ? coverageMaterial.coverage.y
            : COVERAGE_SOURCE_ALBEDO_ALPHA;
        uint classification = thinSheet
            ? coverageMaterial.coverage.z
            : COVERAGE_CLASS_MASKED;
        canonicalCoverage = sampleCanonicalCoverage(
            albedoTextures[NonUniformResourceIndex(sourceIndex)],
            coverageUv,
            input.coverageAnchor,
            sourceKind,
            classification,
            coverageMaterial.baseColor.a,
            thinSheet ? coverageMaterial.coverageHash.zw : uint2(1u),
            thinSheet ? coverageMaterial.coverageHash.xy : uint2(0u),
            coverageMaterial.tex1.z,
            coverageMaterial.pbr.w,
            thinSheet,
            kAlphaToCoverage && classification == COVERAGE_CLASS_MASKED);
        if (!canonicalCoverage.covered) { discard; }
    }
    MaterialInput mi = makeMaterialInput(input);
    SurfaceData surf = evalSurface(mi);
    surf.frontFace = isFront ? 1.0 : 0.0;
    if (thinSheet)
    {
        surf.metallic = 0.0;
        surf.opacity = canonicalCoverage.alpha;
    }
    if ((thinSheet || standardMasked) && kAlphaToCoverage)
    {
        float4 lit = evalViewMode(input, surf, kUnlit, kTranslucent);
        lit.a = canonicalCoverage.alpha;
        return lit;
    }
    // Resolve the active view mode (full shading, a debug channel, or the unlit permutation). Debug
    // channels write into the HDR scene color, so tonemap still applies — values are display-referred.
    return evalViewMode(input, surf, kUnlit, kTranslucent);
}
