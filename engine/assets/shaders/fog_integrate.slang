// Froxel volumetric fog — integration (Hillaire 2015 energy-conserving slice). One thread per XY
// column, marching front-to-back along Z and writing the running accumulation into the integration
// volume. Each slice adds its in-scatter scaled by the *analytically-integrated* transmittance across
// the slice, `S_int = (S - S*exp(-sigma*d)) / max(sigma, 1e-5)`, which removes the density-dependent
// banding of summing `S*T` at slice centers (Wronski's naive accumulation).
//
// Both volumes are bound as storage images in GENERAL (the scatter volume is read, the integration
// volume written) — no layout transition between the inject write and this read. `d` is the world
// thickness of each exponential-Z slice, mirroring the inject pass's distance mapping.

// Mirrors `froxel_fog::FogGridParams` (272 B std140). The integrate pass reads only `gridSize` +
// `zPlanes`, but the full field set must precede them so the offsets match the shared UBO.
struct FogGrid
{
    float4x4 invProjection;
    float4x4 invView;
    float4x4 prevViewProj;
    uint4 gridSize;    // xyz froxel dims, w light count
    float4 screenSize; // xy offscreen pixels
    float4 zPlanes;    // x near, y far, z froxel-far coverage
    float4 temporal;   // x historyBlend, y historyValid, z neighborhoodClamp, w lightClamp
    float4 jitter;     // xy sub-froxel jitter, z jitter phase index
};

[[vk::image_format("rgba16f")]]
[[vk::binding(0, 0)]] RWTexture3D<float4> scatterExtinction; // read (L_scat, sigma_t)
[[vk::image_format("rgba16f")]]
[[vk::binding(1, 0)]] RWTexture3D<float4> integration;        // write (accum, transmittance)
[[vk::binding(2, 0)]] ConstantBuffer<FogGrid> grid;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (tid.x >= grid.gridSize.x || tid.y >= grid.gridSize.y)
    {
        return;
    }

    float near = grid.zPlanes.x;
    float far = grid.zPlanes.y;
    uint nz = grid.gridSize.z;

    float3 accum = float3(0.0);
    float totalT = 1.0;
    for (uint z = 0; z < nz; z = z + 1)
    {
        float4 se = scatterExtinction[uint3(tid.xy, z)];
        float sigma = max(se.a, 0.0);

        // The exponential-Z slice's world thickness (near-edge to far-edge distance).
        float zNear = near * pow(far / near, float(z) / float(nz));
        float zFar = near * pow(far / near, float(z + 1) / float(nz));
        float d = zFar - zNear;

        float sliceT = exp(-sigma * d);
        float3 sInt = (se.rgb - se.rgb * sliceT) / max(sigma, 1e-5);
        accum += totalT * sInt;
        totalT *= sliceT;
        integration[uint3(tid.xy, z)] = float4(accum, totalT);
    }
}
