module octahedral;

// Octahedral encode/decode of a unit vector into two [-1,1] floats. The SDF-AO map carries a
// bent normal in two channels (ao + viewZ take the other two) so the rgba16f temporal
// accumulator can blend it; the bent normal sits in the surface's upper hemisphere, away from
// the octahedral seam, so the wrap distortion is negligible. Kept in its own resource-free
// module so the SDF cone-trace and the SDF-AO accumulator share one encode without the
// accumulator pulling in the `sdf` module's bindless field bindings.

public float2 octEncode(float3 n)
{
    n /= (abs(n.x) + abs(n.y) + abs(n.z));
    float2 e = n.xy;
    if (n.z < 0.0)
    {
        e = (1.0 - abs(e.yx)) * float2(e.x >= 0.0 ? 1.0 : -1.0, e.y >= 0.0 ? 1.0 : -1.0);
    }
    return e;
}

public float3 octDecode(float2 e)
{
    float3 n = float3(e.xy, 1.0 - abs(e.x) - abs(e.y));
    float t = saturate(-n.z);
    n.x += n.x >= 0.0 ? -t : t;
    n.y += n.y >= 0.0 ? -t : t;
    return normalize(n);
}
