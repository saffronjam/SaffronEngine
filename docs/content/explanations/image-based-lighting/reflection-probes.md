+++
title = 'Reflection probes'
weight = 8
math = true
+++

# Reflection probes

A reflection-probe component describes a bounded source of local image-based lighting (IBL). Its scene data, renderer metadata, descriptor bindings, and shader-selection contract all exist, but the renderer does not create a cubemap for an individual probe. Every descriptor-array slot therefore points at the global IBL cubes, and every metadata record remains invalid. Probe components do not change the rendered image.

## Authoring record

`ReflectionProbe` gets its origin from the entity's world transform. The component stores the volume and sampling controls:

| Field | Default | Meaning |
|---|---:|---|
| `influence_radius` | `10.0` | Radius of the spherical selection volume, in world units |
| `intensity` | `1.0` | Multiplier for the local-probe blend weight |
| `box_projection` | `false` | Selects box-projected reflection directions |
| `box_extent` | `(10.0, 10.0, 10.0)` | Half-extents used by box projection |

The serialized component uses `influenceRadius`, `intensity`, `boxProjection`, and `boxExtent`. Loading a component and editing it through the registry set its runtime-only `dirty` flag. A saved component has this shape:

```json
"ReflectionProbe": {
  "influenceRadius": 10.0,
  "intensity": 1.0,
  "boxProjection": false,
  "boxExtent": [10.0, 10.0, 10.0]
}
```

## Upload and slot state

Scene gathering sends at most `MAX_REFLECTION_PROBES = 8` records to the renderer. Each `ReflectionProbeUpload` carries the stable entity ID, world-space origin, component fields, and dirty state. Gathering clears the component's dirty flag after copying it.

Each frame slot has an IBL descriptor set with three probe bindings:

| Binding | Resource |
|---:|---|
| `3` | Array of eight prefiltered cubemaps |
| `4` | Array of eight irradiance cubemaps |
| `5` | Storage buffer of `ProbeMetaGpu` records |

`ReflectionProbes::seed` and `ReflectionProbes::write_slot` bind the global prefiltered cube and
environment cube as valid fallback descriptors for every array element in every frame set. Global
diffuse lighting comes from the sky SH buffer, outside these local-probe arrays. A metadata record
occupies 48 bytes: origin and radius, box extent and intensity, then validity and box-projection
flags.

`ReflectionProbes::submit` tracks entity, origin, radius, and dirty changes in CPU state and raises
`capture_pending` when a slot needs new data. `prepare_frame` writes that state into the metadata
buffer paired with the frame slot whose fence has completed. Descriptor binding 5 remains fixed for
the set's lifetime; only bindings 3 and 4 use update-after-bind when the global fallback changes. No
renderer operation consumes `capture_pending` or marks a slot allocated and valid, so every uploaded
record keeps `valid = 0`.

> [!NOTE]
> `sa recapture-probes` marks scene components dirty and causes submission to raise `capture_pending`; it does not capture a cubemap.

## Shader contract

The mesh shader defines the selection and blend that a valid metadata record would use. It examines the first `probeCount` records, skips records whose validity flag is zero, and chooses the nearest probe whose sphere contains the fragment.

For box projection, `boxProject` applies the slab-intersection form of [parallax-corrected cubemap sampling](https://seblagarde.wordpress.com/2012/09/29/image-based-lighting-approaches-and-parallax-corrected-cubemap/):

$$
R' = \big(p + R d\big) - o,
\qquad
d = \min_i \max\!\left(t_i^+, t_i^-\right),
$$

where $p$ is the fragment position, $R$ is its reflection direction, $o$ is the probe origin, and $t_i^+$ and $t_i^-$ are intersections with the positive and negative box planes.

The selected record blends both the prefiltered reflection and irradiance samples over the global cubes. For distance $d$, influence radius $r$, and authored intensity $I$, the weight is

$$
w = \operatorname{saturate}\!\left(1 - \frac{d/r - 0.6}{0.4}\right) I.
$$

The contribution is constant through 60% of the radius and falls to zero at the boundary. The specular blend belongs to the shared IBL path. The irradiance blend feeds transparent surfaces; opaque diffuse lighting reads the screen-space GI resolve instead.

The CPU bit-casts the active record count into `LightUbo.ambient_color.w`. `sa set-probes 0` makes that count zero. With probe sampling enabled, the count can be nonzero, but invalid records are skipped and the global IBL values remain unchanged.

## Inspection and control

`sa list-probes` reports each renderer slot, including `allocated`, `valid`, and `dirty`. The allocated and valid fields remain false because no per-probe cube resource is created. `sa set-probes {0|1}` controls whether the renderer publishes a nonzero probe count, and `sa recapture-probes` marks all active-scene probe components dirty.

## In code

| What | File | Symbols |
|---|---|---|
| Scene component | `engine/crates/scene/src/component.rs` | `ReflectionProbe` |
| Save and load | `engine/crates/scene/src/serde.rs` | `SceneSerialize for ReflectionProbe`, `to_json`, `load_json` |
| Scene gathering | `engine/crates/assets/src/render_scene.rs` | `gather_reflection_probes` |
| Upload and metadata | `engine/crates/rendering/src/ibl.rs` | `ReflectionProbeUpload`, `ProbeMetaGpu`, `ReflectionProbes::submit`, `ReflectionProbes::prepare_frame` |
| Descriptor slots | `engine/crates/rendering/src/ibl.rs` | `ReflectionProbes::seed`, `ReflectionProbes::refresh_fallbacks`, `ReflectionProbes::write_slot` |
| Shader selection | `engine/assets/shaders/lighting.slang` | `ProbeMeta`, `boxProject`, `probeCubes`, `probeIrradiance`, `probeMeta` |
| Control commands | `engine/crates/control/src/commands_render.rs` | `set-probes`, `recapture-probes`, `list-probes` |

## Related

- [Image-based lighting](../)
- [IBL bake pass](../ibl-bake-pass/)
- [Specular prefilter](../specular-prefilter/)
- [Dynamic diffuse global illumination](../../global-illumination-and-raytracing/ddgi-overview/)
