+++
title = 'Light components'
weight = 1
math = true
+++

# Light components

A light component is an ECS component that carries the radiometric and shape parameters of one
light source. Anima has three: a directional sun, point lights, and spot lights. Nothing is
baked. Each frame the lights are gathered from the scene, packed into GPU structs, and uploaded
into the lighting set the fragment shader reads.

## The three components

All three are plain `Copy` structs in `saffron_scene` (the `component` module), attached to a
hecs entity. Position comes from the entity's `Transform`; the light itself only carries
radiometric and shape parameters.

```rust
pub struct DirectionalLight {
    pub direction: Vec3,  // the way the light travels
    pub color: Vec3,
    pub intensity: f32,
    pub ambient: f32,
}

pub struct PointLight {
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
}

pub struct SpotLight {
    pub direction: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,  // full intensity inside this half-angle (degrees)
    pub outer_angle: f32,  // zero past this half-angle
}
```

`DirectionalLight::default()` aims at `(-0.5, -1.0, -0.3)` with `ambient = 0.15`; `SpotLight`
defaults to a `20°`/`30°` cone with `range = 10.0`. The scene shades through the first
directional light it finds and ignores the rest; with no directional light there is no direct
sun. It is the only light carrying an `ambient` scalar, which feeds the flat-ambient fallback
when [IBL](../ibl-ambient-term/) is off (and only while a directional light exists). Point and
spot lights are the punctual lights: a position and an inverse-square falloff with a hard
`range`. A spot adds a cone aimed by `direction`, with a soft edge between `inner_angle` and
`outer_angle`.

## Two GPU shapes

The directional light and the punctual lights take different paths to the GPU because they are
evaluated differently. The directional light is a handful of scalars folded into the lighting
UBO (set 1, binding 0). The punctual lights become a variable-length array, one `GpuLight` per
point or spot light:

```rust
#[repr(C)]
pub struct GpuLight {
    pub position_range: Vec4,   // xyz = world position, w = range
    pub color_intensity: Vec4,  // rgb = color, a = intensity
    pub direction_type: Vec4,   // xyz = world direction (spot), w = type (0 = point, 1 = spot)
    pub spot_cos: Vec4,         // x = cos(inner_angle), y = cos(outer_angle)
}
```

Four `Vec4`s, `#[repr(C)]` + `bytemuck::Pod`, keep the struct naturally aligned for std430 (a
pinned `assert!(size_of::<GpuLight>() == 64)` holds the contract). A point light leaves
`direction_type` zeroed; a spot writes its normalized direction with type `1` in `.w` and
pre-computes the cosines of its two half-angles into `spot_cos`. The shader compares against
cosines, so converting degrees to cosine once on the CPU costs less than a `cos` per fragment.
`gather_punctual_lights` builds the array by querying the scene's `PointLight` and `SpotLight`
components.

## The upload

`Lighting::set_scene_lighting` writes the current frame's copies. Writing directly is safe
because the frame's fence was already waited, so no in-flight frame is reading them.

The punctual array goes into a host-mapped storage buffer (set 1, binding 1) whose capacity
grows to the next power of two on demand and never shrinks (`ensure_light_capacity`). The same
buffer is bound twice, into the fragment lighting set and into the compute cull set, so growing
it rewrites both descriptors. The directional light and the punctual count land in the
`LightUbo`, where `counts.x` is the count the brute-force loop reads and the other lanes are
feature flags ([directional shadow](../directional-light/), IBL, SSAO).

## In the code

| What | File | Symbols |
|---|---|---|
| The components | `engine/crates/scene/src/component.rs` | `DirectionalLight`, `PointLight`, `SpotLight` |
| Gather + pack | `engine/crates/assets/src/render_scene/` | `gather_directional_light`, `gather_punctual_lights` |
| The GPU struct | `engine/crates/rendering/src/gpu_types.rs` | `GpuLight` |
| The upload | `engine/crates/rendering/src/lighting/` | `Lighting::set_scene_lighting`, `Lighting::ensure_light_capacity`, `LightUbo` |

> [!NOTE]
> Only the first directional light shades the scene; extra ones are silently ignored, and a
> scene with none has no direct sun. The light buffer grows by powers of two and never shrinks
> within a session, so a scene that briefly spikes to many lights keeps the larger allocation.

## Related

- [Cook-Torrance BRDF](../cook-torrance-brdf/) — the model every one of these runs through
- [Punctual lights and attenuation](../punctual-lights-and-attenuation/) — how `range`, cone, and falloff are evaluated
- [Directional light](../directional-light/) — the shadowed sun term
- [Clustered forward](../clustered-forward/) — what culls the punctual array into froxels
