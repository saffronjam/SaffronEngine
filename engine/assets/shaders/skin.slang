// Compute skinning pre-pass. Deforms each skinned vertex once into a base-layout Vertex buffer, so
// every graphics pass (scene, depth, shadow, AO, motion, RT BLAS) reads it as an ordinary static mesh
// — no skinned vertex-shader permutation. One thread per vertex.
//
// The vertex + skin streams are read/written as raw byte buffers at the engine's tightly-packed
// strides: sa::Vertex is 48 B (position@0, normal@12, uv0@24, tangent@32) and sa::VertexSkin is 24 B
// (joints@0, weights@8). ByteAddressBuffer.Load/Store use scalar packing, so these tight strides match
// the engine layout exactly (a typed StructuredBuffer<Vertex> would impose std430's 16-byte float3
// alignment and misplace the fields). The tangent (xyz + w handedness) is rotated by the skin matrix
// and written through, so the deformed mesh keeps a valid UV-aligned frame.
//
// The deformed position is the skin-matrix applied to the bind pose WITHOUT the instance model matrix:
// the graphics passes still apply `inst.model` / `inst.normalMatrix`, exactly as for a static mesh.

[[vk::binding(0, 0)]] ByteAddressBuffer inVertices;             // static bind-pose stream (32 B stride)
[[vk::binding(1, 0)]] ByteAddressBuffer inSkins;                // parallel skin influences (24 B stride)
[[vk::binding(2, 0)]] StructuredBuffer<float4x4> jointMatrices;  // the frame's joint palette (64 B, matches glm)
[[vk::binding(3, 0)]] RWByteAddressBuffer outVertices;          // deformed output, this frame (32 B stride)

static const uint VertexStride = 48u;      // sizeof(sa::Vertex)
static const uint SkinStride = 24u;        // sizeof(sa::VertexSkin)
static const uint NormalOffset = 12u;      // sa::Vertex.normal
static const uint UvOffset = 24u;          // sa::Vertex.uv0
static const uint TangentOffset = 32u;     // sa::Vertex.tangent (float4: xyz + w handedness)
static const uint WeightsOffset = 8u;      // sa::VertexSkin.weights (after u16x4 joints)

struct Push
{
    uint vertexCount;     // vertices in this skinned mesh-instance
    uint jointOffset;     // base of this instance's joints in the palette
    uint deformedOffset;  // base of this instance's vertices in the deformed buffer
    uint pad;
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    const uint i = tid.x;
    if (i >= push.vertexCount)
    {
        return;
    }

    const uint vBase = i * VertexStride;
    const float3 position = inVertices.Load<float3>(vBase);
    const float3 normal = inVertices.Load<float3>(vBase + NormalOffset);
    const float2 uv0 = inVertices.Load<float2>(vBase + UvOffset);
    const float4 tangent = inVertices.Load<float4>(vBase + TangentOffset);

    const uint sBase = i * SkinStride;
    const uint2 jointsPacked = inSkins.Load<uint2>(sBase);
    const float4 weights = inSkins.Load<float4>(sBase + WeightsOffset);
    const uint4 joints = uint4(jointsPacked.x & 0xFFFFu, jointsPacked.x >> 16,
                               jointsPacked.y & 0xFFFFu, jointsPacked.y >> 16);

    const uint base = push.jointOffset;
    const float4x4 skin = weights.x * jointMatrices[base + joints.x]
                        + weights.y * jointMatrices[base + joints.y]
                        + weights.z * jointMatrices[base + joints.z]
                        + weights.w * jointMatrices[base + joints.w];

    const uint oBase = (push.deformedOffset + i) * VertexStride;
    const float3 skinnedTangent = normalize(mul((float3x3)skin, tangent.xyz));
    outVertices.Store<float3>(oBase, mul(skin, float4(position, 1.0)).xyz);
    outVertices.Store<float3>(oBase + NormalOffset, normalize(mul((float3x3)skin, normal)));
    outVertices.Store<float2>(oBase + UvOffset, uv0);
    // Rotate the tangent by the skin matrix; the ±1 handedness (w) is invariant under the rotation.
    outVertices.Store<float4>(oBase + TangentOffset, float4(skinnedTangent, tangent.w));
}
