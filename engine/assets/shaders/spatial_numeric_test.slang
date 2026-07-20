import spatial_numeric;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> output;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    uint4 zero = spatialPhilox4x32_10(uint4(0u), uint2(0u));
    output[0] = zero.x;
    output[1] = zero.y;
    output[2] = zero.z;
    output[3] = zero.w;

    SpatialRandomDomain domain;
    domain.map = uint4(0x44556677u, 0x00112233u, 0x89ABCDEFu, 0x01234567u);
    domain.nodeGuid = uint4(0x76543210u, 0xFEDCBA98u, 0x44332211u, 0x88776655u);
    domain.nodeSemanticRevision = 9u;
    domain.seedNamespace = uint4(0x90A0B0C0u, 0x50607080u, 0x10203040u, 0xCAFEBABEu);
    domain.cellLevel = 3u;
    domain.cellX = -5;
    domain.cellY = 7;
    domain.cellZ = -11;
    domain.candidate = 123456u;
    domain.ancestor = 789u;
    domain.species = uint4(0x55667788u, 0x11223344u, 0xCAFEBABEu, 0xDEADBEEFu);
    domain.channel = 4u;
    SpatialRandomStream stream = spatialRandomStream(domain);
    uint4 sample0 = spatialRandomSample(stream, uint64_t(0));
    uint4 sampleMax = spatialRandomSample(stream, ~uint64_t(0));
    output[4] = sample0.x;
    output[5] = sample0.y;
    output[6] = sample0.z;
    output[7] = sample0.w;
    output[8] = sampleMax.x;
    output[9] = sampleMax.y;
    output[10] = sampleMax.z;
    output[11] = sampleMax.w;

    bool valid = false;
    output[12] = uint(spatialFixedAdd(98304, -32768, valid));
    output[13] = uint(spatialFixedSub(98304, -32768, valid));
    output[14] = uint(spatialFixedMulQ16(98304, 43691, valid));
    output[15] = uint(spatialFixedDivQ16(98304, 43691, valid));
    output[16] = uint(spatialFixedLerpQ16(-131072, 131072, 32768u, valid));
    output[17] = uint(spatialFixedCurveSegmentQ16(32768u, 0u, 65535u, -131072, 131072, valid));
    output[18] = uint(spatialDivRoundTiesEven(5, 2, valid));
    output[19] = uint(spatialDivRoundTiesEven(-5, 2, valid));
    output[20] = uint(spatialDivRoundTiesEven(7, 2, valid));
    output[21] = uint(spatialDivRoundTiesEven(-7, 2, valid));
    spatialFixedAdd(2147483647, 1, valid);
    output[22] = valid ? 1u : 0u;
    spatialFixedDivQ16(1, 0, valid);
    output[23] = valid ? 1u : 0u;
    output[24] = spatialChanceFromDraw(0u, 0u) ? 1u : 0u;
    output[25] = spatialChanceFromDraw(0xFFFFFFFFu, 0u) ? 1u : 0u;
    output[26] = spatialChanceFromDraw(0u, 65535u) ? 1u : 0u;
    output[27] = spatialChanceFromDraw(0xFFFFFFFFu, 65535u) ? 1u : 0u;
    output[28] = spatialChanceFromDraw(0x80000000u, 32768u) ? 1u : 0u;
    output[29] = spatialChanceFromDraw(0x80010002u, 32768u) ? 1u : 0u;
    output[30] = spatialRandomChance(stream, uint64_t(0), 0u, 46076u) ? 1u : 0u;
    output[31] = spatialRandomChance(stream, uint64_t(0), 0u, 46077u) ? 1u : 0u;
}
