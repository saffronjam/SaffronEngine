import thin_sheet;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> outputValues;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    ThinSheetEnergyPartition front = thinSheetEnergyPartition(
        0.625, 0.003, float3(0.5, 1.0, 2.0), float3(1.0, 0.75, 0.5), 0.9, 0.25);
    ThinSheetEnergyPartition back = thinSheetEnergyPartition(
        0.25, 0.003, float3(0.5, 1.0, 2.0), float3(1.0, 0.75, 0.5), 0.9, 0.25);
    outputValues[0] = asuint(front.reflection);
    outputValues[1] = asuint(front.transmission.x);
    outputValues[2] = asuint(front.transmission.y);
    outputValues[3] = asuint(front.transmission.z);
    outputValues[4] = asuint(back.reflection);
    outputValues[5] = asuint(back.transmission.x);
    outputValues[6] = asuint(back.transmission.y);
    outputValues[7] = asuint(back.transmission.z);
}
