// Compute morph (blend-shape) pre-pass. Deforms each morph-target vertex into the shared deformed
// Vertex buffer BEFORE the skin pass, so skinning reads the morphed base and every graphics pass reads
// an ordinary static mesh. Three passes selected by `push.pass`:
//
//   0 clear   — one thread per vertex: zero the fixed-point accumulator.
//   1 scatter — one thread per active (target, delta): atomicAdd the quantized weighted delta into the
//               accumulator. Integer atomics commute, so the sum is bit-deterministic regardless of
//               thread order (required for the golden-buffer gate and the llvmpipe CI GPU). No float
//               atomics anywhere.
//   2 resolve — one thread per vertex: dequantize, add the base vertex, renormalize the normal, copy
//               uv0 + tangent, and Store the 48 B Vertex into the deformed buffer at the instance's offset.
//
// Like skin.slang, the streams are raw byte buffers at the engine's tight strides (sa::Vertex 48 B,
// sa::MorphDelta 28 B) read via ByteAddressBuffer scalar packing — a typed StructuredBuffer would impose
// std430 padding that does not match the engine layout. Morph deltas carry no tangent, so the base
// tangent is copied through unchanged (the fragment recovers fine detail from screen-space derivatives).

struct ActiveTarget
{
    uint targetIndex;  // index into targetRanges
    uint scatterBase;  // cumulative delta count of preceding active targets (flattens the scatter)
    float weight;      // the resolved morph weight
    float pad;
};

[[vk::binding(0, 0)]] ByteAddressBuffer inVertices;               // base bind-pose stream (48 B stride)
[[vk::binding(1, 0)]] ByteAddressBuffer inDeltas;                 // flat MorphDelta array (28 B stride)
[[vk::binding(2, 0)]] StructuredBuffer<uint2> targetRanges;       // per-target {first_delta, delta_count}
[[vk::binding(3, 0)]] StructuredBuffer<ActiveTarget> activeTargets; // per-frame compacted active list
[[vk::binding(4, 0)]] RWByteAddressBuffer accum;                 // per-vertex fixed-point scratch (6 i32)
[[vk::binding(5, 0)]] RWByteAddressBuffer outVertices;           // deformed output (48 B stride)

static const uint VertexStride = 48u;     // sizeof(sa::Vertex)
static const uint NormalOffset = 12u;     // sa::Vertex.normal
static const uint UvOffset = 24u;         // sa::Vertex.uv0
static const uint TangentOffset = 32u;    // sa::Vertex.tangent (float4: xyz + w handedness)
static const uint DeltaStride = 28u;      // sizeof(sa::MorphDelta)
static const uint DeltaPosOffset = 4u;    // MorphDelta.d_position (after u32 vertex_index)
static const uint DeltaNrmOffset = 16u;   // MorphDelta.d_normal
static const uint AccumStride = 24u;      // 6 * i32 per vertex

// Fixed-point scale: positions/normals quantize to i32 at ~1/65536 precision; the accumulated
// Σ weight·delta stays far inside i32 range for any plausible rig. Mirrored in the Rust push side.
static const float MorphFixedScale = 65536.0;

struct Push
{
    uint vertexCount;     // vertices in this morph mesh-instance
    uint scatterCount;    // total active deltas (the scatter dispatch size)
    uint activeCount;     // number of active targets (the scatter scan bound)
    uint activeBase;      // base of this instance's active targets in the shared buffer
    uint deformedOffset;  // base of this instance's vertices in the deformed buffer
    uint pass;            // 0 clear, 1 scatter, 2 resolve
};
[[vk::push_constant]] Push push;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    const uint i = tid.x;

    if (push.pass == 0u)
    {
        // Clear: zero the 6 i32 accumulator lanes for this vertex.
        if (i >= push.vertexCount)
        {
            return;
        }
        const uint a = i * AccumStride;
        accum.Store<int>(a, 0);
        accum.Store<int>(a + 4u, 0);
        accum.Store<int>(a + 8u, 0);
        accum.Store<int>(a + 12u, 0);
        accum.Store<int>(a + 16u, 0);
        accum.Store<int>(a + 20u, 0);
        return;
    }

    if (push.pass == 1u)
    {
        // Scatter: thread i handles the i-th active delta across all active targets.
        if (i >= push.scatterCount)
        {
            return;
        }
        // Find the active target whose flattened [scatterBase, scatterBase + delta_count) span
        // contains i (few active targets, so a linear scan is cheap and order-independent).
        uint a = 0u;
        for (; a + 1u < push.activeCount; ++a)
        {
            const ActiveTarget cand = activeTargets[push.activeBase + a];
            const uint count = targetRanges[cand.targetIndex].y;
            if (i < cand.scatterBase + count)
            {
                break;
            }
        }
        const ActiveTarget at = activeTargets[push.activeBase + a];
        const uint local = i - at.scatterBase;
        const uint deltaIndex = targetRanges[at.targetIndex].x + local;
        const uint dBase = deltaIndex * DeltaStride;
        const uint vertexIndex = inDeltas.Load<uint>(dBase);
        const float3 dPos = inDeltas.Load<float3>(dBase + DeltaPosOffset);
        const float3 dNrm = inDeltas.Load<float3>(dBase + DeltaNrmOffset);

        const float w = at.weight;
        const uint accBase = vertexIndex * AccumStride;
        accum.InterlockedAdd(accBase, (int)round(w * dPos.x * MorphFixedScale));
        accum.InterlockedAdd(accBase + 4u, (int)round(w * dPos.y * MorphFixedScale));
        accum.InterlockedAdd(accBase + 8u, (int)round(w * dPos.z * MorphFixedScale));
        accum.InterlockedAdd(accBase + 12u, (int)round(w * dNrm.x * MorphFixedScale));
        accum.InterlockedAdd(accBase + 16u, (int)round(w * dNrm.y * MorphFixedScale));
        accum.InterlockedAdd(accBase + 20u, (int)round(w * dNrm.z * MorphFixedScale));
        return;
    }

    // Resolve: thread i finalizes vertex i into the deformed buffer.
    if (i >= push.vertexCount)
    {
        return;
    }
    const uint vBase = i * VertexStride;
    const float3 basePos = inVertices.Load<float3>(vBase);
    const float3 baseNrm = inVertices.Load<float3>(vBase + NormalOffset);
    const float2 uv0 = inVertices.Load<float2>(vBase + UvOffset);
    const float4 tangent = inVertices.Load<float4>(vBase + TangentOffset);

    const uint accBase = i * AccumStride;
    const float3 dPos = float3(accum.Load<int>(accBase), accum.Load<int>(accBase + 4u),
                               accum.Load<int>(accBase + 8u))
                        / MorphFixedScale;
    const float3 dNrm = float3(accum.Load<int>(accBase + 12u), accum.Load<int>(accBase + 16u),
                               accum.Load<int>(accBase + 20u))
                        / MorphFixedScale;

    const float3 morphedPos = basePos + dPos;
    float3 morphedNrm = baseNrm + dNrm;
    const float len = length(morphedNrm);
    morphedNrm = len > 1e-6 ? morphedNrm / len : baseNrm;

    const uint oBase = (push.deformedOffset + i) * VertexStride;
    outVertices.Store<float3>(oBase, morphedPos);
    outVertices.Store<float3>(oBase + NormalOffset, morphedNrm);
    outVertices.Store<float2>(oBase + UvOffset, uv0);
    outVertices.Store<float4>(oBase + TangentOffset, tangent);
}
