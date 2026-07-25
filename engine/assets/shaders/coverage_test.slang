import coverage;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> output;

[shader("compute")]
[numthreads(8, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint fixture = tid.x;
    CoverageSample sample;
    if (fixture == 0u)
    {
        sample = classifyCanonicalCoverage(
            0.625,
            float2(0.125, 0.625),
            float3(0.25, -0.5, 1.0),
            COVERAGE_SOURCE_ALBEDO_ALPHA,
            COVERAGE_CLASS_TRANSMISSIVE,
            0.5,
            uint2(64u, 32u),
            uint2(0x55667788u, 0x11223344u),
            7u,
            0.25,
            0.5,
            false,
            false);
    }
    else if (fixture == 1u || fixture == 2u)
    {
        sample = classifyCanonicalCoverage(
            0.25,
            float2(0.2, 0.3),
            float3(1.0, 2.0, 3.0),
            COVERAGE_SOURCE_TEXTURE,
            COVERAGE_CLASS_MASKED,
            0.5,
            uint2(64u, 64u),
            uint2(7u, 0u),
            2u,
            0.5,
            0.0,
            true,
            fixture == 2u);
    }
    else if (fixture == 3u)
    {
        sample = classifyCanonicalCoverage(
            0.0,
            float2(0.2, 0.3),
            float3(1.0, 2.0, 3.0),
            COVERAGE_SOURCE_TEXTURE,
            COVERAGE_CLASS_TRANSMISSIVE,
            1.0,
            uint2(64u, 64u),
            uint2(7u, 0u),
            2u,
            0.5,
            0.0,
            true,
            false);
    }
    else if (fixture == 4u)
    {
        sample = classifyCanonicalCoverage(
            0.0,
            float2(0.0),
            float3(0.0),
            COVERAGE_SOURCE_MODELED_GEOMETRY,
            COVERAGE_CLASS_MASKED,
            0.0,
            uint2(1u),
            uint2(1u, 0u),
            0u,
            0.5,
            0.0,
            true,
            false);
    }
    else if (fixture == 5u)
    {
        sample = classifyCanonicalCoverage(
            0.0,
            float2(0.0),
            float3(0.0),
            COVERAGE_SOURCE_TEXTURE,
            COVERAGE_CLASS_OPAQUE,
            0.0,
            uint2(1u),
            uint2(1u, 0u),
            0u,
            0.5,
            0.0,
            true,
            false);
    }
    else if (fixture == 6u)
    {
        sample = classifyCanonicalCoverage(
            0.75,
            float2(0.2, 0.3),
            float3(1.0, 2.0, 3.0),
            COVERAGE_SOURCE_TEXTURE,
            COVERAGE_CLASS_MASKED,
            1.0,
            uint2(64u, 64u),
            uint2(7u, 0u),
            2u,
            0.5,
            0.0,
            false,
            false);
    }
    else
    {
        sample = classifyCanonicalCoverage(
            0.5,
            float2(0.2, 0.3),
            float3(1.0, 2.0, 3.0),
            COVERAGE_SOURCE_ALBEDO_ALPHA,
            COVERAGE_CLASS_TRANSMISSIVE,
            0.5,
            uint2(64u, 64u),
            uint2(7u, 0u),
            2u,
            0.5,
            0.0,
            true,
            false);
    }
    uint outputIndex = fixture * 2u;
    output[outputIndex] = asuint(sample.alpha);
    output[outputIndex + 1u] = sample.covered ? 1u : 0u;
}
