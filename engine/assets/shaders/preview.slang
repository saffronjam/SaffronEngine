// Material / texture preview shader: renders a sphere with a candidate material under fixed studio
// lighting, for the editor's cached asset thumbnails (the interactive 3D preview tab uses the real
// scene pass, not this shader). Self-contained — only the bindless texture array (set 0, shared with
// the übershader) + a 128-byte push constant; no scene / light / IBL / GI sets. A controlled studio
// look (a key directional light + hemisphere ambient), matching how DCC tools preview materials.
// The view direction is a constant that matches the camera framing in renderMaterialPreview.

struct PreviewPush
{
    float4x4 viewProj;
    float4 baseColor;
    uint4 tex;   // x = albedo, y = metallic-roughness, z = normal, w = occlusion (bindless indices)
    uint4 tex2;  // x = emissive, y = height, z = feature bits, w = 0
    float4 pbr;  // x = metallic, y = roughness, z = normalStrength, w = emissiveStrength
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
    float2 uv : TEXCOORD0;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input)
{
    VertexOutput output;
    output.position = mul(push.viewProj, float4(input.position, 1.0));
    output.worldPos = input.position;
    output.normal = input.normal;
    output.uv = input.uv0;
    return output;
}

static const uint FEATURE_NORMAL = 1u;
static const uint FEATURE_OCCLUSION = 2u;
static const uint FEATURE_EMISSIVE = 4u;
static const uint FEATURE_HEIGHT = 8u;
static const uint FEATURE_ALPHA = 16u;
static const float PI = 3.14159265;

// A tangent-space basis for the sphere at this fragment, derived from screen-space derivatives —
// shared by normal mapping and height-to-bump so both perturb in the same frame.
float3x3 tangentBasis(float3 worldPos, float3 n, float2 uv)
{
    float3 dp1 = ddx(worldPos);
    float3 dp2 = ddy(worldPos);
    float2 du1 = ddx(uv);
    float2 du2 = ddy(uv);
    float3 dp2p = cross(dp2, n);
    float3 dp1p = cross(n, dp1);
    float3 T = dp2p * du1.x + dp1p * du2.x;
    float3 B = dp2p * du1.y + dp1p * du2.y;
    float inv = rsqrt(max(dot(T, T), max(dot(B, B), 1e-8)));
    return float3x3(T * inv, B * inv, n);
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    uint features = push.tex2.z;
    float2 uv = input.uv;

    float4 base = textures[NonUniformResourceIndex(push.tex.x)].Sample(uv);

    // Opacity: the albedo slot carries the mask; below the cutoff the fragment reads as a checker
    // cutout (one pass, no discard) so the tile shows the mask shape over a backdrop.
    if ((features & FEATURE_ALPHA) != 0u)
    {
        float mask = base.a < 1.0 ? base.a : dot(base.rgb, float3(0.299, 0.587, 0.114));
        if (mask < 0.5)
        {
            float2 cell = floor(input.position.xy / 8.0);
            float chk = frac((cell.x + cell.y) * 0.5) * 2.0;
            return float4(lerp(float3(0.16, 0.16, 0.18), float3(0.42, 0.42, 0.46), chk), 1.0);
        }
        // Opaque region: shade a neutral grey surface, ignoring the mask's own greyscale.
        base = float4(0.72, 0.72, 0.72, 1.0);
    }

    float3 albedo = base.rgb * push.baseColor.rgb;
    float4 mr = textures[NonUniformResourceIndex(push.tex.y)].Sample(uv);
    float metallic = saturate(push.pbr.x * mr.b);
    float roughness = clamp(push.pbr.y * mr.g, 0.045, 1.0);

    float ao = 1.0;
    if ((features & FEATURE_OCCLUSION) != 0u)
    {
        ao = textures[NonUniformResourceIndex(push.tex.w)].Sample(uv).r;
    }

    float3 n = normalize(input.normal);
    if ((features & FEATURE_NORMAL) != 0u)
    {
        float3x3 tbn = tangentBasis(input.worldPos, n, uv);
        float3 mapN = textures[NonUniformResourceIndex(push.tex.z)].Sample(uv).xyz * 2.0 - 1.0;
        mapN.xy *= push.pbr.z;
        n = normalize(mul(mapN, tbn));
    }
    else if ((features & FEATURE_HEIGHT) != 0u)
    {
        // Height → bump: perturb the normal by the height gradient so a greyscale displacement
        // map reads as shaded relief on the tile (the interactive tab gets real VS-displacement).
        float texel = 1.0 / 512.0;
        float hL = textures[NonUniformResourceIndex(push.tex2.y)].Sample(uv - float2(texel, 0.0)).r;
        float hR = textures[NonUniformResourceIndex(push.tex2.y)].Sample(uv + float2(texel, 0.0)).r;
        float hD = textures[NonUniformResourceIndex(push.tex2.y)].Sample(uv - float2(0.0, texel)).r;
        float hU = textures[NonUniformResourceIndex(push.tex2.y)].Sample(uv + float2(0.0, texel)).r;
        float3x3 tbn = tangentBasis(input.worldPos, n, uv);
        float3 bumpN = normalize(float3((hL - hR) * 8.0, (hD - hU) * 8.0, 1.0));
        n = normalize(mul(bumpN, tbn));
    }

    // Fixed studio lighting: a key directional light + hemisphere ambient. V matches the camera dir.
    float3 V = normalize(float3(0.3, 0.4, 1.0));
    float3 L = normalize(float3(0.5, 0.6, 0.6));
    float3 H = normalize(L + V);
    float ndotl = max(dot(n, L), 0.0);
    float ndotv = max(dot(n, V), 1e-3);
    float ndoth = max(dot(n, H), 0.0);
    float hdotv = max(dot(H, V), 0.0);

    float3 F0 = lerp(float3(0.04), albedo, metallic);
    float a = roughness * roughness;
    float a2 = a * a;
    float d = ndoth * ndoth * (a2 - 1.0) + 1.0;
    float D = a2 / max(PI * d * d, 1e-5);
    float k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    float G = (ndotv / (ndotv * (1.0 - k) + k)) * (ndotl / (ndotl * (1.0 - k) + k));
    float3 F = F0 + (1.0 - F0) * pow(1.0 - hdotv, 5.0);
    float3 spec = D * G * F / max(4.0 * ndotv * ndotl, 1e-4);
    float3 kd = (1.0 - F) * (1.0 - metallic);
    float3 lit = (kd * albedo / PI + spec) * ndotl * 2.5;

    float hemi = 0.5 + 0.5 * n.y;
    float3 ambient = albedo * (1.0 - metallic) * lerp(float3(0.08, 0.08, 0.10), float3(0.30, 0.32, 0.38), hemi);

    float3 color = (lit + ambient) * ao;

    if ((features & FEATURE_EMISSIVE) != 0u)
    {
        float3 emis = textures[NonUniformResourceIndex(push.tex2.x)].Sample(uv).rgb;
        color += emis * push.pbr.w;
    }

    color = color / (color + 1.0);  // Reinhard tonemap into [0,1]
    return float4(color, 1.0);
}
