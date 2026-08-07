// Instance visibility for the persistent GPU scene: one parameterized compute pipeline
// classifies every occupied instance slot for one view. The cull pass (kind 0) frustum-
// tests the instance's world sphere (prototype local sphere through the current
// transform) and, for instances visible last frame under a valid history, occlusion-
// tests the PREVIOUS transform's bounds against the previous HZB — occluded established
// instances land on the retest list instead of dying, and new/history-invalid instances
// bypass the stale pyramid. The retest pass (kind 1) re-tests the retest list against
// the CURRENT HZB with current transforms and merges survivors. Both passes write the
// per-slot visibility history word the next frame's cull reads.
//
// The reach pass (kind 2) serves global illumination, which is not a camera: a march
// gathers from behind the eye and a reflection shows what the camera cannot, so neither
// the frustum nor the depth pyramid is a sound rejection. It keeps every instance whose
// world sphere meets the reach box and rejects the rest.
//
// Set 0: b0 = counters (0 visible, 1 retest, 2 overflow flags), b1 = visible slot list,
// b2 = retest slot list, b3 = per-slot history words, b4 = HZB max pyramid,
// b5 = the GPU-scene address block.

import global_gpu_data;

public static const uint SCENE_VISIBILITY_PASS_CULL = 0u;
public static const uint SCENE_VISIBILITY_PASS_RETEST = 1u;
public static const uint SCENE_VISIBILITY_PASS_REACH = 2u;
public static const uint SCENE_VISIBILITY_OVERFLOW_VISIBLE = 1u;
public static const uint SCENE_VISIBILITY_OVERFLOW_RETEST = 2u;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> visibleList;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> retestList;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> history;
[[vk::binding(4, 0)]] Sampler2D<float> hzb;
[[vk::binding(5, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

struct Push
{
    float4x4 viewProj;
    float4x4 prevViewProj;
    uint2 hzbExtent;
    uint hzbMipCount;
    uint passKind;
    uint historyValid;
    uint listCapacity;
    // Alignment padding ahead of the reach corners, which are 16-byte aligned.
    uint2 reserved;
    // The reach pass's world box; `w` is unused.
    float4 reachMin;
    float4 reachMax;
};
[[vk::push_constant]] Push pc;

struct SphereBounds
{
    float3 center;
    float radius;
};

SphereBounds worldSphere(
    GpuSceneInstanceGpuRecord instance, uint slot, float4 localBounds, bool previous)
{
    // A static instance with explicit conservative bounds (vegetation) composes its
    // own current/previous local sphere instead of the prototype's.
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS) != 0u
        && instance.transformKind == GPU_SCENE_TRANSFORM_STATIC)
    {
        localBounds = instance.transform[previous ? 5u : 4u];
    }
    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, previous, c0, c1, c2, c3);
    float3 center = c0.xyz * localBounds.x + c1.xyz * localBounds.y + c2.xyz * localBounds.z
        + c3.xyz;
    float scale = max(length(c0.xyz), max(length(c1.xyz), length(c2.xyz)));
    SphereBounds sphere;
    sphere.center = center;
    sphere.radius = localBounds.w * scale;
    // A displaced instance's cooked sphere describes the UNDISPLACED surface, so it gains the
    // frame's relief bound. Local units, like the sphere it inflates, because the amplitude
    // rides the model matrix with the geometry.
    GpuDisplacedRow displaced;
    if (gpuSceneDisplacedRow(sceneAddresses, slot, displaced))
    {
        sphere.radius += displaced.localAmplitude * scale;
    }
    // A wind-deformed instance's sphere gains the prepass record's world-space
    // sway slack (the maximum of the current and previous displacement).
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_WIND) != 0u
        && sceneAddresses.windRecords != 0)
    {
        GpuWindInstanceRecord* record = (GpuWindInstanceRecord*)(
            sceneAddresses.windRecords + uint64_t(slot) * GPU_WIND_INSTANCE_RECORD_SIZE);
        sphere.radius += (*record).boundsInflation;
        // Counter word 19: deformed instances this view composed bounds for. Counted here rather
        // than in the prepass because the prepass has no counter binding, and because this is the
        // more useful number anyway: deformation work that reached a view, not slots that merely
        // carry the flag.
        if (!previous)
        {
            InterlockedAdd(counters[19], 1u);
            // Counter word 21: of those, the ones the interaction field's re-centring scroll
            // reset out from under this frame. Counted against word 19 on purpose — the two
            // share a denominator, so their ratio is the fraction of deformed instances whose
            // interaction displacement jumped rather than moved.
            if ((*record).interactionReset != 0.0)
            {
                InterlockedAdd(counters[21], 1u);
            }
        }
    }
    return sphere;
}

// Conservative screen bounds of the sphere's world AABB through `matrix`. Returns false
// (treat as visible) when any corner reaches behind the near plane.
bool screenBounds(SphereBounds sphere, float4x4 matrix, out float3 ndcMin, out float3 ndcMax)
{
    ndcMin = float3(1e30);
    ndcMax = float3(-1e30);
    for (uint corner = 0u; corner < 8u; ++corner)
    {
        float3 offset = float3(
            (corner & 1u) != 0u ? sphere.radius : -sphere.radius,
            (corner & 2u) != 0u ? sphere.radius : -sphere.radius,
            (corner & 4u) != 0u ? sphere.radius : -sphere.radius);
        float4 clip = mul(matrix, float4(sphere.center + offset, 1.0));
        if (clip.w <= 1e-5)
        {
            return false;
        }
        float3 ndc = clip.xyz / clip.w;
        ndcMin = min(ndcMin, ndc);
        ndcMax = max(ndcMax, ndc);
    }
    return true;
}

bool frustumCulled(float3 ndcMin, float3 ndcMax)
{
    return ndcMax.x < -1.0 || ndcMin.x > 1.0 || ndcMax.y < -1.0 || ndcMin.y > 1.0
        || ndcMax.z < 0.0 || ndcMin.z > 1.0;
}

// Whether the NDC box is provably behind the pyramid's stored depth.
bool hzbOccluded(float3 ndcMin, float3 ndcMax)
{
    float2 uvMin = float2(ndcMin.x, ndcMin.y) * 0.5 + 0.5;
    float2 uvMax = float2(ndcMax.x, ndcMax.y) * 0.5 + 0.5;
    uvMin = clamp(uvMin, 0.0, 1.0);
    uvMax = clamp(uvMax, 0.0, 1.0);
    float2 pixels = (uvMax - uvMin) * float2(pc.hzbExtent);
    float longest = max(max(pixels.x, pixels.y), 1.0);
    uint mip = min(uint(ceil(log2(longest))), pc.hzbMipCount - 1u);
    float depth0 = hzb.SampleLevel(float2(uvMin.x, uvMin.y), float(mip));
    float depth1 = hzb.SampleLevel(float2(uvMax.x, uvMin.y), float(mip));
    float depth2 = hzb.SampleLevel(float2(uvMin.x, uvMax.y), float(mip));
    float depth3 = hzb.SampleLevel(float2(uvMax.x, uvMax.y), float(mip));
    float farthest = max(max(depth0, depth1), max(depth2, depth3));
    return ndcMin.z > farthest;
}

void appendVisible(uint slot)
{
    uint index;
    InterlockedAdd(counters[0], 1u, index);
    if (index < pc.listCapacity)
    {
        visibleList[index] = slot;
    }
    else
    {
        InterlockedOr(counters[2], SCENE_VISIBILITY_OVERFLOW_VISIBLE);
    }
    history[slot] = 1u;
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    if (pc.passKind == SCENE_VISIBILITY_PASS_RETEST)
    {
        uint pending = min(counters[1], pc.listCapacity);
        if (tid.x >= pending)
        {
            return;
        }
        uint slot = retestList[tid.x];
        let instance = gpuSceneLoadInstance(addresses, slot);
        let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
        SphereBounds sphere = worldSphere(instance, slot, prototype.bounds, false);
        float3 ndcMin;
        float3 ndcMax;
        if (!screenBounds(sphere, pc.viewProj, ndcMin, ndcMax) || !hzbOccluded(ndcMin, ndcMax))
        {
            appendVisible(slot);
        }
        else
        {
            // Counter word 14: the retest confirmed occlusion — the instance stays
            // hidden this frame.
            InterlockedAdd(counters[14], 1u);
        }
        return;
    }

    uint slot = tid.x;
    if (slot >= addresses.instanceCapacity)
    {
        return;
    }
    let header = gpuSceneSlotHeader(addresses.instances, GPU_SCENE_INSTANCE_SLOT_STRIDE, slot);
    if (header.occupied == 0u)
    {
        if (slot < pc.listCapacity)
        {
            history[slot] = 0u;
        }
        return;
    }
    let instance = gpuSceneLoadInstance(addresses, slot);
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD) != 0u)
    {
        // A field instance anchors micro-blade records only; its tiles cull per
        // texel in the micro pass, never through instance bounds.
        history[slot] = 0u;
        return;
    }
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    SphereBounds sphere = worldSphere(instance, slot, prototype.bounds, false);
    if (pc.passKind == SCENE_VISIBILITY_PASS_REACH)
    {
        // Reach keeps whatever a gather can read, which is a world-box test and nothing
        // else: an occluder behind the eye still shadows what the eye sees, and one the
        // depth pyramid hides still bounces light onto what is in front of it.
        if (any(sphere.center + sphere.radius < pc.reachMin.xyz)
            || any(sphere.center - sphere.radius > pc.reachMax.xyz))
        {
            // Counter word 22: instances outside the window any march can reach.
            InterlockedAdd(counters[22], 1u);
            history[slot] = 0u;
            return;
        }
        appendVisible(slot);
        return;
    }
    float3 ndcMin;
    float3 ndcMax;
    bool bounded = screenBounds(sphere, pc.viewProj, ndcMin, ndcMax);
    if (bounded && frustumCulled(ndcMin, ndcMax))
    {
        // Counter word 13: the frustum rejected the instance outright.
        InterlockedAdd(counters[13], 1u);
        history[slot] = 0u;
        return;
    }

    bool established = history[slot] != 0u;
    if (pc.historyValid != 0u && established)
    {
        // Established instances test last frame's bounds against the previous pyramid;
        // an occluded one waits on the retest list for the current pyramid.
        SphereBounds prevSphere = worldSphere(instance, slot, prototype.bounds, true);
        float3 prevMin;
        float3 prevMax;
        if (screenBounds(prevSphere, pc.prevViewProj, prevMin, prevMax)
            && hzbOccluded(prevMin, prevMax))
        {
            uint index;
            InterlockedAdd(counters[1], 1u, index);
            if (index < pc.listCapacity)
            {
                retestList[index] = slot;
            }
            else
            {
                InterlockedOr(counters[2], SCENE_VISIBILITY_OVERFLOW_RETEST);
            }
            history[slot] = 0u;
            return;
        }
    }
    appendVisible(slot);
}
