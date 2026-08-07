// Virtual-shadow receiver demand, pass 2: compacts the demand bitmap into the
// frame's request list (a count word followed by raw page-table indices the CPU
// drains and decodes), clearing the bitmap for the next frame.
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> demandBitmap;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> requests;

struct VsmCompactPush
{
    uint capacity;
    uint reserved0;
    uint reserved1;
    uint reserved2;
};

[[vk::push_constant]] VsmCompactPush pc;

static const uint VSM_BITMAP_WORDS = (8u * 32u * 32u + 16u * 16u + 6u * 8u * 8u + 31u) / 32u;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    uint word = tid.x;
    if (word >= VSM_BITMAP_WORDS)
    {
        return;
    }
    uint bits = demandBitmap[word];
    demandBitmap[word] = 0u;
    while (bits != 0u)
    {
        uint bit = firstbitlow(bits);
        bits &= bits - 1u;
        uint index = word * 32u + bit;
        uint slot;
        InterlockedAdd(requests[0], 1u, slot);
        if (slot < pc.capacity)
        {
            requests[1u + slot] = index;
        }
    }
}
