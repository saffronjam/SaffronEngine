// The `VK_EXT_mesh_shader` raster front end (phase C2): a task (amplification) shader that culls +
// launches one mesh workgroup per meshlet, and a mesh shader that emits that meshlet's displaced
// vertices + triangles into the *same* `VertexOutput` the übershader fragment (`mesh::fragmentMain`)
// consumes — so the meshlet path swaps only the geometry front end, reusing all downstream shading.
//
// One draw covers one mesh-instance's submesh: the push carries the instance row index + the submesh's
// meshlet range, and `cmd_draw_mesh_tasks` dispatches `meshletCount` task groups. The task shader
// frustum-culls each meshlet by its bounding sphere and compacts the survivors into the mesh dispatch.
//
// Geometry lives on set 8 as raw storage buffers (the meshlet decomposition + the mesh's base vertex
// stream, read at the engine's tight 48 B stride) rather than vertex-input attributes — a mesh shader
// fetches vertices itself. The transform mirrors `lighting::transformVertex` but reads its `viewProj`
// from this path's own push (the vertex path's `camera` push is a separate pipeline's).

import lighting;

// Set 8: the meshlet decomposition + the base vertex stream this draw rasterizes.
[[vk::binding(0, 8)]] StructuredBuffer<Meshlet> meshlets;         // per-meshlet descriptors
[[vk::binding(1, 8)]] StructuredBuffer<uint> meshletVertices;     // flat global vertex indices
[[vk::binding(2, 8)]] ByteAddressBuffer meshletTriangles;         // packed local indices (u8, 3 per tri)
[[vk::binding(3, 8)]] ByteAddressBuffer vertexStream;             // base Vertex stream (48 B stride)

// Matches `saffron_geometry::Meshlet` (32 B): offsets + counts + a bounding sphere for cull.
struct Meshlet
{
    uint vertexOffset;
    uint triangleOffset;
    uint vertexCount;
    uint triangleCount;
    float3 center;
    float radius;
};

struct MeshPush
{
    float4x4 viewProj;    // world -> clip (this path's own copy; the vertex path uses `camera`)
    uint instanceIndex;   // row into the shared `instances` buffer (set 2)
    uint meshletBase;     // first meshlet of this submesh in `meshlets`
    uint meshletCount;    // meshlets in this submesh (task dispatch size)
    uint vertexBase;      // added to each global vertex index — the deformed slice for a skinned/
                          // displaced batch (`vertexStream` is then the deformed buffer), 0 for static
};
[[vk::push_constant]] MeshPush push;

static const uint VertexStride = 48u;   // sizeof(sa::Vertex)
static const uint NormalOffset = 12u;
static const uint UvOffset = 24u;

// The compacted per-group meshlet indices the task shader hands to the mesh shader.
struct Payload
{
    uint meshletIndices[128];
};
groupshared Payload payload;

// A meshlet's bounding sphere vs the frustum. The sphere (`m.center`/`m.radius`) is in the mesh's
// object space, so it is first moved to world space by the instance `model` (the same `mul(model, v)`
// convention `lighting::transformVertex` uses), the radius scaled by the model's largest axis scale —
// then six half-space tests against the view-projection rows (Gribb–Hartmann): outside if the world
// sphere is fully behind any plane.
bool meshletVisible(Meshlet m, float4x4 model)
{
    float3 worldCenter = mul(model, float4(m.center, 1.0)).xyz;
    // Conservative world radius: object radius × the longest transformed basis axis (handles
    // non-uniform scale). Derived via `mul(model, axis)` so it shares the known-good matrix convention
    // rather than indexing rows/columns (whose storage order would otherwise have to be assumed).
    float sx = length(mul(model, float4(1.0, 0.0, 0.0, 0.0)).xyz);
    float sy = length(mul(model, float4(0.0, 1.0, 0.0, 0.0)).xyz);
    float sz = length(mul(model, float4(0.0, 0.0, 1.0, 0.0)).xyz);
    float worldRadius = m.radius * max(sx, max(sy, sz));

    float4x4 vp = push.viewProj;
    float4 rows[4] = { vp[0], vp[1], vp[2], vp[3] };
    float4 planes[6] = {
        rows[3] + rows[0], rows[3] - rows[0],  // left, right
        rows[3] + rows[1], rows[3] - rows[1],  // bottom, top
        rows[3] + rows[2], rows[3] - rows[2],  // near, far
    };
    for (int i = 0; i < 6; ++i)
    {
        float3 n = planes[i].xyz;
        float len = length(n);
        if (len < 1e-6) { continue; }
        float dist = (dot(n, worldCenter) + planes[i].w) / len;
        if (dist < -worldRadius) { return false; }
    }
    return true;
}

[shader("amplification")]
[numthreads(32, 1, 1)]
void taskMain(uint3 tid: SV_DispatchThreadID, uint lane: SV_GroupIndex)
{
    // Each lane tests one meshlet; survivors compact into the payload via a wave prefix count.
    bool visible = false;
    uint meshlet = push.meshletBase + tid.x;
    if (tid.x < push.meshletCount)
    {
        // The instance's model matrix moves the object-space bounding sphere into the world frustum.
        visible = meshletVisible(meshlets[meshlet], instances[push.instanceIndex].model);
    }
    uint slot = WavePrefixCountBits(visible);
    if (visible)
    {
        payload.meshletIndices[slot] = meshlet;
    }
    uint survivors = WaveActiveCountBits(visible);
    if (lane == 0)
    {
        DispatchMesh(survivors, 1, 1, payload);
    }
}

// Fetches one base vertex's (position, normal, uv0) from the raw stream at its global index
// (shifted by `vertexBase` — the deformed-buffer slice for a skinned/displaced batch).
void loadVertex(uint global, out float3 position, out float3 normal, out float2 uv0)
{
    uint base = (global + push.vertexBase) * VertexStride;
    position = vertexStream.Load<float3>(base);
    normal = vertexStream.Load<float3>(base + NormalOffset);
    uv0 = vertexStream.Load<float2>(base + UvOffset);
}

// One meshlet's local triangle: 3 packed u8 local indices at `triangleOffset + tri*3`.
uint3 loadTriangle(uint triangleOffset, uint tri)
{
    uint byteOffset = triangleOffset + tri * 3u;
    uint word = meshletTriangles.Load<uint>(byteOffset & ~3u);
    uint shift = (byteOffset & 3u) * 8u;
    // A triangle's 3 bytes may straddle a 4-byte word; read a second word when they do.
    uint next = meshletTriangles.Load<uint>((byteOffset & ~3u) + 4u);
    uint64_t combined = (uint64_t(next) << 32) | uint64_t(word);
    uint bytes = uint(combined >> shift);
    return uint3(bytes & 0xffu, (bytes >> 8) & 0xffu, (bytes >> 16) & 0xffu);
}

[outputtopology("triangle")]
[numthreads(128, 1, 1)]
[shader("mesh")]
void meshMain(
    uint tid: SV_GroupThreadID,
    uint gid: SV_GroupID,
    in payload Payload pl,
    out vertices VertexOutput verts[64],
    out indices uint3 tris[124])
{
    Meshlet m = meshlets[pl.meshletIndices[gid]];
    SetMeshOutputCounts(m.vertexCount, m.triangleCount);

    Instance inst = instances[push.instanceIndex];
    if (tid < m.vertexCount)
    {
        uint global = meshletVertices[m.vertexOffset + tid];
        float3 position, normal;
        float2 uv0;
        loadVertex(global, position, normal, uv0);

        float4 worldPos = mul(inst.model, float4(position, 1.0));
        VertexOutput o;
        o.worldPos = worldPos.xyz;
        o.position = mul(push.viewProj, worldPos);
        o.worldNormal = mul((float3x3)inst.normalMatrix, normal);
        o.uv0 = uv0;
        o.coverageAnchor = position;
        o.materialIndex = inst.texture.w;
        verts[tid] = o;
    }
    if (tid < m.triangleCount)
    {
        tris[tid] = loadTriangle(m.triangleOffset, tid);
    }
}
