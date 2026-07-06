// HDRI chrome-ball thumbnail: a static mirror sphere reflecting an equirectangular HDRI, for the
// editor's cached environment tiles. Self-contained — only the bindless texture array (set 0, shared
// with the übershader) + the 128-byte PreviewPush; no IBL prefilter. A direct per-fragment mirror
// reflection of the raw equirect (the interactive 3D tab renders lit balls in the prefiltered
// environment; this is the cheap static tile). The equirect mapping matches sky.slang so a tile
// reflects the environment at the same orientation the scene backdrop shows.

import tonemap_ops;

struct PreviewPush
{
    float4x4 viewProj;
    float4 baseColor;  // xyz = camera world position (the per-fragment view ray origin)
    uint4 tex;         // x = HDRI equirect (bindless index)
    uint4 tex2;        // unused here
    float4 pbr;        // unused here
};
[[vk::push_constant]] PreviewPush push;

[[vk::binding(0, 0)]] Sampler2D textures[1024];

struct VertexInput
{
    [[vk::location(0)]] float3 position;
    [[vk::location(1)]] float3 normal;
    [[vk::location(2)]] float2 uv0;
};

struct VertexOutput
{
    float4 position : SV_Position;
    float3 worldPos : POSITION0;
    float3 normal : NORMAL;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input)
{
    VertexOutput output;
    output.position = mul(push.viewProj, float4(input.position, 1.0));
    // The preview sphere is a unit sphere at the origin with no model transform, so object space
    // is world space — the reflection is computed directly against these positions/normals.
    output.worldPos = input.position;
    output.normal = input.normal;
    return output;
}

static const float PI = 3.14159265;

// Reflection direction → equirectangular UV, identical to sky.slang's backdrop mapping.
float2 equirectUv(float3 dir)
{
    return float2(atan2(dir.z, dir.x) * (0.5 / PI) + 0.5,
                  acos(clamp(dir.y, -1.0, 1.0)) / PI);
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    float3 n = normalize(input.normal);
    float3 view = normalize(input.worldPos - push.baseColor.xyz);  // camera → surface
    float3 r = reflect(view, n);
    float3 env = textures[NonUniformResourceIndex(push.tex.x)].SampleLevel(equirectUv(r), 0.0).rgb;

    // A faint Fresnel rim lifts the silhouette so the mirror ball reads as a sphere against the
    // dark tile, not a flat disc.
    float fresnel = pow(1.0 - saturate(dot(-view, n)), 5.0);
    float3 color = env + fresnel * 0.15;

    // PBR Neutral + sRGB encode, shared with the material-preview tile + the viewport tonemap.
    color = tonemapAndEncode(color, 3u);
    return float4(color, 1.0);
}
