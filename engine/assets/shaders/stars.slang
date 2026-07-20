// Instanced BSC5 point-spread stars. Each instance expands to a fixed screen-space Gaussian quad;
// apparent magnitude controls HDR radiance, never sprite size. Radiance is attenuated through the
// atmosphere transmittance LUT before additive composition with the sky's in-scatter.

struct StarGpu
{
    float4 directionLuminance;
    float4 color;
};

[[vk::binding(0, 0)]] StructuredBuffer<StarGpu> stars;
[[vk::binding(1, 0)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(2, 0)]] Sampler2D<float4> skyViewLut;

struct StarPush
{
    float4x4 viewProj;
    float4 worldFromEquatorial;
    float4 params; // xy viewport, z radiance scale, w atmosphere height (0 = neutral extinction)
};
[[vk::push_constant]] StarPush push;

float3 rotateByQuaternion(float3 value, float4 rotation)
{
    return value + 2.0 * cross(rotation.xyz, cross(rotation.xyz, value) + rotation.w * value);
}

struct VertexOutput
{
    float4 position : SV_Position;
    float2 psf : TEXCOORD0;
    float3 direction : TEXCOORD1;
    float luminance : TEXCOORD2;
    float3 color : TEXCOORD3;
};

[shader("vertex")]
VertexOutput vertexMain(uint vertexId : SV_VertexID, uint instanceId : SV_InstanceID)
{
    static const float2 corners[6] = {
        float2(-1.0, -1.0), float2(1.0, -1.0), float2(1.0, 1.0),
        float2(-1.0, -1.0), float2(1.0, 1.0), float2(-1.0, 1.0)
    };
    StarGpu star = stars[instanceId];
    float3 direction = normalize(rotateByQuaternion(
        star.directionLuminance.xyz, push.worldFromEquatorial));
    float4 center = mul(push.viewProj, float4(direction, 0.0));
    float2 ndcOffset = corners[vertexId] * 2.2 * 2.0 / max(push.params.xy, 1.0);

    VertexOutput output;
    output.position = center;
    output.position.xy += ndcOffset * center.w;
    output.psf = corners[vertexId];
    output.direction = direction;
    output.luminance = star.directionLuminance.w;
    output.color = star.color.rgb;
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    if (input.direction.y <= 0.0 || push.params.z <= 0.0)
    {
        discard;
    }
    float radius2 = dot(input.psf, input.psf);
    if (radius2 > 1.0)
    {
        discard;
    }
    float psf = exp(-5.0 * radius2);
    float3 transmittance = float3(1.0, 1.0, 1.0);
    if (push.params.w > 0.0)
    {
        float2 uv = float2(input.direction.y * 0.5 + 0.5, saturate(0.5 / push.params.w));
        transmittance = transmittanceLut.SampleLevel(uv, 0.0).rgb;
    }
    float3 radiance = input.color * input.luminance * push.params.z * psf * transmittance;
    return float4(radiance, 0.0);
}
