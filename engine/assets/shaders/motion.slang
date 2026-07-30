// Per-pixel screen-space motion vectors for TAA. Renders the instanced scene depth-tested;
// the fragment outputs (prevUV - curUV), i.e. the offset from this pixel's current UV to
// where the same surface point was last frame. RG16F. Set 2 = instances; push = current +
// previous camera viewProj.
//
// Two sources of motion are reprojected together:
//   - object motion: the scene instance record's current and previous world columns.
//   - deformation motion: the current and previous streams of whatever deforms the vertex —
//     skin/morph output, a regenerated blade, or the displacement arena's geomorph slide. A
//     static vertex reads one position for both, so only object motion contributes.
// current clip = curViewProj  * curColumns  * position
// previous clip = prevViewProj * prevColumns * prevPosition

import coverage;
import global_gpu_data;
import material_params;
import scene_micro_common;

[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

[[vk::binding(2, 2)]] StructuredBuffer<MaterialParams> materialParams;

struct Push
{
    float4x4 curViewProj;
    float4x4 prevViewProj;
};
[[vk::push_constant]] Push pc;

struct VertexOutput
{
    float4 position : SV_Position;
    float4 curClip : TEXCOORD0;
    float4 prevClip : TEXCOORD1;
    float3 worldPos : POSITION0;
    float2 uv0 : TEXCOORD2;
    nointerpolation uint materialIndex : TEXCOORD3;
    float3 coverageAnchor : TEXCOORD4;
    // The record's representation-crossfade word (zero outside a transition).
    nointerpolation uint transition : TEXCOORD5;
};

// The frame's semantic record stream (layout parity with the mesh module's binding).
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;

// Indexed-MDI executor vertex path: current position from the static or deformed
// stream, previous position from the prev-deformed stream (static falls back to the
// same static position), current/previous world transforms from the scene instance
// record's two column sets.
[shader("vertex")]
VertexOutput vertexMainExecutor(
    uint vertexIndex : SV_VulkanVertexID, uint recordIndex : SV_VulkanInstanceID)
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
    float3 prevLocal;
    float2 uv;
    if (record.representation == GPU_REPRESENTATION_MICRO_BLADE)
    {
        let candidate = gpuSceneMicroCandidate(addresses, record.contentIndex);
        let blade = gpuSceneMicroBladeVertex(candidate, vertexIndex, false);
        local = blade.position;
        // The previous frame's blade rebuilds with the stored previous wind bend,
        // so blade motion vectors carry the exact wind term.
        prevLocal = gpuSceneMicroBladeVertex(candidate, vertexIndex, true).position;
        uv = blade.uv;
    }
    else if (record.representation == GPU_REPRESENTATION_AGGREGATE_VOXEL)
    {
        GpuHandle pageHandle = GpuHandle(record.contentIndex, 0u);
        let page = gpuSceneResidentPage(addresses, pageHandle);
        let node = gpuScenePageNode(addresses, page);
        let vertex = gpuScenePageVoxelVertex(addresses, page, node, vertexIndex);
        local = vertex.position;
        prevLocal = local;
        uv = float2(0.0);
    }
    else if (record.representation == GPU_REPRESENTATION_DISPLACED_MICRO)
    {
        // The amplification arena carries a parallel previous-frame stream at the same
        // slot: same local grid, last frame's per-edge factors, so the cur→prev delta is
        // exactly the geomorph slide the micro-vertex made.
        float3 microNormal;
        float2 prevUv;
        float3 prevNormal;
        gpuSceneDisplacedVertex(addresses, vertexIndex, false, local, microNormal, uv);
        gpuSceneDisplacedVertex(addresses, vertexIndex, true, prevLocal, prevNormal, prevUv);
    }
    else if (record.deformation.index != 0xFFFFFFFFu
        && instance.deformation.index == record.deformation.index)
    {
        let scene = gpuSceneLoadDeformation(addresses, record.deformation.index);
        let provider = gpuSceneDeformationProvider(addresses, scene.target.index);
        uint firstVertex = gpuSceneDeformationParameter(addresses, provider.firstParameter);
        uint prevFirst = gpuSceneDeformationParameter(addresses, provider.firstParameter + 1u);
        local = *(float3*)(addresses.deformedVertices
            + uint64_t(firstVertex + vertexBase + vertexIndex) * geometry.vertexStride);
        prevLocal = *(float3*)(addresses.prevDeformedVertices
            + uint64_t(prevFirst + vertexBase + vertexIndex) * geometry.vertexStride);
        uint64_t staticBase = addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride;
        uv = *(float2*)(staticBase + 24u);
    }
    else
    {
        uint64_t base = addresses.vertices + geometry.vertices.first
            + uint64_t(vertexBase + vertexIndex) * geometry.vertexStride;
        local = *(float3*)(base);
        prevLocal = local;
        uv = *(float2*)(base + 24u);
    }
    if (assembly)
    {
        local = gpuSceneAssemblyTransformPoint(use, local);
        prevLocal = gpuSceneAssemblyTransformPoint(use, prevLocal);
    }

    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    float3 curWorld = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz
        + gpuSceneWindDeform(
            addresses, instance, record.instance.index, local, false, assembly, use,
            record.clusterState);
    // A dynamic instance's previous columns carry its object motion; a static
    // instance decodes to its one exact placement (zero object motion). The stored
    // previous wind sway restores the wind term of that motion.
    float4 p0, p1, p2, p3;
    gpuSceneInstanceColumns(instance, true, p0, p1, p2, p3);
    float3 prevWorld =
        p0.xyz * prevLocal.x + p1.xyz * prevLocal.y + p2.xyz * prevLocal.z + p3.xyz
        + gpuSceneWindDeform(
            addresses, instance, record.instance.index, prevLocal, true, assembly, use,
            record.clusterState);

    let materialReference = gpuSceneLoadMaterialReference(addresses, record.material.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);

    VertexOutput output;
    output.position = mul(pc.curViewProj, float4(curWorld, 1.0));
    output.curClip = output.position;
    output.prevClip = mul(pc.prevViewProj, float4(prevWorld, 1.0));
    output.worldPos = curWorld;
    output.uv0 = uv;
    output.materialIndex = material.parameterIndex;
    output.coverageAnchor = local;
    output.transition = record.transition;
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    // Mirrors the depth and shading passes' stochastic crossfade share exactly.
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
        CoverageSample coverageSample = sampleCanonicalCoverage(
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
            false);
        if (!coverageSample.covered) { discard; }
    }
    float2 curNdc = input.curClip.xy / input.curClip.w;
    float2 prevNdc = input.prevClip.xy / input.prevClip.w;
    // NDC -> UV is *0.5 (+0.5); the offset is the NDC delta scaled by 0.5. Both clips use
    // the same Y-flipped projection, so the Y sign is consistent with the sampled images.
    float2 motionUv = (prevNdc - curNdc) * 0.5;
    return float4(motionUv, 0.0, 0.0);
}
