module thin_sheet;

public struct ThinSheetEnergyPartition
{
    public float reflection;
    public float3 transmission;
};

/// Partitions incident energy between reflection and Beer-Lambert transmission.
public ThinSheetEnergyPartition thinSheetEnergyPartition(
    float faceResponse,
    float thickness,
    float3 absorption,
    float3 transmissionTint,
    float energyLimit,
    float cosine)
{
    ThinSheetEnergyPartition partition;
    energyLimit = saturate(energyLimit);
    partition.reflection = clamp(faceResponse, 0.0, energyLimit);
    float remaining = energyLimit - partition.reflection;
    float opticalDepth = max(thickness, 0.0) / max(abs(cosine), 0.08);
    float3 beer = exp(-max(absorption, float3(0.0)) * opticalDepth);
    partition.transmission = remaining * saturate(transmissionTint) * beer;
    return partition;
}
