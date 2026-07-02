module giprobe;

// DDGI probe-cage sampling, shared by the mesh forward shade (`lighting`) and the half-res
// screen-space GI resolve (`gi_resolve`) — one copy of the 8-probe interpolation (trilinear ×
// soft-backface × Chebyshev visibility). The module declares NO bindings: the two probe atlases
// (irradiance + distance-moment) and the per-frame volume placement are passed in as parameters, so
// each consumer keeps its atlases in whatever descriptor set its pipeline layout uses (the mesh at
// set 5, the resolve pass in its own single set) — no shared set number, no globals binding here.

/// The camera-centered probe clipmap's placement this frame (each consumer fills it from its own
/// globals: the mesh light UBO, the resolve pass's push/UBO).
public struct DdgiVolume
{
    public float4 volumeMin;     // xyz = probe volume world min
    public float4 volumeExtent;  // xyz = probe volume world size
    public uint4 probeCount;     // xyz = probes per axis, w = irradiance tile interior
    public uint4 scrollBase;     // xyz = toroidal scroll base (wrapMod(snapBase, count))
    public float4 eyePosition;   // xyz = world-space camera position (surface-bias view vector)
};

float2 ddgiOctEncode(float3 d)
{
    d /= (abs(d.x) + abs(d.y) + abs(d.z));
    float2 o = d.z >= 0.0 ? d.xy : (1.0 - abs(d.yx)) * float2(d.x >= 0.0 ? 1.0 : -1.0, d.y >= 0.0 ? 1.0 : -1.0);
    return o * 0.5 + 0.5;
}

// Atlas UV for probe `p` sampling direction `dir`, given the tile interior + gutter.
float2 ddgiAtlasUv(uint probesX, uint3 p, float3 dir, uint interior, uint2 atlasSize)
{
    uint tile = interior + 2;
    uint tileX = p.x + p.y * probesX;
    uint tileY = p.z;
    float2 oct = ddgiOctEncode(normalize(dir));
    float2 interiorPx = oct * float(interior) + 1.0;
    float2 atlasPx = float2(tileX, tileY) * float(tile) + interiorPx;
    return (atlasPx + 0.5) / float2(atlasSize);
}

// Sample the 8-probe cage around worldPos: trilinear weight * soft backface * Chebyshev
// visibility (kills light leaking). Returns rgb = irradiance, w = coverage.
//
// Coverage is the *cage-presence* signal that lerps DDGI over the analytic IBL diffuse, so it
// must read ~1 anywhere inside the probe cage and fall to 0 only outside it. Coverage is the
// trilinear mass of the corners that are genuinely IN the cage — a corner whose probe index is
// clamped to the edge (i.e. the sample lies outside the volume on that side) is excluded from
// `covsum`. Inside the cage all eight corners are in-bounds and the trilinear weights are a
// partition of unity, so coverage reads exactly 1; only at/past a boundary cell do out-of-cage
// corners drop, ramping coverage to 0 so a surface outside the camera-centered cage falls back to
// analytic IBL. The clamped edge probes still feed the per-probe *irradiance* average (`wsum`) so
// the DDGI colour is defined right up to the boundary; they just do not count toward replacement.
// The soft-backface, low-weight-crush, and Chebyshev terms also shape `wsum` only — folding them
// into coverage would drive it below 1 deep inside the cage and leave a residual indoor skybox leak.
public float4 ddgiSampleIrradiance(
    DdgiVolume vol,
    Sampler2D irradianceAtlas,
    Sampler2D distanceAtlas,
    float3 worldPos,
    float3 n)
{
    uint3 pc = vol.probeCount.xyz;
    uint interior = vol.probeCount.w;
    uint2 irrAtlas = uint2(pc.x * pc.y * (interior + 2), pc.z * (interior + 2));
    uint2 distAtlas = uint2(pc.x * pc.y * (16u + 2u), pc.z * (16u + 2u));

    // Surface bias (RTXGI / DDGI-2021): nudge the query point off the shaded surface along a blend of
    // the normal and the view vector before the lookup, so the trilinear + Chebyshev sample is taken
    // away from the high-variance self-visibility boundary. Sampling the raw surface point sits it on
    // that boundary and the interpolation shows ~1-cell sliding lobes ("blobs") on flat floors.
    float3 spacing = vol.volumeExtent.xyz / float3(pc);
    float minSpacing = min(spacing.x, min(spacing.y, spacing.z));
    float3 viewDir = normalize(vol.eyePosition.xyz - worldPos);
    float3 biasedPos = worldPos + (n * 0.2 + viewDir * 0.8) * (0.75 * minSpacing) * 0.3;

    float3 t = (biasedPos - vol.volumeMin.xyz) / vol.volumeExtent.xyz;  // [0,1] in volume
    // Probes are cell-centered: probe i sits at (i + 0.5)/count of the volume, so the probe-space
    // coordinate is t·count - 0.5 (a worldPos at probe i's centre lands on integer i, frac 0). The
    // corner-aligned t·(count-1) would bleed weight off-centre and mis-feed the Chebyshev test.
    float3 grid = t * float3(pc) - 0.5;                 // probe-space coordinate
    int3 base = int3(floor(grid));
    float3 frac = grid - float3(base);

    float3 sum = float3(0.0);
    float wsum = 0.0;     // full weight mass — drives the irradiance average
    float covsum = 0.0;   // trilinear-only mass — drives DDGI-over-IBL coverage
    for (uint c = 0; c < 8; c = c + 1)
    {
        int3 off = int3(c & 1u, (c >> 1) & 1u, (c >> 2) & 1u);
        int3 corner = base + off;
        bool inCage = all(corner >= int3(0)) && all(corner < int3(pc));
        int3 pi = clamp(corner, int3(0), int3(pc) - 1);
        float3 probePos = vol.volumeMin.xyz + (float3(pi) + 0.5) / float3(pc) * vol.volumeExtent.xyz;
        // Toroidal scroll fold: the logical probe `pi` is stored at the physical atlas tile
        // wrapMod(pi + scrollBase, count) — the camera-centered clipmap keeps a staying probe's
        // tile + history as the volume recenters.
        uint3 physTile = uint3((pi + int3(vol.scrollBase.xyz)) % int3(pc));

        // Trilinear weight.
        float3 tw = lerp(1.0 - frac, frac, float3(off));
        float trilinear = tw.x * tw.y * tw.z;
        if (inCage) { covsum += trilinear; }  // only in-cage corners count toward replacement
        float weight = trilinear;

        // Soft backface: prefer probes the surface normal faces. Canonical RTXGI form — squared
        // wrap-cosine with an additive 0.2, not a linear term with a multiplicative floor; the
        // linear+floor form lets one corner dominate a cell and slide a soft lobe across it.
        float3 dirToProbe = probePos - biasedPos;
        float distToProbe = length(dirToProbe);
        dirToProbe = distToProbe > 1e-4 ? dirToProbe / distToProbe : n;
        float wrap = dot(dirToProbe, n) * 0.5 + 0.5;
        weight *= wrap * wrap + 0.2;

        // Chebyshev visibility: compare distToProbe against the probe's stored moments in
        // the surface->probe direction (kills leaking through occluders).
        float2 distUv = ddgiAtlasUv(pc.x, physTile, -dirToProbe, 16u, distAtlas);
        float2 moments = distanceAtlas.SampleLevel(distUv, 0.0).rg;
        float meanDist = moments.x;
        if (distToProbe > meanDist)
        {
            float variance = abs(moments.x * moments.x - moments.y);
            float d = distToProbe - meanDist;
            float cheb = variance / (variance + d * d);
            cheb = max(cheb * cheb * cheb, 0.0);  // sharpen
            weight *= cheb;
        }

        // Low-weight crush (RTXGI): square weights below a small threshold so a barely-contributing
        // corner cannot smear a soft lobe across the cell — sharpens the inter-cell transition.
        const float crush = 0.2;
        if (weight < crush) { weight *= (weight * weight) / (crush * crush); }

        float2 irrUv = ddgiAtlasUv(pc.x, physTile, n, interior, irrAtlas);
        sum += irradianceAtlas.SampleLevel(irrUv, 0.0).rgb * weight;
        wsum += weight;
    }
    float3 irr = wsum > 1e-4 ? sum / wsum : float3(0.0);
    return float4(irr, saturate(covsum));
}
