// Point (nearest) upscale of the input-extent scene depth into the display-extent overlay depth.
// Under temporal upsampling the scene (and its depth) render at input extent while the ground grid
// and editor gizmo composite on the display-extent resolved color; a graphics pass depth-tests
// against a display-extent depth, so the input depth is resampled up here. NEAREST (not bilinear):
// interpolating depth across a silhouette would fabricate an in-between surface and mis-occlude the
// overlay. A fullscreen triangle (no vertex buffer) writes the sampled depth as SV_Depth.
// Set 0: 0 = input scene depth sampler.

struct DepthUpscalePush
{
    float2 inputSize;   // input depth extent in texels (for the nearest-texel-centre fetch)
};
[[vk::push_constant]] DepthUpscalePush push;

[[vk::binding(0, 0)]] Sampler2D inputDepth;

struct VertexOutput
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

[shader("vertex")]
VertexOutput vertexMain(uint vertexId : SV_VertexID)
{
    float2 uv = float2((vertexId << 1) & 2, vertexId & 2);
    VertexOutput out;
    out.uv = uv;
    out.position = float4(uv * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

struct FragmentOutput
{
    float depth : SV_Depth;
};

[shader("fragment")]
FragmentOutput fragmentMain(VertexOutput input)
{
    // Snap to the nearest input texel centre so the linear sampler returns that texel exactly
    // (a point read without a dedicated nearest sampler).
    float2 texel = floor(input.uv * push.inputSize);
    float2 sampleUv = (texel + 0.5) / push.inputSize;
    FragmentOutput out;
    out.depth = inputDepth.SampleLevel(sampleUv, 0.0).r;
    return out;
}
