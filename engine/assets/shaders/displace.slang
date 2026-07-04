// Compute displacement pre-pass. Displaces each vertex of a displacement-enabled mesh-instance once
// along its normal by the material's height field into a base-layout Vertex buffer, so every graphics
// pass (scene, depth, shadow, AO, motion, RT BLAS) reads it as an ordinary static mesh — no displaced
// vertex-shader permutation, and the buffer can back a BLAS (phase C1). One thread per vertex.
//
// Same raw-byte discipline as skin.slang: sa::Vertex is 32 B (position@0, normal@12, uv0@24), read and
// written through ByteAddressBuffer so the scalar packing matches the engine layout exactly (a typed
// StructuredBuffer<Vertex> would impose std430's 16-byte float3 alignment — a 48 B stride — and shatter
// the mesh). The displaced position is in the mesh's LOCAL space (height applied along the local
// normal), WITHOUT the instance model matrix: the graphics passes still apply inst.model /
// inst.normalMatrix, exactly as for a static mesh.

// The bindless albedo/height array on the shared set 0 (matches the übershader), sampled by index.
[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];

// The displacement buffers on set 1: the static base stream in, the displaced stream out.
[[vk::binding(0, 1)]] ByteAddressBuffer inVertices;      // static base stream (32 B stride)
[[vk::binding(1, 1)]] RWByteAddressBuffer outVertices;   // displaced output, this frame (32 B stride)

static const uint VertexStride = 32u;   // sizeof(sa::Vertex)
static const uint NormalOffset = 12u;   // sa::Vertex.normal
static const uint UvOffset = 24u;       // sa::Vertex.uv0

struct Push
{
    uint vertexCount;     // vertices in this mesh-instance
    uint deformedOffset;  // base of this instance's vertices in the displaced buffer
    uint heightIndex;     // bindless index of the height map
    float heightScale;    // local-space displacement amplitude (== MaterialParams.emissive.w)
    float4 uvTransform;   // tiling.xy, offset.xy (== MaterialParams.uv)
};
[[vk::push_constant]] Push push;

float sampleHeight(float2 uv)
{
    return albedoTextures[NonUniformResourceIndex(push.heightIndex)].SampleLevel(uv, 0.0).r;
}

// Branchless orthonormal basis from a unit normal (Duff et al. 2017) — the mesh vertex stream carries
// no tangents, and any consistent frame suffices to turn the height gradient into a coarse geometric
// normal (the fragment recomputes the fine shading normal from screen-space UV derivatives).
void branchlessBasis(float3 n, out float3 t, out float3 b)
{
    const float s = n.z >= 0.0 ? 1.0 : -1.0;
    const float a = -1.0 / (s + n.z);
    const float d = n.x * n.y * a;
    t = float3(1.0 + s * n.x * n.x * a, s * d, -s * n.x);
    b = float3(d, s + n.y * n.y * a, -n.y);
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

    const float2 uv = uv0 * push.uvTransform.xy + push.uvTransform.zw;
    const float h = sampleHeight(uv);
    const float3 displaced = position + normal * (h * push.heightScale);

    // Coarse geometric normal from the height gradient in the tangent frame.
    const float eps = 1.0 / 512.0;
    const float hu = sampleHeight(uv + float2(eps, 0.0));
    const float hv = sampleHeight(uv + float2(0.0, eps));
    float3 t, b;
    branchlessBasis(normal, t, b);
    const float2 grad = float2(hu - h, hv - h) / eps * push.heightScale;
    const float3 bumpN = normalize(normal - t * grad.x - b * grad.y);

    const uint oBase = (push.deformedOffset + i) * VertexStride;
    outVertices.Store<float3>(oBase, displaced);
    outVertices.Store<float3>(oBase + NormalOffset, bumpN);
    outVertices.Store<float2>(oBase + UvOffset, uv0);
}
