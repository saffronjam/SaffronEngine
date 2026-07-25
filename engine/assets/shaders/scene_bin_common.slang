// Shared executor-bucket vocabulary: the CPU enumerates live (shaderIndex, psoBin)
// combos into dense draw buckets each frame and uploads a sorted lookup plus per-bucket
// command-slice bases and ends. Kernels binary-search the lookup for a record's key; a
// miss raises the pressure flag rather than truncating silently.

public static const uint SCENE_EXECUTOR_BUCKET_CAPACITY = 512u;

public struct SceneBucketTable
{
    public uint liveCount;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
    public uint2 lookup[SCENE_EXECUTOR_BUCKET_CAPACITY];
    public uint bases[SCENE_EXECUTOR_BUCKET_CAPACITY];
    public uint ends[SCENE_EXECUTOR_BUCKET_CAPACITY];
};

public uint sceneBucketKey(uint shaderIndex, uint psoBin)
{
    return (shaderIndex << 16u) | (psoBin & 0xFFFFu);
}

// Binary search over the sorted lookup; 0xFFFFFFFF on a miss. The live count and the
// resolved bucket both clamp to the capacity so a corrupt table can never drive an
// out-of-bounds cursor or command write.
public uint sceneBucketFor(StructuredBuffer<SceneBucketTable> table, uint key)
{
    uint low = 0u;
    uint high = min(table[0].liveCount, SCENE_EXECUTOR_BUCKET_CAPACITY);
    while (low < high)
    {
        uint mid = (low + high) / 2u;
        uint2 entry = table[0].lookup[mid];
        if (entry.x == key)
        {
            return entry.y < SCENE_EXECUTOR_BUCKET_CAPACITY ? entry.y : 0xFFFFFFFFu;
        }
        if (entry.x < key)
        {
            low = mid + 1u;
        }
        else
        {
            high = mid;
        }
    }
    return 0xFFFFFFFFu;
}
