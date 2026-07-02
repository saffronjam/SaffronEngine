import lighting;

// The unlit permutation stays a consumer spec constant so the PSO can specialize it (constant_id 0);
// keeping it out of the module avoids relying on spec-constant export across the link.
[[vk::constant_id(0)]] const bool kUnlit = false;

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

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    MaterialInput mi = makeMaterialInput(input);
    SurfaceData surf = evalSurface(mi);
    if (((mi.mat.tex1.w & FEATURE_ALPHACLIP) != 0u) && (surf.opacity < mi.mat.pbr.w))
    {
        discard;
    }
    // Resolve the active view mode (full shading, a debug channel, or the unlit permutation). Debug
    // channels write into the HDR scene color, so tonemap still applies — values are display-referred.
    return evalViewMode(input, surf, kUnlit);
}
