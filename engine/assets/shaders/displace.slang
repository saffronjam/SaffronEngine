// Compute displacement pre-pass. Displaces each vertex of a displacement-enabled mesh-instance once
// along its normal by the material's height field into a base-layout Vertex buffer, so every graphics
// pass (scene, depth, shadow, AO, motion, RT BLAS) reads it as an ordinary static mesh — no displaced
// vertex-shader permutation, and the buffer can back a BLAS (phase C1). One thread per vertex.
//
// Same raw-byte discipline as skin.slang: sa::Vertex is 48 B (position@0, normal@12, uv0@24,
// tangent@32), read and written through ByteAddressBuffer so the scalar packing matches the engine
// layout exactly (a typed StructuredBuffer<Vertex> would misplace the fields under std430). The
// displaced position is in the mesh's LOCAL space, WITHOUT the instance model matrix: the graphics
// passes still apply inst.model / inst.normalMatrix, exactly as for a static mesh.
//
// The stored UV-aligned tangent gives a true TBN: scalar displacement offsets along the normal (and
// derives a coarse bump from the height gradient in that frame), while **vector displacement** — when
// the material binds a vector map (`vectorIndex != 0`) — offsets by the sampled tangent-space XYZ
// transformed through the TBN, so overhangs/undercuts a scalar height cannot express become real
// geometry (visible to every pass + the BLAS).

// The bindless albedo/height array on the shared set 0 (matches the übershader), sampled by index.
[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

// The displacement buffers on set 1: the static base stream in, the displaced stream out.
[[vk::binding(0, 1)]] ByteAddressBuffer inVertices;      // static base stream (48 B stride)
[[vk::binding(1, 1)]] RWByteAddressBuffer outVertices;   // displaced output, this frame (48 B stride)

static const uint VertexStride = 48u;   // sizeof(sa::Vertex)
static const uint NormalOffset = 12u;   // sa::Vertex.normal
static const uint UvOffset = 24u;       // sa::Vertex.uv0
static const uint TangentOffset = 32u;  // sa::Vertex.tangent (float4: xyz + w handedness)

struct Push
{
    uint vertexCount;     // vertices in this mesh-instance
    uint deformedOffset;  // base of this instance's vertices in the displaced buffer
    uint heightIndex;     // bindless index of the height map
    float heightScale;    // local-space displacement amplitude (== MaterialParams.emissive.w)
    float4 uvTransform;   // tiling.xy, offset.xy (== MaterialParams.uv)
    uint vectorIndex;     // bindless index of the vector-displacement map (0 = scalar-only)
    // Three scalar pads (not a `uint3`, which std430 would 16-align — opening a gap that pushes the
    // block to 60 B and past the 48-B `DisplacePush` push range). Scalars keep the block at 48 B.
    uint pad0;
    uint pad1;
    uint pad2;
};
[[vk::push_constant]] Push push;

float sampleHeight(float2 uv)
{
    return albedoTextures[NonUniformResourceIndex(push.heightIndex)].SampleLevel(uv, 0.0).r;
}

// Tangent-space XYZ from the vector-displacement map ([0,1] → [-1,1]).
float3 sampleVector(float2 uv)
{
    return albedoTextures[NonUniformResourceIndex(push.vectorIndex)].SampleLevel(uv, 0.0).xyz * 2.0 - 1.0;
}

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
    const float3 normal = normalize(inVertices.Load<float3>(vBase + NormalOffset));
    const float2 uv0 = inVertices.Load<float2>(vBase + UvOffset);
    const float4 tangent = inVertices.Load<float4>(vBase + TangentOffset);

    // The UV-aligned tangent frame: t along +U, b = handedness · (n × t) along +V.
    const float3 t = normalize(tangent.xyz - normal * dot(normal, tangent.xyz));
    const float3 b = cross(normal, t) * tangent.w;

    const float2 uv = uv0 * push.uvTransform.xy + push.uvTransform.zw;

    float3 displaced;
    float3 geoNormal;
    if (push.vectorIndex != 0u)
    {
        // Vector displacement: offset by the tangent-space XYZ through the TBN — overhangs/undercuts
        // a scalar height cannot express. The fragment recovers the fine normal from derivatives.
        const float3 v = sampleVector(uv) * push.heightScale;
        displaced = position + t * v.x + b * v.y + normal * v.z;
        geoNormal = normal;
    }
    else
    {
        // Scalar displacement along the normal, plus a coarse geometric normal from the height
        // gradient in the true tangent frame.
        const float h = sampleHeight(uv);
        displaced = position + normal * (h * push.heightScale);
        const float eps = 1.0 / 512.0;
        const float hu = sampleHeight(uv + float2(eps, 0.0));
        const float hv = sampleHeight(uv + float2(0.0, eps));
        const float2 grad = float2(hu - h, hv - h) / eps * push.heightScale;
        geoNormal = normalize(normal - t * grad.x - b * grad.y);
    }

    const uint oBase = (push.deformedOffset + i) * VertexStride;
    outVertices.Store<float3>(oBase, displaced);
    outVertices.Store<float3>(oBase + NormalOffset, geoNormal);
    outVertices.Store<float2>(oBase + UvOffset, uv0);
    // Carry the tangent through so the deformed mesh keeps a valid UV-aligned frame.
    outVertices.Store<float4>(oBase + TangentOffset, tangent);
}
