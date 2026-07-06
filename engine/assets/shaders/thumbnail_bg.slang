// The thumbnail backdrop: a subtle vertical gradient (lighter at top) drawn as a fullscreen triangle
// behind every 3D-render tile (model / material / texture sphere / HDRI ball), so the tiles share one
// neutral studio backdrop instead of four different near-black clears. The pipeline disables depth
// test + write, so this paints over the cleared color without touching the depth buffer the object
// draw relies on. Colors are display-space (written straight to the UNORM target, no tonemap) so they
// read as authored; the object pixels are tonemap-encoded into the same space.

struct VertexOutput
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

[shader("vertex")]
VertexOutput vertexMain(uint vertexId : SV_VertexID)
{
    // Fullscreen triangle: (0,0) (2,0) (0,2) in UV -> (-1,-1) (3,-1) (-1,3) in clip.
    float2 uv = float2((vertexId << 1) & 2, vertexId & 2);
    VertexOutput output;
    output.uv = uv;
    output.position = float4(uv * 2.0 - 1.0, 0.0, 1.0);
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target
{
    // uv.y is 0 at the top row of the tile and 1 at the bottom (framebuffer NDC maps -1 -> top row).
    float t = saturate(input.uv.y);
    float3 top = float3(0.235, 0.238, 0.255);     // lighter, subtly cool
    float3 bottom = float3(0.110, 0.110, 0.125);  // darker floor
    return float4(lerp(top, bottom, t), 1.0);
}
