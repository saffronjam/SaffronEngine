// GPU harness for the ray-candidate coverage contract: reads each case's identity from the
// per-frame ray-instance table (gpuSceneRayInstance), resolves the candidate surface through
// the persistent-scene address block (gpuSceneResolveCandidate) and classifies it exactly as
// the ray-query candidate loops do (explicit-LOD sample supplied by the case, zero alpha
// width, no alpha-to-coverage), writing every resolved word for the Rust fixture to compare
// byte-exactly against the CPU classifier.
import global_gpu_data;
import coverage;

struct CandidateCase
{
    uint rayInstance;
    uint primitive;
    float sampledAlpha;
    uint geometryIndex;
    float2 barycentrics;
    uint reserved0;
    uint reserved1;
};

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> output;
[[vk::binding(1, 0)]] StructuredBuffer<GpuSceneAddressBlock> addressBlocks;
[[vk::binding(2, 0)]] StructuredBuffer<CandidateCase> cases;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = addressBlocks[0];
    let testCase = cases[tid.x];
    let identity = gpuSceneRayInstance(addresses, testCase.rayInstance);
    let surface = gpuSceneResolveCandidate(
        addresses,
        identity,
        testCase.geometryIndex,
        testCase.primitive,
        testCase.barycentrics);

    uint base = tid.x * 20u;
    output[base + 0u] = surface.valid ? 1u : 0u;
    output[base + 1u] = asuint(surface.uv.x);
    output[base + 2u] = asuint(surface.uv.y);
    output[base + 3u] = asuint(surface.anchor.x);
    output[base + 4u] = asuint(surface.anchor.y);
    output[base + 5u] = asuint(surface.anchor.z);
    output[base + 6u] = surface.valid ? surface.coverage.sourceKind : 0u;
    output[base + 7u] = surface.valid ? surface.coverage.classification : 0u;
    output[base + 8u] = surface.valid ? surface.coverage.sourceExtent.x : 0u;
    output[base + 9u] = surface.valid ? surface.coverage.sourceExtent.y : 0u;
    output[base + 10u] = surface.canonicalProbability ? 1u : 0u;
    output[base + 11u] = asuint(surface.baseColorAlpha);

    // The ray-candidate verdict: an unresolved surface counts as covered, exactly as
    // gpuSceneRayCandidateCovered treats it.
    float alpha = 1.0;
    uint covered = 1u;
    if (surface.valid)
    {
        CoverageSample sample = classifyCanonicalCoverage(
            testCase.sampledAlpha,
            surface.uv,
            surface.anchor,
            surface.coverage.sourceKind,
            surface.coverage.classification,
            surface.baseColorAlpha,
            surface.coverage.sourceExtent,
            surface.coverage.hashSalt,
            addresses.coverageTemporalPhase,
            surface.coverage.cutoff,
            0.0,
            surface.canonicalProbability,
            false);
        alpha = sample.alpha;
        covered = sample.covered ? 1u : 0u;
    }
    output[base + 12u] = asuint(alpha);
    output[base + 13u] = covered;
    output[base + 14u] = surface.valid ? asuint(surface.coverage.cutoff) : 0u;
    output[base + 15u] = surface.valid ? surface.coverage.hashSalt.x : 0u;

    // The identity the table returned, so a bad table read is distinguishable from a bad resolve.
    output[base + 16u] = identity.instanceSlot;
    output[base + 17u] = identity.firstSubmesh;
    output[base + 18u] = identity.clusterCount;
    output[base + 19u] = identity.clusterCorners != 0 ? 1u : 0u;
}
