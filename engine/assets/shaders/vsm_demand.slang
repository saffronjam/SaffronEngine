// Virtual-shadow receiver demand, pass 1: every camera depth pixel reconstructs
// its world position, projects into the directional light plane, picks the clip
// level whose texel density matches its screen footprint, and sets that page's bit
// in the demand bitmap. Pass 2 (`vsm_demand_compact.slang`) appends the set bits
// as compact requests the CPU drains.
import lighting_common;

[[vk::binding(0, 0)]] ConstantBuffer<LightGlobals> globals;
[[vk::binding(1, 0)]] Sampler2D<float> sceneDepth;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> demandBitmap;

struct VsmDemandPush
{
    float4x4 invViewProj;
    uint2 extent;
    uint2 reserved;
};

[[vk::push_constant]] VsmDemandPush pc;

static const uint VSM_LEVELS = 8u;
static const uint VSM_PAGES = 32u;

[shader("compute")]
[numthreads(8, 8, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (tid.x >= pc.extent.x || tid.y >= pc.extent.y || globals.vsmParams.z == 0.0)
    {
        return;
    }
    float2 uv = (float2(tid.xy) + 0.5) / float2(pc.extent);
    float depth = sceneDepth.SampleLevel(uv, 0.0);
    if (depth >= 1.0)
    {
        // Sky: no receiver.
        return;
    }
    float4 clip = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    float4 world = mul(pc.invViewProj, clip);
    if (abs(world.w) < 1e-6)
    {
        return;
    }
    float3 worldPos = world.xyz / world.w;
    float3 lightPos = mul(globals.vsmBasis, float4(worldPos, 1.0)).xyz;

    // The shadowed spot's receivers mark its projective pages (index base 8192).
    if (globals.spotShadow.y != 0u)
    {
        float4 spotClip = mul(globals.spotShadowViewProj, float4(worldPos, 1.0));
        if (spotClip.w > 0.0)
        {
            float3 ndc = spotClip.xyz / spotClip.w;
            if (abs(ndc.x) < 1.0 && abs(ndc.y) < 1.0 && ndc.z > 0.0 && ndc.z < 1.0)
            {
                float2 uv = ndc.xy * 0.5 + 0.5;
                uint2 page = uint2(uv * 16.0);
                uint index = 8192u + page.y * 16u + page.x;
                InterlockedOr(demandBitmap[index / 32u], 1u << (index % 32u));
            }
        }
    }

    // The shadowed point light's receivers mark the page of the cube face their
    // direction selects (index base 8448, 8x8 pages per face). The face-local
    // projection is the standard cube major-axis mapping, matching the face
    // matrices the page renderer uses.
    if (globals.pointShadowMeta.y != 0u)
    {
        float3 toFrag = worldPos - globals.pointShadow.xyz;
        float far = globals.pointShadow.w;
        float3 a = abs(toFrag);
        uint face;
        float axis;
        float2 st;
        if (a.x >= a.y && a.x >= a.z)
        {
            face = toFrag.x > 0.0 ? 0u : 1u;
            axis = a.x;
            st = float2(toFrag.x > 0.0 ? -toFrag.z : toFrag.z, -toFrag.y);
        }
        else if (a.y >= a.z)
        {
            face = toFrag.y > 0.0 ? 2u : 3u;
            axis = a.y;
            st = float2(toFrag.x, toFrag.y > 0.0 ? toFrag.z : -toFrag.z);
        }
        else
        {
            face = toFrag.z > 0.0 ? 4u : 5u;
            axis = a.z;
            st = float2(toFrag.z > 0.0 ? toFrag.x : -toFrag.x, -toFrag.y);
        }
        if (axis > 0.05 && axis < far)
        {
            float2 uv = clamp(st / axis * 0.5 + 0.5, 0.0, 0.999);
            uint2 page = uint2(uv * 8.0);
            uint index = 8448u + face * 64u + page.y * 8u + page.x;
            InterlockedOr(demandBitmap[index / 32u], 1u << (index % 32u));
        }
    }

    // The screen footprint of this pixel in world metres approximates the receiver
    // density; pick the finest level whose shadow texels are at least that dense.
    float2 uvStep = float2(1.0, -1.0) / float2(pc.extent) * 2.0;
    float4 worldRight = mul(pc.invViewProj, clip + float4(uvStep.x, 0.0, 0.0, 0.0));
    float3 neighbour = worldRight.xyz / max(abs(worldRight.w), 1e-6) * sign(worldRight.w);
    float footprint = max(length(neighbour - worldPos), 1e-4);

    for (uint level = 0u; level < VSM_LEVELS; ++level)
    {
        float4 window = globals.vsmLevels[level];
        if (window.z <= 0.0)
        {
            continue;
        }
        float texelMeters = window.z / (float(VSM_PAGES) * 128.0);
        // Coarser levels than the footprint needs add nothing; the first level
        // that both contains the receiver and is dense enough wins.
        float2 local = (lightPos.xy - window.xy) / window.z;
        if (local.x < 0.0 || local.y < 0.0 || local.x >= 1.0 || local.y >= 1.0)
        {
            continue;
        }
        if (texelMeters < footprint * 0.5 && level + 1u < VSM_LEVELS)
        {
            continue;
        }
        uint2 page = uint2(local * float(VSM_PAGES));
        uint index = level * VSM_PAGES * VSM_PAGES + page.y * VSM_PAGES + page.x;
        InterlockedOr(demandBitmap[index / 32u], 1u << (index % 32u));
        return;
    }
}
