// Shared executor-bucket vocabulary: the CPU enumerates live (shaderIndex, psoBin)
// combos into dense draw buckets each frame and uploads a sorted lookup plus per-bucket
// command-slice bases and ends. Kernels binary-search the lookup for a record's key; a
// miss raises the pressure flag rather than truncating silently.

public static const uint SCENE_EXECUTOR_BUCKET_CAPACITY = 512u;

// Triangles one mesh-executor workgroup emits. Three vertices per triangle go out without
// deduplication — a cluster carries no local vertex table, only a flat index range — so the
// bound is the 256-vertex output limit, not the primitive one: 62 x 3 = 186. Two groups cover
// a full 124-triangle cluster.
public static const uint MESH_TRIANGLES_PER_GROUP = 62u;

// One binned draw. The scatter writes these as `VkDrawIndexedIndirectCommand`; the indexed
// executor consumes them as draw arguments and the mesh executor reads the same words as data.
public struct DrawIndexedIndirectCommand
{
    public uint indexCount;
    public uint instanceCount;
    public uint firstIndex;
    public int vertexOffset;
    public uint firstInstance;
};

// The mesh executor's per-draw dispatch arguments (`VkDrawMeshTasksIndirectCommandEXT`),
// written by the scatter beside each indexed command so both executors read one count word.
public struct DrawMeshTasksIndirectCommand
{
    public uint groupCountX;
    public uint groupCountY;
    public uint groupCountZ;
};

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
