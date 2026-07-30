import lighting;
import global_gpu_data;
import scene_bin_common;
import scene_micro_common;

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
    s.sheetNormal = normalize(m.sheetNormal);
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

// The frame's semantic record stream (the active view's traversal output); the executor
// vertex path below indexes it by firstInstance.
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;

// The binner's indexed command stream. The indexed path consumes these as draw arguments;
// the mesh executor below reads the identical words as data, which is what makes the two
// executors share one cluster cut rather than each selecting its own.
[[vk::binding(5, 2)]] StructuredBuffer<DrawIndexedIndirectCommand> executorCommands;

float2 executorOctToVector(uint packed)
{
    float2 oct = float2(
        float(int(packed << 16u) >> 16u) / 32767.0,
        float(int(packed) >> 16u) / 32767.0);
    return oct;
}

float3 executorDecodeNormal(uint packed)
{
    float2 oct = executorOctToVector(packed);
    float3 n = float3(oct.x, oct.y, 1.0 - abs(oct.x) - abs(oct.y));
    if (n.z < 0.0)
    {
        float2 folded = (1.0 - abs(float2(n.y, n.x))) * sign(float2(n.x, n.y));
        n.x = folded.x;
        n.y = folded.y;
    }
    return normalize(n);
}

// The whole per-vertex body, shared by the indexed and mesh executor entries below. Agreement
// between the two executors is vertex-for-vertex or it is nothing, so there is one copy of this
// arithmetic and both entries call it. Geometry pulls through buffer device addresses and the
// emitted interface is `VertexOutput`, so every fragment entry shades identically.
//
// Takes the record *index* rather than the record: passing `GpuDrawRecord` by value across a
// function boundary makes Slang emit a natural-layout copy of the struct, which then collides
// with the std430 layout the buffer-device-address pointers below require, and the module fails
// spirv-val. Loading inside keeps one layout.
VertexOutput executorVertexOutput(uint recordIndex, uint vertexIndex)
{
    let addresses = gpuSceneAddresses;
    let record = executorRecords[recordIndex];
    let instance = gpuSceneLoadInstance(addresses, record.instance.index);
    let geometry = gpuSceneResidentGeometry(addresses, record.geometry);

    // An assembly record (a plant use) rebases its vertex fetch to the placed
    // prototype's slice and premultiplies the use's family-local transform.
    bool assembly =
        geometry.parts.count != 0u && record.clusterState != GPU_ASSEMBLY_NO_USE;
    GpuAssemblyUseRecord use = {};
    uint vertexBase = 0u;
    if (assembly)
    {
        use = gpuSceneAssemblyUse(addresses, geometry, geometry.reserved, record.clusterState);
        vertexBase =
            gpuSceneAssemblyPrototype(addresses, geometry, use.prototype).vertexBase;
    }

    float3 local;
    float3 localNormal;
    float2 uv;
    if (record.representation == GPU_REPRESENTATION_MICRO_BLADE)
    {
        let blade = gpuSceneMicroBladeVertex(
            gpuSceneMicroCandidate(addresses, record.contentIndex), vertexIndex, false);
        local = blade.position;
        localNormal = blade.normal;
        uv = blade.uv;
    }
    else if (record.representation == GPU_REPRESENTATION_AGGREGATE_VOXEL)
    {
        GpuHandle pageHandle = GpuHandle(record.contentIndex, 0u);
        let page = gpuSceneResidentPage(addresses, pageHandle);
        let node = gpuScenePageNode(addresses, page);
        let vertex = gpuScenePageVoxelVertex(addresses, page, node, vertexIndex);
        local = vertex.position;
        localNormal = executorDecodeNormal(vertex.normalOct);
        uv = float2(0.0);
    }
    else if (record.representation == GPU_REPRESENTATION_DISPLACED_MICRO)
    {
        // The amplification arena already carries the displaced local position and the
        // normal the emit kernel derived from the displaced surface, so the vertex is
        // final: `vertexIndex` is the absolute micro-vertex slot (the draw's vertexOffset
        // added the row's slice base).
        gpuSceneDisplacedVertex(addresses, vertexIndex, false, local, localNormal, uv);
    }
    else if (record.deformation.index != 0xFFFFFFFFu
        && (instance.deformation.index == record.deformation.index))
    {
        // A deformed instance pulls the skinning compute's stable output; UVs stay in
        // the static stream (deformation never rewrites them).
        let scene = gpuSceneLoadDeformation(addresses, record.deformation.index);
        let provider = gpuSceneDeformationProvider(addresses, scene.target.index);
        uint firstVertex = gpuSceneDeformationParameter(addresses, provider.firstParameter);
        uint64_t base = addresses.deformedVertices
            + uint64_t(firstVertex + vertexBase + vertexIndex) * geometry.vertexStride;
        local = *(float3*)(base);
        localNormal = *(float3*)(base + 12u);
        uint64_t staticBase = addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride;
        uv = *(float2*)(staticBase + 24u);
    }
    else
    {
        uint64_t base = addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride;
        local = *(float3*)(base);
        localNormal = *(float3*)(base + 12u);
        uv = *(float2*)(base + 24u);
    }
    if (assembly)
    {
        local = gpuSceneAssemblyTransformPoint(use, local);
        localNormal = normalize(gpuSceneAssemblyTransformVector(use, localNormal));
    }

    // The record's material resolves to the resident parameter block the fragment's
    // set-2 params buffer indexes.
    let materialReference = gpuSceneLoadMaterialReference(addresses, record.material.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);
    float3 windOffset = gpuSceneWindDeform(
        addresses,
        instance,
        record.instance.index,
        local,
        false,
        assembly,
        use,
        record.clusterState);
    return transformExecutorVertex(
        local,
        localNormal,
        uv,
        instance,
        windOffset,
        material.parameterIndex,
        record.transition);
}

// Indexed-MDI executor vertex path: SV_VulkanVertexID is the pulled index value plus the
// command's `vertexOffset` (the D3D-flavoured SV_VertexID subtracts the offset back out, which
// loses a displaced row's arena slice base), SV_VulkanInstanceID is the record index the bin
// scatter stored in firstInstance.
[shader("vertex")]
VertexOutput vertexMainExecutor(
    uint vertexIndex : SV_VulkanVertexID, uint recordIndex : SV_VulkanInstanceID)
{
    return executorVertexOutput(recordIndex, vertexIndex);
}

// Mesh-shader executor: the same records, the same cut, reached through a mesh stage. One
// workgroup covers MESH_TRIANGLES_PER_GROUP triangles of one draw — `SV_DrawIndex` names the
// draw within the bucket's slice, the group id names the block within it, and the scatter sized
// the group count from that draw's own index count.
//
// Vertices go out un-deduplicated (three per triangle), which is what caps a group at 62: 186
// output vertices against the 256 limit, not the primitive limit.
[shader("mesh")]
[outputtopology("triangle")]
[numthreads(MESH_TRIANGLES_PER_GROUP, 1, 1)]
void meshMainExecutor(
    uint threadIndex: SV_GroupIndex,
    uint3 groupId: SV_GroupID,
    uint drawIndex: SV_DrawIndex,
    out vertices VertexOutput verts[MESH_TRIANGLES_PER_GROUP * 3],
    out indices uint3 tris[MESH_TRIANGLES_PER_GROUP])
{
    let command = executorCommands[executorSliceBase() + drawIndex];

    let totalTriangles = command.indexCount / 3u;
    let firstTriangle = groupId.x * MESH_TRIANGLES_PER_GROUP;
    uint triangles = 0u;
    if (firstTriangle < totalTriangles)
    {
        triangles = min(MESH_TRIANGLES_PER_GROUP, totalTriangles - firstTriangle);
    }
    SetMeshOutputCounts(triangles * 3u, triangles);
    if (threadIndex >= triangles)
    {
        return;
    }

    // `firstIndex` offsets the PAGES arena, not the global index arena — the indexed path binds
    // `gpu_data.pages` as its index buffer and the scatter derives `firstIndex` from a page byte
    // offset. Reading `addresses.indices` here rasterizes nothing. A displaced row's stream is
    // the amplification arena instead, the buffer the indexed path binds for that bucket.
    let displaced =
        executorRecords[command.firstInstance].representation == GPU_REPRESENTATION_DISPLACED_MICRO;
    uint* indexBlob = (uint*)(
        displaced ? gpuSceneAddresses.displacedIndices : gpuSceneAddresses.pageBytes);
    let triangle = firstTriangle + threadIndex;
    let base = threadIndex * 3u;
    for (uint corner = 0u; corner < 3u; ++corner)
    {
        // The fixed-function path adds `vertexOffset` to every fetched index; the mesh stage
        // must do it itself, and it is zero for every stream except the arena.
        let vertexIndex = indexBlob[command.firstIndex + triangle * 3u + corner]
            + uint(command.vertexOffset);
        verts[base + corner] = executorVertexOutput(command.firstInstance, vertexIndex);
    }
    tris[threadIndex] = uint3(base, base + 1u, base + 2u);
}

// Depth-prepass fragment: depth-only (the pass binds no color attachment), doing solely the
// alpha-clip test so masked materials (foliage/curtains) do NOT write depth at their cutout
// texels — otherwise the scene pass, which loads this depth and discards at the same texels,
// would leave holes bleeding the background. The float4 return carries the coverage alpha in the
// alpha component for alpha-to-coverage under MSAA; the rgb lanes are never consumed. Reads only
// material params (set 2) + albedo (set 0), never `globals` (set 1), so the prepass PSO binds no
// light set. The test mirrors `fragmentMain` exactly so the depth matches at edges.
[shader("fragment")]
float4 depthPrepassFragment(VertexOutput input) : SV_Target
{
    if (!gpuTransitionCovered(input.transition, uint2(input.position.xy)))
    {
        discard;
    }
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
        return float4(0.0, 0.0, 0.0, coverage.alpha);
    }
    return float4(0.0, 0.0, 0.0, 1.0);
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

// Transition-reactive vertex path: re-walks the opaque record stream and keeps only
// content whose reprojected history is unreliable — micro blades (regenerated per
// frame), records mid representation-transition, and instances the wind prepass flagged
// as reading a scroll-reset interaction displacement. Every other record collapses to a
// degenerate off-screen point, so it rasterizes nothing.
[shader("vertex")]
VertexOutput vertexMainReactiveTransition(
    uint vertexIndex : SV_VulkanVertexID, uint recordIndex : SV_VulkanInstanceID)
{
    let record = executorRecords[recordIndex];
    if (record.representation != GPU_REPRESENTATION_MICRO_BLADE && record.transition == 0u
        && !gpuSceneWindInteractionReset(gpuSceneAddresses, record.instance.index))
    {
        VertexOutput collapsed = {};
        collapsed.position = float4(2.0e9, 2.0e9, 2.0e9, 1.0);
        return collapsed;
    }
    return vertexMainExecutor(vertexIndex, recordIndex);
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input, bool isFront : SV_IsFrontFace) : SV_Target
{
    // A record mid representation-crossfade owns only its stochastic share of the
    // pixels; the complementary side draws the rest (mirrors depthPrepassFragment).
    if (!gpuTransitionCovered(input.transition, uint2(input.position.xy)))
    {
        discard;
    }
    // Double-sided materials rasterize both faces (cull NONE); glTF requires the back-face normal be
    // inverted before lighting, else a back-facing fragment reads N·L < 0 (no direct light) and samples
    // IBL along the wrong hemisphere. Single-sided materials cull back faces, so `isFront` is always
    // true for them and this is a no-op. Flip the interpolated normal in this by-value copy so the whole
    // downstream chain (tangent frame, normal-map perturb, the specular horizon term) sees it.
    MaterialParams coverageMaterial = materialParams[input.materialIndex];
    bool thinSheet = (coverageMaterial.tex1.w & FEATURE_THIN_SHEET) != 0u;
    uint normalMode = coverageMaterial.coverage.w;
    float3 sheetNormal = input.worldNormal;
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
    mi.sheetNormal = sheetNormal;
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
