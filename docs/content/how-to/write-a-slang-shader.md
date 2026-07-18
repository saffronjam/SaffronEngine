+++
title = 'Write a Slang shader'
weight = 6
math = false
+++

# Write a Slang shader

Add a [Slang](https://shader-slang.org/) vertex/fragment module and compile it into the runtime shader directory.

## Prerequisites

- Run commands from the repository root.
- Complete [Build and run](../build-and-run/) so the `saffron-build` toolbox or macOS Slang toolchain is available.
- Use the debug Cargo profile for this procedure.

## Steps

1. Create `engine/assets/shaders/diagnostic_triangle.slang` with two tagged entry points:

   ```hlsl
   struct VertexOutput
   {
       float4 position : SV_Position;
       float3 color : COLOR0;
   };

   [shader("vertex")]
   VertexOutput vertexMain(uint vertexId : SV_VertexID)
   {
       float2 positions[3] = {
           float2(0.0, -0.5),
           float2(0.5, 0.5),
           float2(-0.5, 0.5),
       };
       float3 colors[3] = {
           float3(1.0, 0.0, 0.0),
           float3(0.0, 1.0, 0.0),
           float3(0.0, 0.0, 1.0),
       };

       VertexOutput output;
       output.position = float4(positions[vertexId], 0.0, 1.0);
       output.color = colors[vertexId];
       return output;
   }

   [shader("fragment")]
   float4 fragmentMain(VertexOutput input) : SV_Target
   {
       return float4(input.color, 1.0);
   }
   ```

2. Run the shader pipeline:

   ```sh
   just shaders
   # xtask shaders: using slangc <path>
   # xtask shaders: <compiled> compiled, <cached> up to date, ... -> .../target/debug/shaders
   ```

   The command scans `engine/assets/shaders/*.slang`, preserves the entry-point names, and writes one SPIR-V module per entry-point source file.

3. Verify that the compiler staged both the binary and the source copy:

   ```sh
   test -s engine/target/debug/shaders/diagnostic_triangle.spv \
     && cmp -s \
       engine/assets/shaders/diagnostic_triangle.slang \
       engine/target/debug/shaders/diagnostic_triangle.slang \
     && echo 'shader outputs OK'
   # shader outputs OK
   ```

4. Run the pipeline again without editing the source:

   ```sh
   just shaders
   # xtask shaders: 0 compiled, <cached> up to date, lighting module up to date -> .../target/debug/shaders
   ```

   A nonzero compile count on the second run means a source or shared-module dependency changed between runs.

5. Run the complete engine build to confirm the same module survives the normal build path:

   ```sh
   just engine
   test -s engine/target/debug/shaders/diagnostic_triangle.spv \
     && echo 'engine shader stage OK'
   # engine shader stage OK
   ```

## Verify

The task is complete when both output checks print `OK` and the immediate no-change `just shaders` run reports `0 compiled`.

Compiling a module does not schedule it for rendering. A render pass must create a compatible pipeline, bind every declared resource, and name `vertexMain` and `fragmentMain` as its entry points. Follow the [render graph API](../../reference/render-graph-api/) and [material and PSO selection](../../explanations/materials-and-pipelines/material-and-pso-selection/) before connecting a custom module to a frame.

## Related

- [Shader compilation](../../explanations/architecture-and-conventions/shader-compilation/) — flags, shared modules, staleness, and profile directories
- [Shader descriptor sets](../../reference/shader-descriptor-sets/) — binding layouts used by engine pipelines
- [Ubershader and specialization](../../explanations/materials-and-pipelines/ubershader-and-specialization/) — the mesh shader entry-point contract
