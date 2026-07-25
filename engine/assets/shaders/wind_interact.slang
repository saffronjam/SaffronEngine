// The world interaction field's per-frame step: one dispatch covers every texel of
// every cascade — reset slots whose stored world coordinate no longer matches the
// one their position derives (the recentring scroll), splat this frame's impulses
// into texel velocity, and integrate the damped oscillator back toward rest. The
// header's cascade centres are rewritten from the push so samplers agree with the
// texels the same frame.
import global_gpu_data;

// One swept impulse: a world-space XZ disc pushing along `direction` and pressing
// the ground down by `depress`, with a smooth falloff over `radius`.
struct WindInteractImpulse
{
    float2 position;
    float radius;
    float strength;
    float2 direction;
    float depress;
    float reserved;
};

struct WindInteractPush
{
    uint64_t field;
    uint64_t impulses;
    int2 center0;
    int2 center1;
    uint impulseCount;
    float dt;
    uint2 reserved;
};

[[vk::push_constant]] WindInteractPush pc;

// Spring stiffness and damping of the texel oscillator (critically-ish damped
// recovery over roughly a second).
static const float INTERACT_STIFFNESS = 40.0;
static const float INTERACT_DAMPING = 9.0;
static const float INTERACT_MAX_DISPLACEMENT = 1.0;
static const float INTERACT_MAX_DEPRESS = 0.5;

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid: SV_DispatchThreadID)
{
    if (pc.field == 0)
    {
        return;
    }
    uint total = GPU_INTERACTION_CASCADES * GPU_INTERACTION_TEXELS * GPU_INTERACTION_TEXELS;
    uint index = tid.x;
    if (index >= total)
    {
        return;
    }
    if (index == 0u)
    {
        GpuInteractionHeader header;
        header.centerTexel[0] = pc.center0;
        header.centerTexel[1] = pc.center1;
        header.generation = 0u;
        header.reserved = uint3(0u);
        *(GpuInteractionHeader*)pc.field = header;
    }
    uint cascade = index / (GPU_INTERACTION_TEXELS * GPU_INTERACTION_TEXELS);
    uint within = index % (GPU_INTERACTION_TEXELS * GPU_INTERACTION_TEXELS);
    uint2 slot = uint2(within % GPU_INTERACTION_TEXELS, within / GPU_INTERACTION_TEXELS);
    int2 center = cascade == 0u ? pc.center0 : pc.center1;
    int half = int(GPU_INTERACTION_TEXELS / 2u);
    int2 worldCoord = center + int2(int(slot.x) - half, int(slot.y) - half);

    GpuInteractionTexel* texel = (GpuInteractionTexel*)(
        pc.field + gpuInteractionTexelOffset(cascade, slot));
    GpuInteractionTexel state = *texel;
    if (state.worldCoord.x != worldCoord.x || state.worldCoord.y != worldCoord.y)
    {
        state.displacement = float2(0.0, 0.0);
        state.velocity = float2(0.0, 0.0);
        state.depress = 0.0;
        state.depressVelocity = 0.0;
        state.worldCoord = worldCoord;
    }

    float texelMeters = gpuInteractionTexelMeters(cascade);
    float2 texelCenter = (float2(worldCoord) + 0.5) * texelMeters;
    for (uint impulse = 0u; impulse < pc.impulseCount; ++impulse)
    {
        WindInteractImpulse* record =
            (WindInteractImpulse*)(pc.impulses + uint64_t(impulse) * 32u);
        WindInteractImpulse hit = *record;
        float radius = max(hit.radius, texelMeters);
        float distance = length(texelCenter - hit.position);
        if (distance >= radius)
        {
            continue;
        }
        float falloff = 1.0 - distance / radius;
        float weight = falloff * falloff;
        float2 direction = hit.direction;
        float lengthSq = dot(direction, direction);
        // A directionless impulse pushes radially outward from its centre.
        if (lengthSq < 1e-8)
        {
            direction = distance > 1e-4
                ? (texelCenter - hit.position) / distance
                : float2(0.0, 0.0);
        }
        else
        {
            direction *= rsqrt(lengthSq);
        }
        state.velocity += direction * (hit.strength * weight);
        state.depressVelocity += hit.depress * weight;
    }

    float dt = clamp(pc.dt, 0.0, 0.1);
    state.velocity += (-INTERACT_STIFFNESS * state.displacement
        - INTERACT_DAMPING * state.velocity) * dt;
    state.displacement += state.velocity * dt;
    float magnitude = length(state.displacement);
    if (magnitude > INTERACT_MAX_DISPLACEMENT)
    {
        state.displacement *= INTERACT_MAX_DISPLACEMENT / magnitude;
    }
    state.depressVelocity += (-INTERACT_STIFFNESS * state.depress
        - INTERACT_DAMPING * state.depressVelocity) * dt;
    state.depress = clamp(state.depress + state.depressVelocity * dt, 0.0, INTERACT_MAX_DEPRESS);
    *texel = state;
}
