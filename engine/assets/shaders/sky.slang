// Visible sky background. A fullscreen triangle (no vertex buffer); the fragment
// reconstructs the world-space view ray from the inverse view-projection and shades it by
// mode. Output is linear HDR into the scene color target — the tonemap pass handles display.
// Camera translation cancels in (far - near), so the sky is naturally camera-locked.
//
// Modes (push.params.z): 0 = flat clear color, 1 = equirectangular panorama (a bindless 2D
// texture, set 0, indexed by push.params.w), 2 = the procedural IBL environment cube (set 1),
// 3 = a fixed neutral studio gradient independent of the environment (the offscreen thumbnail
// backdrop — decouples the visible backdrop from the IBL so a subject always reads as a silhouette,
// e.g. a chrome ball reflecting a dark HDRI on a light gradient).

static const float PI = 3.14159265;

// Set 0: the same bindless albedo array the mesh samples (any uploaded texture, incl. a
// loaded sky panorama). Size matches MaxBindlessTextures in renderer.cppm.
[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];
// Set 1: the procedural environment cube the IBL bake produced (shared with the lighting).
[[vk::binding(0, 1)]] SamplerCube envCube;
[[vk::binding(1, 1)]] SamplerCube milkyWayCube;
[[vk::binding(2, 1)]] Sampler2D<float4> transmittanceLut;
[[vk::binding(3, 1)]] Sampler2D<float4> skyViewLut;

struct SkyPush
{
    float4x4 invViewProj;  // clip -> world (this frame's camera, inverted)
    float4 params;         // x = intensity, y = rotation (yaw radians), z = mode, w = texture index
    float4 clearColor;     // rgb = tinted Color-mode fill, or the sky tint in other scene modes
    float4 worldFromEquatorial;
    float4 night;          // x Milky Way intensity, y atmosphere height, z atmosphere live
};
[[vk::push_constant]] SkyPush push;

struct VertexOutput
{
    float4 position : SV_Position;
    float2 ndc : TEXCOORD0;
};

float3 rotateByQuaternion(float3 value, float4 rotation)
{
    return value + 2.0 * cross(rotation.xyz, cross(rotation.xyz, value) + rotation.w * value);
}

[shader("vertex")]
VertexOutput vertexMain(uint vertexId : SV_VertexID)
{
    // Fullscreen triangle: (0,0) (2,0) (0,2) in UV -> (-1,-1) (3,-1) (-1,3) in clip.
    float2 uv = float2((vertexId << 1) & 2, vertexId & 2);
    VertexOutput output;
    output.ndc = uv * 2.0 - 1.0;
    output.position = float4(output.ndc, 0.0, 1.0);
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    // World ray direction: unproject the near + far points; their difference is translation-
    // independent, so the sky stays fixed as the camera moves.
    float4 nearW = mul(push.invViewProj, float4(input.ndc, 0.0, 1.0));
    float4 farW = mul(push.invViewProj, float4(input.ndc, 1.0, 1.0));
    float3 dir = normalize(farW.xyz / farW.w - nearW.xyz / nearW.w);

    // Yaw the lookup around the world up axis.
    float a = push.params.y;
    float s = sin(a);
    float c = cos(a);
    dir = float3(c * dir.x + s * dir.z, dir.y, -s * dir.x + c * dir.z);

    float intensity = push.params.x;
    uint mode = (uint)push.params.z;
    float3 color;
    if (mode == 0u)
    {
        color = push.clearColor.rgb;
    }
    else if (mode == 1u)
    {
        float2 uv = float2(atan2(dir.z, dir.x) * (0.5 / PI) + 0.5,
                           acos(clamp(dir.y, -1.0, 1.0)) / PI);
        uint index = (uint)push.params.w;
        color = albedoTextures[NonUniformResourceIndex(index)].SampleLevel(uv, 0.0).rgb;
    }
    else if (mode == 2u)
    {
        color = envCube.SampleLevel(dir, 0.0).rgb;
        if (push.night.x > 0.0)
        {
            float4 inverseRotation = float4(-push.worldFromEquatorial.xyz,
                                             push.worldFromEquatorial.w);
            float3 equatorialDir = normalize(rotateByQuaternion(dir, inverseRotation));
            float3 milkyWay = milkyWayCube.SampleLevel(equatorialDir, 0.0).rgb * push.night.x;
            if (push.night.z > 0.5)
            {
                float2 transmittanceUv = float2(dir.y * 0.5 + 0.5,
                    saturate(0.5 / max(push.night.y, 1.0)));
                milkyWay *= transmittanceLut.SampleLevel(transmittanceUv, 0.0).rgb;
            }
            color += dir.y > 0.0 ? milkyWay : float3(0.0, 0.0, 0.0);
        }
    }
    else
    {
        // Fixed neutral studio gradient (linear HDR): a light upper hemisphere fading to a darker
        // floor, keyed only on the view ray's elevation. Independent of the environment, so the
        // subject (a lit sphere, or a chrome ball reflecting a dark HDRI) always has contrast at its
        // silhouette.
        float t = smoothstep(0.0, 1.0, saturate(dir.y * 0.5 + 0.5));
        color = lerp(float3(0.16, 0.16, 0.18), float3(0.48, 0.50, 0.55), t);
    }
    if (mode == 1u || mode == 2u)
    {
        color *= push.clearColor.rgb;
    }
    return float4(color * intensity, 1.0);
}
