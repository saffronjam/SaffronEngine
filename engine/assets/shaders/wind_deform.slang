// The wind deformation prepass: one dispatch over the world's instance slots samples
// the shared wind field once per wind-flagged instance and stores its sway record —
// current and previous frame times both evaluated here (the field is a pure function,
// so the previous sway is recomputed exactly, never carried across frames). Every
// raster pass applies the stored record and the visibility cull adds its bounds
// slack; no other shader evaluates the field.
import global_gpu_data;
import wind;

// The frame's wind words: direction/speed/gust and sampling parameters as authored,
// the octave count and phase seed, and the current + previous simulation seconds.
struct WindDeformPush
{
    float4 dirSpeedGust;
    float4 params;
    uint octaves;
    uint seed;
    float timeCurrent;
    float timePrevious;
    uint64_t sources;
    uint sourceCount;
    uint reserved;
};

[[vk::push_constant]] WindDeformPush pc;

[[vk::binding(5, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    uint slot = tid.x;
    if (slot >= addresses.instanceCapacity || addresses.windRecords == 0)
    {
        return;
    }
    GpuWindInstanceRecord* out = (GpuWindInstanceRecord*)(
        addresses.windRecords + uint64_t(slot) * GPU_WIND_INSTANCE_RECORD_SIZE);
    let header = gpuSceneSlotHeader(addresses.instances, GPU_SCENE_INSTANCE_SLOT_STRIDE, slot);
    if (header.occupied == 0u)
    {
        *out = GpuWindInstanceRecord(
            float3(0.0), 0.0, float3(0.0), 0.0, float3(0.0), 0.0, float3(0.0), 0.0,
            float4(0.0), 0.0, 0.0, float2(0.0));
        return;
    }
    let instance = gpuSceneLoadInstance(addresses, slot);
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_WIND) == 0u)
    {
        *out = GpuWindInstanceRecord(
            float3(0.0), 0.0, float3(0.0), 0.0, float3(0.0), 0.0, float3(0.0), 0.0,
            float4(0.0), 0.0, 0.0, float2(0.0));
        return;
    }

    // The instance-local bounds sphere: the vegetation static payload's explicit
    // conservative sphere when present, else the prototype sphere.
    float4 localBounds;
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS) != 0u
        && instance.transformKind == GPU_SCENE_TRANSFORM_STATIC)
    {
        localBounds = instance.transform[4];
    }
    else
    {
        let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
        localBounds = prototype.bounds;
    }
    float topLocalY = localBounds.y + localBounds.w;
    float heightScale = 1.0 / max(topLocalY, 0.05);

    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    float3 worldTop = c0.xyz * localBounds.x + c1.xyz * topLocalY + c2.xyz * localBounds.z
        + c3.xyz;
    uint hash = windInstanceHash(c3.xyz);

    float3 swayCurrent = windSwayOffset(
        worldTop, pc.timeCurrent, hash, pc.dirSpeedGust, pc.params, pc.octaves, pc.seed,
        pc.sources, pc.sourceCount);
    float3 swayPrevious = windSwayOffset(
        worldTop, pc.timePrevious, hash, pc.dirSpeedGust, pc.params, pc.octaves, pc.seed,
        pc.sources, pc.sourceCount);
    // The interaction field is stateful: this frame's sample becomes the record's
    // current words and last frame's current words become the previous words.
    float3 interactionPrevious = (*out).interactionCurrent;
    float3 interaction = gpuSceneInteractionSample(addresses.interactionField, c3.xz);
    // The branch mode: one oscillation whose angular frequency follows the shear-
    // scaled mean speed over the plant height (taller plants swing slower) and whose
    // amplitude follows the sampled speed; the quadrature at both frame times lets a
    // per-use phase offset apply with no time in the vertex path.
    float meanSpeed = length(swayCurrent) / 0.06;
    float plantHeight = max(topLocalY, 0.5);
    float branchOmega = clamp(6.0 / plantHeight + meanSpeed * 0.15, 0.8, 14.0);
    float branchAmplitude = min(meanSpeed * 0.02, 0.5) * min(plantHeight * 0.25, 1.0);
    float flutterAmplitude = min(meanSpeed * 0.012, 0.2);
    float4 branchQuadrature = float4(
        sin(branchOmega * pc.timeCurrent),
        cos(branchOmega * pc.timeCurrent),
        sin(branchOmega * pc.timePrevious),
        cos(branchOmega * pc.timePrevious));
    float inflation = max(length(swayCurrent), length(swayPrevious))
        + max(length(interaction), length(interactionPrevious))
        + branchAmplitude + flutterAmplitude;
    *out = GpuWindInstanceRecord(
        swayCurrent,
        heightScale,
        swayPrevious,
        inflation,
        interaction,
        0.0,
        interactionPrevious,
        0.0,
        branchQuadrature,
        branchAmplitude,
        flutterAmplitude,
        float2(0.0));
}
