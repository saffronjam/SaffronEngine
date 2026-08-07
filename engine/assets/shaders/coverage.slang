// Canonical material coverage sampling and classification shared by every raster pass.

public static const uint COVERAGE_SOURCE_ALBEDO_ALPHA = 0u;
public static const uint COVERAGE_SOURCE_TEXTURE = 1u;
public static const uint COVERAGE_SOURCE_MODELED_GEOMETRY = 2u;

public static const uint COVERAGE_CLASS_OPAQUE = 0u;
public static const uint COVERAGE_CLASS_MASKED = 1u;
public static const uint COVERAGE_CLASS_TRANSMISSIVE = 2u;

public struct CoverageSample
{
    public float alpha;
    public bool covered;
};

uint coverageMix(uint value)
{
    value ^= value >> 16u;
    value *= 0x7feb352du;
    value ^= value >> 15u;
    value *= 0x846ca68bu;
    value ^= value >> 16u;
    return value;
}

float spatialCoverageThreshold(
    float3 anchorPos,
    float2 uv,
    uint2 sourceExtent,
    uint2 salt,
    uint temporalPhase)
{
    int3 objectCell = int3(floor(anchorPos * 4096.0));
    int2 texel = int2(floor(frac(uv) * float2(max(sourceExtent, uint2(1u)))));
    uint hash = coverageMix(asuint(objectCell.x) ^ salt.x);
    hash = coverageMix(hash ^ asuint(objectCell.y));
    hash = coverageMix(hash ^ asuint(objectCell.z));
    hash = coverageMix(hash ^ asuint(texel.x) ^ (asuint(texel.y) * 0x9e3779b9u));
    hash = coverageMix(hash ^ salt.y ^ (temporalPhase * 0x85ebca6bu));
    return (float(hash) + 0.5) * (1.0 / 4294967296.0);
}

/// Samples and classifies one canonical coverage source.
///
/// A thin-sheet coverage texture stores direct coverage probability at every mip;
/// a standard masked texture supplies authored alpha plus its reference cutoff.
/// `alphaWidth` is a raster derivative (zero for ray classification), and
/// `alphaToCoverage` requests the MSAA sample-mask alpha instead of a binary hash.
public CoverageSample classifyCanonicalCoverage(
    float sampled,
    float2 uv,
    float3 anchorPos,
    uint sourceKind,
    uint classification,
    float baseColorAlpha,
    uint2 sourceExtent,
    uint2 salt,
    uint temporalPhase,
    float referenceCutoff,
    float alphaWidth,
    bool canonicalProbability,
    bool alphaToCoverage)
{
    CoverageSample result;
    if (sourceKind == COVERAGE_SOURCE_MODELED_GEOMETRY)
    {
        result.alpha = 1.0;
        result.covered = true;
        return result;
    }
    if (classification == COVERAGE_CLASS_OPAQUE)
    {
        result.alpha = 1.0;
        result.covered = true;
        return result;
    }

    float probability = sampled;
    float filteredWidth = alphaWidth;
    if (sourceKind == COVERAGE_SOURCE_ALBEDO_ALPHA)
    {
        probability *= baseColorAlpha;
        filteredWidth *= abs(baseColorAlpha);
    }
    if (!canonicalProbability)
    {
        probability = saturate((probability - referenceCutoff) / max(filteredWidth, 1e-5) + 0.5);
    }
    result.alpha = probability;
    if (classification == COVERAGE_CLASS_TRANSMISSIVE || alphaToCoverage)
    {
        result.covered = probability > 0.0;
        return result;
    }
    float threshold = spatialCoverageThreshold(anchorPos, uv, sourceExtent, salt, temporalPhase);
    result.covered = probability >= threshold;
    return result;
}

public CoverageSample sampleCanonicalCoverage(
    Sampler2D source,
    float2 uv,
    float3 anchorPos,
    uint sourceKind,
    uint classification,
    float baseColorAlpha,
    uint2 sourceExtent,
    uint2 salt,
    uint temporalPhase,
    float referenceCutoff,
    bool canonicalProbability,
    bool alphaToCoverage)
{
    float sampled = source.Sample(uv).a;
    return classifyCanonicalCoverage(
        sampled,
        uv,
        anchorPos,
        sourceKind,
        classification,
        baseColorAlpha,
        sourceExtent,
        salt,
        temporalPhase,
        referenceCutoff,
        fwidth(sampled),
        canonicalProbability,
        alphaToCoverage);
}
