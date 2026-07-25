// The shader mirror of the shared deterministic wind field (`saffron-wind`): the
// same mean-advection + gust-front + fixed-phase multiscale turbulence math. The
// module is parameter-fed — the wind deformation prepass passes its push-constant
// words and lighting-side consumers pass the light UBO's wind words — so one
// implementation serves every sampler, and equal (position, time) inputs sample
// equal velocities everywhere.
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
