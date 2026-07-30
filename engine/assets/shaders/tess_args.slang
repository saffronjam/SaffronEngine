// Global emit-dispatch args, one thread: the scan's global micro-vertex total into the
// `VkDispatchIndirectCommand` that sizes the emit dispatch (one thread per micro-vertex, 64 per
// group), so the GPU-computed count drives the dispatch with no CPU readback.

[[vk::binding(0, 0)]] StructuredBuffer<uint> globalTotals;    // [microVertTotal, microTriTotal]
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> dispatchArgs;  // VkDispatchIndirectCommand { x, y, z }

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (tid.x != 0u)
    {
        return;
    }
    const uint microVerts = globalTotals[0];
    dispatchArgs[0] = (microVerts + 63u) / 64u;
    dispatchArgs[1] = 1u;
    dispatchArgs[2] = 1u;
}
