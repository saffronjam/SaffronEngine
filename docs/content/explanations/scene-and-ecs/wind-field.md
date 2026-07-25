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
plants swing slower; the cull inflates by the mode amplitudes.

The world interaction field composes with wind at the same application point. Two
camera-centred cascades of damped-oscillator texels store a horizontal push and a
ground depression; moving physics bodies and characters emit impulses into them
each play step, `emit-interaction-impulse` pushes them from tooling, and every
texel springs back to rest over about a second. The prepass samples the field at
each instance root and micro blades fold it into their baked bend, so trampled
vegetation leans from the root while wind keeps swaying the tips.

Placeable `WindSource` entities composite over the global field: directional, point,
vortex, wake, and volume influences with a radius and edge falloff. A volume source
scales the global term (zero strength shelters its interior); the others add their own
velocities. `sample_composed` folds them in on the CPU, `sample-wind` exposes the
composed field to tooling, and the frame's sources upload to a small ring so
`sampleComposedWindVelocity` composes the identical field in every GPU sampler — the
deformation prepass, the blade scatter, and the fog march.

```sh
sa set-wind --speed 14 --json '{"turbulenceOctaves": 4, "gustFrequency": 0.3}'
sa get-environment -o json | jq .wind
sa -o json sample-wind --positionM '[12, 3, 40]'
sa emit-interaction-impulse --positionM '[12, 40]' --radiusM 2 --strength 4
```

## In the code

| What | File | Symbols |
|---|---|---|
| Field evaluation | `wind/src/lib.rs` | `WindProfile`, `WindSample`, `sample`, `sample_composed` |
| GPU mirror | `wind.slang` | `sampleWindVelocity`, `windSwayOffset`, `windInstanceHash` |
| Deformation prepass + sway records | `wind_deform.slang` · `global_gpu_data.slang` | `GpuWindInstanceRecord`, `gpuSceneWindSway` |
| Interaction field + emitters | `wind_interact.slang` · `world.rs` · `commands_render.rs` | `gpuSceneInteractionSample`, `World::motion_emitters`, `emit-interaction-impulse` |
| Frame wind words | `lighting.rs` · `renderer.rs` | `SceneWind`, `Renderer::set_wind`, `Lighting::set_frame_wind` |
| Local sources | `scene/src/component.rs` · `scene/src/scene.rs` | `WindSource`, `Scene::local_wind_sources` |
| Authored settings | `scene/src/environment.rs` | `WindSettings` |
| Settings wire + validation | `control/src/commands_scene.rs` | `set-wind`, `validate_wind` |
| Editor rows | `editor/src/panels/EnvironmentPanel.tsx` | the Wind section |

## Related

- [Vegetation state](../vegetation-state/) — the runtime plants that deform in this field
- [Cloud integration](../../image-based-lighting/cloud-integration/) — the sky-side consumer of the same settings
