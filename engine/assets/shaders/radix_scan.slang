// LSD radix sort, per-pass step 2: an exclusive scan over the column-major
// (bucket, workgroup) histogram table. Bucket order dominates, so each workgroup's base
// preserves stability.
//
// One workgroup of 256 lanes walks the table in bucket order, scanning each bucket's
// per-workgroup counts in shared memory and carrying the running total across chunks and across
// buckets. The scan must be parallel: a single lane stepping the table issues one dependent
// global round-trip per entry, tens of thousands of them per radix pass and four passes a frame,
// which outlives the driver's command-buffer watchdog instead of merely being slow.

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> histograms;

struct Push
{
    // Total table entries: `buckets * perBucket`.
    uint entries;
    // Histogram columns per bucket — the histogram pass's workgroup count.
    uint perBucket;
    uint reserved0;
    uint reserved1;
};
[[vk::push_constant]] Push pc;

groupshared uint gScan[256];
groupshared uint gCarry;

[shader("compute")]
[numthreads(256, 1, 1)]
void computeMain(uint3 lid : SV_GroupThreadID)
{
    if (lid.x == 0u)
    {
        gCarry = 0u;
    }
    GroupMemoryBarrierWithGroupSync();

    uint perBucket = max(pc.perBucket, 1u);
    uint buckets = pc.entries / perBucket;
    for (uint bucket = 0u; bucket < buckets; ++bucket)
    {
        for (uint base = 0u; base < perBucket; base += 256u)
        {
            uint column = base + lid.x;
            bool live = column < perBucket;
            uint index = bucket * perBucket + column;
            uint value = live ? histograms[index] : 0u;
            gScan[lid.x] = value;
            GroupMemoryBarrierWithGroupSync();
            // Hillis-Steele inclusive scan across the 256 lanes: read, sync, add, sync.
            for (uint offset = 1u; offset < 256u; offset = offset << 1)
            {
                uint addend = (lid.x >= offset) ? gScan[lid.x - offset] : 0u;
                GroupMemoryBarrierWithGroupSync();
                gScan[lid.x] += addend;
                GroupMemoryBarrierWithGroupSync();
            }
            if (live)
            {
                // Exclusive prefix, offset by everything scanned before this chunk.
                histograms[index] = gCarry + gScan[lid.x] - value;
            }
            GroupMemoryBarrierWithGroupSync();
            if (lid.x == 255u)
            {
                // Lane 255 holds the chunk's inclusive total; padding lanes contributed zero.
                gCarry += gScan[255];
            }
            GroupMemoryBarrierWithGroupSync();
        }
    }
}
