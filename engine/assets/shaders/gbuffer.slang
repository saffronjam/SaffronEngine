// Thin G-buffer prepass for screen-space AO: renders view-space normal (rgb) + view-space
// Z (a, negative) into an rgba16f target and per-instance roughness into a second R8 target,
// and lays down depth. Instanced like the mesh / depth-prepass (set 2 = per-instance data);
// the push constant carries viewProj (clip) + view (to take positions/normals into view space).
// The roughness target feeds the specular-occlusion prepass (`specocc.slang`), whose reflection
// cone widens with roughness.

import coverage;
import global_gpu_data;
import material_params;

[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

struct Instance
{
    float4x4 model;
    float4x4 normalMatrix;
    float4x4 prevModel;
    float4 baseColor;
    uint4 texture;
    float4 pbr;
    float4 emissive;
};
[[vk::binding(0, 2)]] StructuredBuffer<Instance> instances;
[[vk::binding(2, 2)]] StructuredBuffer<MaterialParams> materialParams;

struct Push
{
    float4x4 viewProj;  // world -> clip
    float4x4 view;       // world -> view
};
[[vk::push_constant]] Push pc;

struct VertexInput
{
    [[vk::location(0)]] float3 position;
    [[vk::location(1)]] float3 normal;
    [[vk::location(2)]] float2 uv0;
};

struct VertexOutput
{
    float4 position : SV_Position;
    float3 viewNormal : NORMAL;
    float viewZ : TEXCOORD0;
    float roughness : TEXCOORD1;
    float3 worldPos : POSITION0;
    float2 uv0 : TEXCOORD2;
    nointerpolation uint materialIndex : TEXCOORD3;
    float3 coverageAnchor : TEXCOORD4;
    float3 viewDirection : TEXCOORD5;
    // The record's representation-crossfade word (zero outside a transition).
    nointerpolation uint transition : TEXCOORD6;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input, uint instanceIndex : SV_VulkanInstanceID)
{
    Instance inst = instances[instanceIndex];
    VertexOutput output;
    float4 worldPos = mul(inst.model, float4(input.position, 1.0));
    float4 viewPos = mul(pc.view, worldPos);
    output.position = mul(pc.viewProj, worldPos);
    float3 worldNormal = mul((float3x3)inst.normalMatrix, input.normal);
    output.viewNormal = mul((float3x3)pc.view, worldNormal);
    output.viewZ = viewPos.z;
    MaterialParams mat = materialParams[inst.texture.w];
    output.roughness = (mat.tex1.w & FEATURE_THIN_SHEET) != 0u
        ? mat.thinReflection.z
        : inst.pbr.y;
    output.worldPos = worldPos.xyz;
    output.uv0 = input.uv0;
    output.materialIndex = inst.texture.w;
    output.coverageAnchor = input.position;
    output.viewDirection = -viewPos.xyz;
    output.transition = 0u;
    return output;
}

// The frame's semantic record stream (layout parity with the mesh module's binding).
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;

// Indexed-MDI executor vertex path: the record index arrives in firstInstance, geometry
// pulls through buffer device addresses (deformed when the record carries a matching
// deformation), and the material resolves to its resident parameter block — roughness
// comes from the params (interned per override variant), never a per-instance word.
[shader("vertex")]
VertexOutput vertexMainExecutor(uint vertexIndex : SV_VertexID, uint recordIndex : SV_VulkanInstanceID)
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
        float2 oct = float2(
            float(int(vertex.normalOct << 16u) >> 16u) / 32767.0,
            float(int(vertex.normalOct) >> 16u) / 32767.0);
        float3 n = float3(oct.x, oct.y, 1.0 - abs(oct.x) - abs(oct.y));
        if (n.z < 0.0)
        {
            float2 folded = (1.0 - abs(float2(n.y, n.x))) * sign(float2(n.x, n.y));
            n.x = folded.x;
            n.y = folded.y;
        }
        localNormal = normalize(n);
        uv = float2(0.0);
    }
    else if (record.deformation.index != 0xFFFFFFFFu
        && instance.deformation.index == record.deformation.index)
    {
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

    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz
        + gpuSceneWindDeform(
            addresses, instance, record.instance.index, local, false, assembly, use,
            record.clusterState);
    float3 n0 = cross(c1.xyz, c2.xyz);
    float3 n1 = cross(c2.xyz, c0.xyz);
    float3 n2 = cross(c0.xyz, c1.xyz);
    float3 worldNormal = n0 * localNormal.x + n1 * localNormal.y + n2 * localNormal.z;

    let materialReference = gpuSceneLoadMaterialReference(addresses, record.material.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);
    MaterialParams mat = materialParams[material.parameterIndex];

    VertexOutput output;
    float4 viewPos = mul(pc.view, float4(world, 1.0));
    output.position = mul(pc.viewProj, float4(world, 1.0));
    output.viewNormal = mul((float3x3)pc.view, worldNormal);
    output.viewZ = viewPos.z;
    output.roughness = (mat.tex1.w & FEATURE_THIN_SHEET) != 0u
        ? mat.thinReflection.z
        : mat.pbr.y;
    output.worldPos = world;
    output.uv0 = uv;
    output.materialIndex = material.parameterIndex;
    output.coverageAnchor = local;
    output.viewDirection = -viewPos.xyz;
    output.transition = record.transition;
    return output;
}

// Two MRT targets: view normal + view-Z (rgba16f) and per-instance roughness (R8).
struct GbufferOut
{
    float4 normalZ : SV_Target0;
    float roughness : SV_Target1;
};

[shader("fragment")]
GbufferOut fragmentMain(VertexOutput input, bool isFront : SV_IsFrontFace)
{
    // Mirrors the depth and forward passes' stochastic crossfade share exactly.
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
    GbufferOut o;
    // Double-sided geometry rasterizes both faces; invert the back-face normal (glTF) so the
    // screen-space AO/GI/SSR that read this G-buffer see the actually-visible face. Single-sided
    // geometry culls back faces, so `isFront` is always true and this is a no-op.
    float3 viewNormal = normalize(input.viewNormal);
    uint normalMode = mat.coverage.w;
    if ((!isFront && (!thinSheet || normalMode == THIN_NORMAL_FACE_FORWARD_BACK))
        || (thinSheet
            && normalMode == THIN_NORMAL_SYMMETRIC
            && dot(viewNormal, input.viewDirection) < 0.0))
    {
        viewNormal = -viewNormal;
    }
    o.normalZ = float4(viewNormal, input.viewZ);
    o.roughness = input.roughness;
    return o;
}
