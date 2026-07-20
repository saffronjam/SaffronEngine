// Aerial perspective volume fill using the shared bounded atmosphere LUT march.

import atmos_ap;

[[vk::binding(0, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(1, 0)]] Sampler2D<float4> multiScatterLut;
[[vk::image_format("rgba16f")]]
[[vk::binding(2, 0)]] RWTexture3D<float4> apVolume;
[[vk::binding(3, 0)]] ConstantBuffer<ApParams> params;

float3 froxelCenterWorld(uint3 id, float viewZ)
{
    float grid = params.apPlanes.z;
    float2 uv = (float2(id.xy) + 0.5) / grid;
    float2 ndc = uv * 2.0 - 1.0;
    float4 r = mul(params.invProjection, float4(ndc, 1.0, 1.0));
    float3 viewRay = r.xyz / r.w;
    float3 viewPos = viewRay * (viewZ / viewRay.z);
    return mul(params.invView, float4(viewPos, 1.0)).xyz;
}

[shader("compute")]
[numthreads(4, 4, 4)]
void computeMain(uint3 id : SV_DispatchThreadID)
{
    uint gridEdge = uint(params.apPlanes.z + 0.5);
    if (any(id >= gridEdge))
    {
        return;
    }

    float wFrac = (float(id.z) + 0.5) / params.apPlanes.z;
    float viewZ = -params.apPlanes.x * pow(params.apPlanes.y / params.apPlanes.x, wFrac);
    float3 worldPos = froxelCenterWorld(id, viewZ);
    float3 eye = mul(params.invView, float4(0.0, 0.0, 0.0, 1.0)).xyz;
    float3 toFroxel = worldPos - eye;
    float3 viewDir = normalize(toFroxel);
    float distKm = length(toFroxel) * params.apPlanes.w;
    AtmoSample sample = marchAtmosphere(
        atmosphereObserverPosition(params),
        viewDir,
        normalize(params.sunDir.xyz),
        distKm,
        transmittanceLut,
        multiScatterLut,
        params);
    float meanT = dot(sample.transmittance, float3(1.0 / 3.0));
    apVolume[id] = float4(sample.inScatter * params.params1.w, meanT);
}
