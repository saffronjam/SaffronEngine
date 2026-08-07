// Clustered-forward light culling. One invocation per froxel cluster: it builds the
// cluster's view-space AABB on the fly (from the screen tile + the slice's exponential
// Z planes) and appends every intersecting punctual light to the cluster's light list.
// The fragment shader (mesh.slang) then loops only its cluster's lights.
//
// The grid dimensions and per-cluster cap must match the renderer's and `mesh.slang`'s.
// Cluster index encoding: x + y*gridX + z*gridX*gridY.

static const uint MAX_LIGHTS_PER_CLUSTER = 64;

struct GpuLight
{
    float4 positionRange;   // xyz = world position, w = range
    float4 colorIntensity;
    float4 directionType;
    float4 spotCos;
};

struct ClusterParams
{
    float4x4 view;               // world -> view
    float4x4 inverseProjection;  // clip -> view
    uint4 gridSize;              // xyz = grid dims, w = light count
    uint4 screenSize;            // xy = pixels, z = clustered flag
    float4 zPlanes;              // x = near, y = far
};

struct Cluster
{
    uint count;
    uint indices[MAX_LIGHTS_PER_CLUSTER];
};

[[vk::binding(0, 0)]] ConstantBuffer<ClusterParams> params;
[[vk::binding(1, 0)]] StructuredBuffer<GpuLight> lights;
[[vk::binding(2, 0)]] RWStructuredBuffer<Cluster> clusters;

// Screen pixel + Vulkan NDC depth (0 = near) -> view-space point.
float3 screenToView(float2 screenPx, float ndcZ)
{
    float2 tex = screenPx / float2(params.screenSize.xy);
    float4 ndc = float4(tex.x * 2.0 - 1.0, tex.y * 2.0 - 1.0, ndcZ, 1.0);
    float4 view = mul(params.inverseProjection, ndc);
    return view.xyz / view.w;
}

// Intersect the eye ray through P (eye at view-space origin) with the plane z = zDist.
float3 rayToZ(float3 p, float zDist)
{
    return p * (zDist / p.z);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint clusterIndex = tid.x;
    uint total = params.gridSize.x * params.gridSize.y * params.gridSize.z;
    if (clusterIndex >= total)
    {
        return;
    }

    uint gx = clusterIndex % params.gridSize.x;
    uint gy = (clusterIndex / params.gridSize.x) % params.gridSize.y;
    uint gz = clusterIndex / (params.gridSize.x * params.gridSize.y);

    float2 tileSize = float2(params.screenSize.xy) / float2(params.gridSize.xy);
    float2 minSS = float2(float(gx), float(gy)) * tileSize;
    float2 maxSS = float2(float(gx + 1), float(gy + 1)) * tileSize;

    // The tile's view-space extent on the near plane.
    float3 minNearPlane = screenToView(minSS, 0.0);
    float3 maxNearPlane = screenToView(maxSS, 0.0);

    // Exponential view-space Z slice (negative: camera looks down -Z).
    float near = params.zPlanes.x;
    float far = params.zPlanes.y;
    float tileNear = -near * pow(far / near, float(gz) / float(params.gridSize.z));
    float tileFar = -near * pow(far / near, float(gz + 1) / float(params.gridSize.z));

    float3 a = rayToZ(minNearPlane, tileNear);
    float3 b = rayToZ(maxNearPlane, tileNear);
    float3 c = rayToZ(minNearPlane, tileFar);
    float3 d = rayToZ(maxNearPlane, tileFar);

    float3 aabbMin = min(min(a, b), min(c, d));
    float3 aabbMax = max(max(a, b), max(c, d));

    uint count = 0;
    uint lightCount = params.gridSize.w;
    for (uint i = 0; i < lightCount; i = i + 1)
    {
        GpuLight lt = lights[i];
        float3 posView = mul(params.view, float4(lt.positionRange.xyz, 1.0)).xyz;
        float radius = lt.positionRange.w;

        float3 closest = clamp(posView, aabbMin, aabbMax);
        float3 delta = posView - closest;
        if (dot(delta, delta) <= radius * radius)
        {
            if (count < MAX_LIGHTS_PER_CLUSTER)
            {
                clusters[clusterIndex].indices[count] = i;
                count = count + 1;
            }
        }
    }
    clusters[clusterIndex].count = count;
}
