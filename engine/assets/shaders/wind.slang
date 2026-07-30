// The shader mirror of the shared deterministic wind field (`saffron-wind`): the
// same mean-advection + gust-front + fixed-phase multiscale turbulence math, plus
// the world interaction field the same consumers read. The module is parameter-fed —
// the wind deformation prepass passes its push-constant words and lighting-side
// consumers pass the light UBO's wind words — so one implementation serves every
// sampler, and equal (position, time) inputs sample equal velocities everywhere.
module wind;

static const float WIND_TAU = 6.28318530718;
// The base spatial wavelength of the coarsest turbulence octave, in metres.
static const float WIND_BASE_WAVELENGTH_M = 61.0;
// The advection rate of turbulence features relative to the mean speed.
static const float WIND_TURBULENCE_ADVECTION = 0.7;
// The spatial wavelength of a gust front along the mean direction, in metres.
static const float WIND_GUST_FRONT_WAVELENGTH_M = 190.0;

// One fixed per-octave phase derived from the seed (matches the CPU hash exactly).
float windOctavePhase(uint seed, uint octave, uint lane)
{
    uint hash = seed * 0x9e3779b9u + octave * 0x85ebca6bu + lane * 0xc2b2ae35u;
    hash ^= hash >> 16;
    hash *= 0x7feb352du;
    hash ^= hash >> 15;
    return (float(hash) / 4294967295.0) * WIND_TAU;
}

// Samples the global wind field at a world position (metres) and simulation time
// (seconds). Mirrors `saffron_wind::sample` term for term. `dirSpeedGust` packs
// (dir.x, dir.z, speed, gust); `params` packs (roughness, gustFrequency,
// referenceHeight, heightExponent).
public float3 sampleWindVelocity(
    float3 position,
    float time,
    float4 dirSpeedGust,
    float4 params,
    uint octaves,
    uint seed)
{
    float speed = dirSpeedGust.z;
    if (speed <= 0.0)
    {
        return float3(0.0, 0.0, 0.0);
    }
    float2 meanDir = dirSpeedGust.xy;
    float gust = dirSpeedGust.w;
    float roughness = clamp(params.x, 0.0, 1.0);
    float gustFrequency = params.y;
    float referenceHeight = max(params.z, 0.1);
    float heightExponent = params.w;

    float height = max(position.y, 0.0);
    float shear = heightExponent <= 0.0
        ? 1.0
        : pow(max(height / referenceHeight, 0.05), heightExponent);
    float meanSpeed = speed * shear;

    float along = position.x * meanDir.x + position.z * meanDir.y;
    float frontPhase = (along - meanSpeed * time) / WIND_GUST_FRONT_WAVELENGTH_M * WIND_TAU
        + time * gustFrequency * WIND_TAU;
    float front = sin(frontPhase) * 0.5 + 0.5;
    float gustFront = front * front;

    float3 turbulence = float3(0.0, 0.0, 0.0);
    float amplitude = 1.0;
    float total = 0.0;
    float3 advected = float3(
        position.x - meanDir.x * meanSpeed * WIND_TURBULENCE_ADVECTION * time,
        position.y,
        position.z - meanDir.y * meanSpeed * WIND_TURBULENCE_ADVECTION * time);
    for (uint octave = 0; octave < octaves; octave++)
    {
        float frequency = WIND_TAU / (WIND_BASE_WAVELENGTH_M / float(1u << octave));
        float px = windOctavePhase(seed, octave, 0);
        float py = windOctavePhase(seed, octave, 1);
        float pz = windOctavePhase(seed, octave, 2);
        turbulence += amplitude
            * float3(
                sin(advected.z * frequency + px) * cos(advected.y * frequency * 0.31 + pz),
                0.35 * sin(advected.x * frequency * 0.83 + py)
                    * cos(advected.z * frequency * 0.57 + px),
                sin(advected.x * frequency + pz) * cos(advected.y * frequency * 0.27 + py));
        total += amplitude;
        amplitude *= roughness;
    }
    if (total > 0.0)
    {
        turbulence /= total;
    }

    float gustSpeed = meanSpeed * gust;
    return float3(meanDir.x, 0.0, meanDir.y) * meanSpeed * (1.0 + gust * 0.5 * gustFront)
        + turbulence * gustSpeed;
}

// One local wind source as the frame's ring uploads it (mirrors
// `saffron_wind::LocalWindSource`; kind tags: 0 directional, 1 point, 2 vortex,
// 3 wake, 4 volume).
public struct WindSourceGpu
{
    public float3 position;
    public uint kind;
    public float3 direction;
    public float strength;
    public float radius;
    public float falloff;
    public float2 reserved;
};

public static const uint WIND_SOURCE_GPU_SIZE = 48u;

float3 windNormalizeOrZero(float3 value)
{
    float lengthSq = dot(value, value);
    return lengthSq > 1e-12 ? value * rsqrt(lengthSq) : float3(0.0, 0.0, 0.0);
}

// Samples the composed field: the global profile plus every uploaded local source.
// Mirrors `saffron_wind::sample_composed` term for term — `Volume` sources scale
// the global term, the rest add velocities, and the edge weight fades linearly
// over the falloff fraction.
public float3 sampleComposedWindVelocity(
    float3 position,
    float time,
    float4 dirSpeedGust,
    float4 params,
    uint octaves,
    uint seed,
    uint64_t sources,
    uint sourceCount)
{
    float3 global = sampleWindVelocity(position, time, dirSpeedGust, params, octaves, seed);
    if (sourceCount == 0u || sources == 0)
    {
        return global;
    }
    float globalScale = 1.0;
    float3 added = float3(0.0, 0.0, 0.0);
    float3 meanDirection = float3(dirSpeedGust.x, 0.0, dirSpeedGust.y);
    for (uint index = 0u; index < sourceCount; ++index)
    {
        WindSourceGpu source =
            *(WindSourceGpu*)(sources + uint64_t(index) * WIND_SOURCE_GPU_SIZE);
        float3 offset = position - source.position;
        float distance = length(offset);
        if (source.radius <= 0.0 || distance >= source.radius)
        {
            continue;
        }
        float inner = source.radius * (1.0 - clamp(source.falloff, 0.0, 1.0));
        float weight = (distance <= inner || source.radius <= inner)
            ? 1.0
            : 1.0 - (distance - inner) / (source.radius - inner);
        if (source.kind == 0u)
        {
            added += windNormalizeOrZero(source.direction) * source.strength * weight;
        }
        else if (source.kind == 1u)
        {
            added += windNormalizeOrZero(offset) * source.strength * weight;
        }
        else if (source.kind == 2u)
        {
            float3 planar = float3(offset.x, 0.0, offset.z);
            added += windNormalizeOrZero(float3(-planar.z, 0.0, planar.x))
                * source.strength * weight;
        }
        else if (source.kind == 3u)
        {
            added -= meanDirection * source.strength * weight;
        }
        else
        {
            globalScale *= 1.0 + (source.strength - 1.0) * weight;
        }
    }
    return global * max(globalScale, 0.0) + added;
}

// A stable per-instance hash from the instance's world root position, used to
// decorrelate neighbouring plants' sway phase. Roots are static placements, so the
// hash is identical every frame and in every pass.
public uint windInstanceHash(float3 root)
{
    uint hash = asuint(root.x) * 0x9e3779b9u ^ asuint(root.y) * 0x85ebca6bu
        ^ asuint(root.z) * 0xc2b2ae35u;
    hash ^= hash >> 16;
    return hash;
}

// The full sway displacement of a wind-deformed instance at its bounds top: the
// sampled velocity at a per-instance phase-jittered position, velocity-proportional
// with a soft cap so storms bend rather than tear. Vertex application scales this by
// a root-anchored height weight squared.
public float3 windSwayOffset(
    float3 position,
    float time,
    uint instanceHash,
    float4 dirSpeedGust,
    float4 params,
    uint octaves,
    uint seed,
    uint64_t sources,
    uint sourceCount)
{
    float phase = float(instanceHash & 1023u) * (1.0 / 1023.0);
    float3 jittered = position + float3(phase * 17.0, 0.0, phase * 23.0);
    float3 velocity = sampleComposedWindVelocity(
        jittered, time, dirSpeedGust, params, octaves, seed, sources, sourceCount);
    // ~0.06 m of sway per m/s, capped at 1.5 m of displacement.
    float3 offset = velocity * 0.06;
    float magnitude = length(offset);
    if (magnitude > 1.5)
    {
        offset *= 1.5 / magnitude;
    }
    return offset;
}

// The world interaction field: camera-centered cascades of damped-oscillator texels the
// interaction pass integrates each frame. Every texel stores its ABSOLUTE world texel coordinate;
// a texel whose stored coordinate mismatches the one its slot derives is at rest, so recentring
// never copies state.
public static const uint GPU_INTERACTION_CASCADES = 2u;
// Texels per cascade side.
public static const uint GPU_INTERACTION_TEXELS = 256u;
public static const uint GPU_INTERACTION_TEXEL_SIZE = 32u;
public static const uint GPU_INTERACTION_HEADER_SIZE = 32u;

// Metres per texel of cascade `cascade` (0.25 m, then ×4 per cascade).
public float gpuInteractionTexelMeters(uint cascade)
{
    return 0.25 * float(1u << (2u * cascade));
}

// Scalar reserved words, not a `uint3`: this block starts at offset 20, and std430 requires a
// three-component vector to start on a 16-byte boundary.
public struct GpuInteractionHeader
{
    // Per-cascade centre texel in absolute world texel coordinates.
    public int2 centerTexel[GPU_INTERACTION_CASCADES];
    public uint generation;
    public uint reserved0;
    public uint reserved1;
    public uint reserved2;
};

public struct GpuInteractionTexel
{
    public float2 displacement;
    public float2 velocity;
    public float depress;
    public float depressVelocity;
    public int2 worldCoord;
};

public uint64_t gpuInteractionTexelOffset(uint cascade, uint2 texel)
{
    return uint64_t(GPU_INTERACTION_HEADER_SIZE)
        + (uint64_t(cascade) * GPU_INTERACTION_TEXELS * GPU_INTERACTION_TEXELS
           + uint64_t(texel.y) * GPU_INTERACTION_TEXELS + texel.x)
            * GPU_INTERACTION_TEXEL_SIZE;
}

// One texel's live displacement, or rest when the slot is stale or out of range.
float3 gpuInteractionTexelValue(uint64_t field, uint cascade, int2 worldCoord, int2 center)
{
    int2 slot = worldCoord - center + int(GPU_INTERACTION_TEXELS / 2u);
    if (slot.x < 0 || slot.y < 0 || slot.x >= int(GPU_INTERACTION_TEXELS)
        || slot.y >= int(GPU_INTERACTION_TEXELS))
    {
        return float3(0.0, 0.0, 0.0);
    }
    GpuInteractionTexel* texel = (GpuInteractionTexel*)(
        field + gpuInteractionTexelOffset(cascade, uint2(uint(slot.x), uint(slot.y))));
    GpuInteractionTexel value = *texel;
    if (value.worldCoord.x != worldCoord.x || value.worldCoord.y != worldCoord.y)
    {
        return float3(0.0, 0.0, 0.0);
    }
    return float3(value.displacement.x, -value.depress, value.displacement.y);
}

// Whether `positionXZ` falls inside `cascade`'s bilinear-safe window around `center`,
// and the base texel coordinate + fraction of its footprint.
bool gpuInteractionFootprint(
    uint cascade, float2 positionXZ, int2 center, out int2 baseCoord, out float2 fraction)
{
    float texelMeters = gpuInteractionTexelMeters(cascade);
    float2 grid = positionXZ / texelMeters - 0.5;
    float2 base = floor(grid);
    fraction = grid - base;
    baseCoord = int2(int(base.x), int(base.y));
    int2 offset = baseCoord - center;
    int half = int(GPU_INTERACTION_TEXELS / 2u);
    return offset.x >= -half + 1 && offset.y >= -half + 1 && offset.x < half - 2
        && offset.y < half - 2;
}

// The finest cascade covering `positionXZ` for the given cascade centres, or
// `GPU_INTERACTION_CASCADES` when the position falls outside every cascade. The centres are
// arguments so a caller can ask the same question of the previous frame's field placement.
public uint gpuSceneInteractionCascade(float2 positionXZ, int2 center0, int2 center1)
{
    for (uint cascade = 0u; cascade < GPU_INTERACTION_CASCADES; ++cascade)
    {
        int2 baseCoord;
        float2 fraction;
        int2 center = cascade == 0u ? center0 : center1;
        if (gpuInteractionFootprint(cascade, positionXZ, center, baseCoord, fraction))
        {
            return cascade;
        }
    }
    return GPU_INTERACTION_CASCADES;
}

// The field's live cascade centres, as the interaction step wrote them this frame.
public void gpuSceneInteractionCenters(uint64_t field, out int2 center0, out int2 center1)
{
    center0 = int2(0, 0);
    center1 = int2(0, 0);
    if (field == 0)
    {
        return;
    }
    GpuInteractionHeader head = *(GpuInteractionHeader*)field;
    center0 = head.centerTexel[0];
    center1 = head.centerTexel[1];
}

// Bilinear world-space sample of the interaction field's displacement at a ground
// position: horizontal push in xz, ground depression as negative y. The finest
// cascade containing the position wins.
public float3 gpuSceneInteractionSample(uint64_t field, float2 positionXZ)
{
    if (field == 0)
    {
        return float3(0.0, 0.0, 0.0);
    }
    GpuInteractionHeader* header = (GpuInteractionHeader*)field;
    GpuInteractionHeader head = *header;
    for (uint cascade = 0u; cascade < GPU_INTERACTION_CASCADES; ++cascade)
    {
        int2 baseCoord;
        float2 fraction;
        int2 center = head.centerTexel[cascade];
        if (!gpuInteractionFootprint(cascade, positionXZ, center, baseCoord, fraction))
        {
            continue;
        }
        float3 v00 = gpuInteractionTexelValue(field, cascade, baseCoord, center);
        float3 v10 = gpuInteractionTexelValue(field, cascade, baseCoord + int2(1, 0), center);
        float3 v01 = gpuInteractionTexelValue(field, cascade, baseCoord + int2(0, 1), center);
        float3 v11 = gpuInteractionTexelValue(field, cascade, baseCoord + int2(1, 1), center);
        return lerp(
            lerp(v00, v10, fraction.x), lerp(v01, v11, fraction.x), fraction.y);
    }
    return float3(0.0, 0.0, 0.0);
}
