+++
title = 'Wind field'
weight = 13
math = true
+++

# Wind field

One deterministic wind field serves every consumer — clouds, fog advection, vegetation
deformation, and physics queries all sample the same velocity for the same position and
time. The field is a pure function: no retained simulation state, no per-consumer
re-implementation, no divergence between what the sky does and what a branch feels.

## How it works

`SceneEnvironment::wind` is the one authored global source. Beyond the mean flow
(orientation, speed, gust fraction) it carries the deterministic sampling parameters:
turbulence octave count and per-octave roughness, gust-front frequency, the reference
height at which the authored speed holds, a power-law shear exponent for height
response, and a phase seed.

`saffron-wind` evaluates the field. A sample composes three terms:

- **Mean advection** along the orientation, scaled by the shear profile
  $\left(\frac{h}{h_{ref}}\right)^{k}$ so canopies see more wind than ground cover.
- **Gust fronts** — a traveling envelope along the mean direction; the squared-sine
  front factor briefly raises the mean speed and is exposed on the sample for
  consumers that react to fronts rather than raw velocity.
- **Multiscale turbulence** — a fixed-phase sum of sinusoid gradients advected with
  the mean flow; each octave halves the wavelength and decays by the roughness
  ratio, and the phases derive from the seed alone.

```rust
let profile = WindProfile { speed: 12.0, turbulence_octaves: 3, ..Default::default() };
let sample = saffron_wind::sample(&profile, DVec3::new(4.0, 7.5, -20.0), time_s);
// sample.velocity: world m/s     sample.gust_front: 0..1 front envelope
```

Determinism is the contract: equal `(profile, position, time)` inputs sample equal
velocities on every thread and every run, so the GPU mirror of the same math stays in
agreement with CPU consumers. Time is the monotonic simulation clock; the calendar
drives seasonal signals, never wind integration.

The GPU mirror is the `wind.slang` module: `sampleWindVelocity` reproduces `sample`
term for term from explicit parameter words, so any pass carrying the words samples
the identical field. The renderer folds `SceneEnvironment::wind` and the simulation
clock into per-frame words through `Renderer::set_wind`; the light UBO carries them
for lighting-side consumers, and the wind deformation prepass receives them as push
constants.

The prepass samples the field once per wind-flagged scene instance, at the current
and the previous frame's time, and stores a sway record that every raster pass
applies — so motion vectors carry the exact wind term, and no other shader
evaluates the field. The
[persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/) page
describes the record and its consumers.

The record also carries one branch mode as stored quadrature: the prepass evaluates
the mode's sine and cosine at both frame times, and a vertex applies a stable
per-use phase offset with two multiplies — time never reaches the vertex path.
Assembly uses tagged with a moving structural semantic (branch, frond, leaf,
flower, fruit) oscillate about their pivot along the wind, and leaf-family parts
add a double-frequency cross-wind flutter through the quadrature identity. Taller
plants swing slower.

The record carries each deformation term's magnitude beside their total, because the
[visibility cull](../../frame-and-render-graph/hierarchical-visibility/) widens a box by
what *that* box moves. A cluster reads its own greatest instance-local height, derives the
same weight the vertex path applies, and takes sway at the weight squared, interaction at
the weight, and the branch and flutter amplitudes only where the use's semantic earns
them. Only the instance sphere, which has no local box to weigh, takes the whole-instance
total.

The world interaction field composes with wind at the same application point. Two
camera-centred cascades of damped-oscillator texels store a horizontal push and a
ground depression; moving physics bodies and characters emit impulses into them
each play step, `emit-interaction-impulse` pushes them from tooling, and every
texel springs back to rest over about a second. The prepass samples the field at
each instance root and micro blades fold it into their baked bend, so trampled
vegetation leans from the root while wind keeps swaying the tips.

Because the cascades follow the camera, texels scroll in at rest. Texels are keyed by
absolute world coordinate, so a standing plant keeps its texel and reads a continuous
value as the window slides past — the jump comes only when a plant changes which cascade
covers it, since the coarser cascade holds separate state. The prepass asks which cascade
covers the instance under this frame's centres and under the previous frame's, and marks
the instance reactive when the answers differ, so the reactive-coverage pass biases those
pixels toward the current frame instead of letting TAA reproject a value with no history
behind it. Motion within a cascade is not a reset and is not marked. `gpu-scene-stats`
reports `visibility.interactionResets` as a running total since boot: a reset lasts one
frame, so a per-frame count would read zero on nearly every sample.

Placeable `WindSource` entities composite over the global field: directional, point,
vortex, wake, and volume influences with a radius and edge falloff. A volume source
scales the global term (zero strength shelters its interior); the others add their own
velocities. `sample_composed` folds them in on the CPU, `sample-wind` exposes the
composed field to tooling, and the frame's sources upload to a small ring so
`sampleComposedWindVelocity` composes the identical field in every GPU sampler — the
deformation prepass, the blade scatter, and the fog march.

Physics reads the same seam. The play world binds the profile, the sources, and the
simulation clock each step, `World::sample_wind` answers a query onto them, and an awake
dynamic body takes the quadratic drag of the air it is moving through — so a crate and the
grass beside it are pushed by one field rather than two models. How much of that drag a
body feels is its [`windFactor`](../../physics/rigidbody-and-collider/), which scales the
cross-section its collider describes and defaults to zero: a collision proxy is not an
aerodynamic profile, and the field evaluates `sin`/`cos`, so a coupled body trades the
bit-exact cross-target trajectory an uncoupled one keeps for same-binary reproducibility.

## Reading the field apart

A velocity is a sum, and a sum hides which term produced it. `sample-wind` therefore
reports the mean advection, the turbulence, and the spectrum behind it — one entry per
evaluated octave with that octave's wavelength and its own contribution — beside every
local source's distance, edge weight, and what it added or scaled. A canopy moving
strangely is then a question with an answer: too much energy in the coarsest octave, or a
vortex source nobody remembered placing.

The decomposition is the sample, not a second model of it: `sample` sums exactly these
terms, in this order, and a unit test compares the two bit for bit. Summing the octaves
before normalizing rather than scaling each first is deliberate — the two agree in exact
arithmetic and differ by a rounding step in floating point, and the shader mirror sums
first.

`wind-interaction-field` captures a whole cascade of the interaction field rather than one
plant's sample: the cascade's placement, how many texels are holding state, the peak
displacement and recovery velocity, and a reduced grid of block means. The reduction runs
beside the readback because a cascade is a quarter of a million texels and a grid is what a
person can read.

```sh
sa set-wind --speed 14 --json '{"turbulenceOctaves": 4, "gustFrequency": 0.3}'
sa get-environment -o json | jq .wind
sa -o json sample-wind --positionM '[12, 3, 40]' | jq '.octaves, .sources'
sa emit-interaction-impulse --positionM '[12, 40]' --radiusM 2 --strength 4
sa -o json wind-interaction-field --cascade 0 --resolution 16 | jq '.peakDisplacementM'
```

## Authored plant response

The field says what the air is doing; the plant family says how it answers. `.splant` authors
stiffness, damping, drag, flutter, and a bend limit, and the deformation prepass applies them:
stiffness raises the branch mode's frequency as its square root and divides the amplitude, drag
scales how hard the field pushes, flutter scales the leaf term, damping bleeds amplitude, and the
bend limit caps the result. A stiff sapling and a supple reed of the same height therefore move
differently, which without the response they could not.

The values travel as cooked integers the whole way — the part table, the family render load, the
prototype record's four words — so the GPU reads exactly what the cooker wrote rather than a float
rounded twice. A prototype that is not a plant family carries zeros, and the prepass reads that as
the height-derived model, which is what every ordinary mesh gets.

A zero bend limit means unlimited. A plant that may not bend at all is a prop rather than a bend
limit, and reading zero literally would freeze every family that left the field alone.

The bend limit also fixes how far each part sweeps at cook time. A part's deformation region
carries that part's own geometry bounds and widens them by its own share of the family bend —
the squared height weight again, so a part topping out at a quarter of the family height sweeps a
sixteenth as far. Giving every part the family box would make a trunk and the leaves it carries
declare the same extent, and nothing reading those regions could then tell one from the other. The
arithmetic is integer Q15.16 throughout, because a cooked bound has to be the same byte on every
target.

`vegetation-wind-record` captures one plant's prepass record: the sway at both frame times, the
interaction displacement at both, the branch quadrature and amplitudes, the height scale, the
bounds slack, and the response behind them. It is explicit and one-shot — the buffer is device-local
and reading it idles the queue — and it is the only view of what the prepass actually computed,
since every raster pass applies these stored words rather than re-deriving wind.

```sh
sa -o json vegetation-wind-record --cell '{"coordinates":["0","0","0"],"level":0}' --plant <id>
```

## Proving the field reaches pixels

Only instances flagged for wind are displaced, and the mirror sets that flag on vegetation points
alone — a cube in a gale is motionless by design, not by defect. That makes wind easy to assert
structurally and easy to get wrong invisibly: a path that computes a correct sway record and never
displaces a vertex satisfies every non-visual check.

The e2e suite closes that gap by measuring **motion over time** rather than calm against gale.
Comparing one calm frame to one gale frame conflates displacement with everything else the two
states differ in. Instead each state is sampled twice across the same interval and compared against
itself: a still field must produce consecutive frames that agree, and a gale must not.

The still pair doubles as the control. It fails if the image is unstable for any reason — temporal
accumulation that never converges, an animation left running, a nondeterministic pass.

What separates the two states is the size of the step one channel takes, not how much of the frame
drifted. A resident micro field is matter in the global distance field, and the occlusion marches
that read it rotate their sample set per frame, so the canopy's shading keeps stepping by one 8-bit
level for as long as the frame runs — with the field at zero and the prepass recording zero sway.
Pooled into a frame mean that residue reads as motion, and reads as more of it than a gale window
that samples the sway near a phase return. Per channel the two states never meet: a still window
peaks at one level and puts no channel past eight, while a gale window peaks around eighty and puts
thousands there. Returning the field to calm must restore stillness, which is what catches a
deformation that latched at its last displacement instead of tracking the field.

> [!NOTE]
> A visual wind test must stay in edit mode. Play renders the scene's primary camera, so the editor
> camera the test positions is ignored and every frame becomes the same picture of nothing.

## In the code

| What | File | Symbols |
|---|---|---|
| Field evaluation | `wind/src/lib.rs` | `WindProfile`, `WindSample`, `sample`, `sample_composed` |
| Reading it apart | `wind/src/lib.rs` · `commands_scene/environment.rs` | `sample_decomposed`, `WindDecomposition`, `source_influence`, `sample-wind` |
| Physics consumer | `physics/src/world/query.rs` · `physics/src/world/step.rs` · `physics/src/world/bodies.rs` | `World::set_wind`, `World::sample_wind`, `apply_wind_drag`, `wind_drag_area` |
| Authored body coupling | `scene/src/component.rs` | `Rigidbody::wind_factor` |
| Whole-field capture | `renderer/lighting.rs` · `commands_render/stats.rs` | `Renderer::capture_interaction_field`, `wind-interaction-field` |
| GPU mirror | `wind.slang` | `sampleWindVelocity`, `windSwayOffset`, `windInstanceHash` |
| Deformation prepass + sway records | `wind_deform.slang` · `global_gpu_data.slang` | `GpuWindInstanceRecord`, `gpuSceneWindDeform`, `gpuSceneWindBoxSlack` |
| Per-part swept bounds | `vegetation/src/virtual_hierarchy.rs` | `deformation_regions`, `part_bounds`, `bend_share` |
| Authored plant response | `vegetation/src/asset/plant.rs` · `vegetation/src/artifact/plant.rs` · `global_gpu_data.slang` | `MechanicalResponse`, `mechanical_response`, `gpuSceneMechanicalResponse` |
| Response capture | `renderer/lighting.rs` · `commands_vegetation_runtime/state.rs` | `Renderer::capture_wind_record`, `vegetation-wind-record` |
| Interaction field + emitters | `wind_interact.slang` · `physics/src/world/query.rs` · `commands_render/stats.rs` | `gpuSceneInteractionSample`, `World::motion_emitters`, `emit-interaction-impulse` |
| Scroll-reset reactive marking | `wind_deform.slang` · `mesh.slang` · `renderer.rs` | `gpuSceneInteractionCascade`, `gpuSceneWindInteractionReset`, `vertexMainReactiveTransition` |
| Frame wind words | `lighting.rs` · `renderer.rs` | `SceneWind`, `Renderer::set_wind`, `Lighting::set_frame_wind` |
| Local sources | `scene/src/component.rs` · `scene/src/scene.rs` | `WindSource`, `Scene::local_wind_sources` |
| Authored settings | `scene/src/environment.rs` | `WindSettings` |
| Settings wire + validation | `control/src/commands_scene/environment.rs` | `set-wind`, `validate_wind` |
| Editor rows | `editor/src/panels/EnvironmentPanel/` | the Wind section |
| Editor read-out | `editor/src/panels/WindDebugPanel.tsx` | the probe, the spectrum bars, the source list, the field grid |
| The visual proof | `tests/e2e/vegetation-wind-visual.test.ts` | the still/gale motion comparison |
| The response proof | `tests/e2e/vegetation-mechanics.test.ts` | the flutter-to-branch amplitude ratio |

## Related

- [Vegetation state](../vegetation-state/) — the runtime plants that deform in this field
- [Cloud integration](../../image-based-lighting/cloud-integration/) — the sky-side consumer of the same settings
