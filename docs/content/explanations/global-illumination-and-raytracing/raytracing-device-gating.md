+++
title = 'RT device gating'
weight = 7
+++

# RT device gating

Hardware ray tracing is an optional Vulkan capability: the `VK_KHR_acceleration_structure` and
`VK_KHR_ray_query` extensions (both introduced with
[Vulkan Ray Tracing](https://www.khronos.org/blog/ray-tracing-in-vulkan)) exist on some devices
and not on others. The renderer probes for them once at device creation, records the verdict in a
single flag, `Capabilities::rt_supported`, and every ray-tracing path downstream consults that
flag instead of re-asking the driver.

## Detection at device selection

`probe_optional_features` runs while the physical device is being selected. It enumerates the
device's extensions and requires both KHR extensions by name. Presence alone is not enough — the
feature bits must also be set — so it chains `vk::PhysicalDeviceAccelerationStructureFeaturesKHR`
and `vk::PhysicalDeviceRayQueryFeaturesKHR` into a `get_physical_device_features2` query:

```rust
let has_as = has_ext(ash::khr::acceleration_structure::NAME);
let has_rq = has_ext(ash::khr::ray_query::NAME);
let rt_supported = if has_as && has_rq {
    // chained get_physical_device_features2 query
    as_feat.acceleration_structure != 0 && rq_feat.ray_query != 0
} else {
    false
};
```

Optional features never gate selection. A device that lacks both extensions is created and used
regardless, and startup logs the verdict once: `ray tracing available (KHR acceleration_structure
+ ray_query)` or `ray tracing unavailable — RT passes disabled`.

The probe reads capabilities, not device type. Mesa's
[llvmpipe](https://docs.mesa3d.org/drivers/llvmpipe.html) software rasterizer advertises ray-query
through lavapipe, its Vulkan front end, so `rt_supported` can be true on a CPU device;
`Capabilities::software_gpu` is a separate flag with its own consumers.

## Enabling on the logical device

`create_logical_device` re-probes the same two extensions. Only when both are present does it push
them, plus `VK_KHR_deferred_host_operations` (a dependency of the acceleration-structure
extension), onto the device and chain the two RT feature structs into `vk::DeviceCreateInfo`. A
device without them is never asked to enable a feature it lacks.

`bufferDeviceAddress` is not part of this gate: it sits in the required feature set because the
renderer uses buffer device addresses broadly, and the VMA allocator always carries
`vk_mem::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS` so it sizes BDA-flagged allocations
correctly. Acceleration-structure builds, which reference vertex, index, and instance buffers by
device address, ride on that baseline.

## Resolving the command dispatch

The acceleration-structure commands are extension entry points, not core Vulkan. On a supporting
device, `Device::new` constructs the `ash::khr::acceleration_structure::Device` dispatch (a handle
plus a resolved function-pointer table) once and exposes it through `Device::accel_dispatch`,
which returns `None` on a non-RT device. The BLAS/TLAS build path calls every AS command through
it.

Each `AccelerationStructure` clones the dispatch at construction, so its `Drop` destroys the
`vk::AccelerationStructureKHR` handle without needing a live `&Device`. The dispatch exists if and
only if the structure could have been built, so the destroy entry point is always resolvable.

## The gate downstream

`rt_supported` is a hard precondition for every RT consumer:

- `Rt::supported` mirrors the flag. `Renderer::set_rt_shadows` and `set_rt_reflections` clamp
  their toggles to `enabled && supported`, so neither can arm on a non-RT device.
- The `tlas-build` pass is added to the frame graph only when `Rt::build_pending`, which is
  `supported && (rt shadows || rt reflections)`.
- The `Uploader` builds a per-mesh BLAS at upload only when it holds the dispatch; otherwise
  `GpuMesh::blas` stays `None` and shading takes the shadow-map path.
- `Restir::supported` gates [ReSTIR](../restir-overview/) the same way; `set_restir` clamps
  through `Restir::set_enabled`.

The control plane refuses rather than silently ignoring. The `set-rt-shadows`,
`set-rt-reflections`, and `set-restir` commands each check `Renderer::rt_supported` first and
return a command error on a non-RT device:

```sh
sa set-rt-shadows 1
# sa: ray tracing not supported on this device
```

On a supporting device the reply reports the effective state (`{ "rt_shadows": … }`), which is
true only once the toggle is on, RT is supported, and a TLAS has been built for the frame.

## The RT-off übershader variant

The mesh übershader declares the TLAS binding (`rtScene`, set 6) and the ReSTIR radiance sampler
(set 7) inside an `#ifndef SAFFRON_NO_RT` block. `xtask shaders` compiles it twice: `mesh.spv`
with the RT bindings and `mesh_nort.spv` with `SAFFRON_NO_RT=1`, which strips those sets and the
ray-query helper functions.

On a non-RT device the mesh pipeline layout omits the set-6/7 layouts, since an
acceleration-structure descriptor needs the extension, and `load_shader_module` prefers the
`_nort` sibling of the requested SPIR-V. The shader's declared descriptor interface then matches
the pipeline layout it runs under. Strict argument-buffer backends such as
[MoltenVK](https://github.com/KhronosGroup/MoltenVK) require exactly that match.

## In the code

| What | File | Symbols |
|---|---|---|
| Extension + feature probe | `rendering/src/device.rs` | `probe_optional_features`, `Capabilities::rt_supported` |
| Device extension / feature enable | `rendering/src/device.rs` | `create_logical_device` (the `enable_rt` branch) |
| BDA on the allocator | `rendering/src/device.rs` | `create_allocator` |
| The command dispatch | `rendering/src/device.rs` | `Device::accel_dispatch` |
| Self-contained destroy | `rendering/src/resources.rs` | `AccelerationStructure` (the cloned dispatch) |
| Runtime clamps | `rendering/src/rt.rs`, `restir.rs` | `Rt::supported`, `Rt::set_rt_shadows`, `Rt::build_pending`; `Restir::supported` |
| BLAS-at-upload gate | `rendering/src/upload.rs` | `Uploader::build_mesh_blas` |
| RT-off shader variant | `xtask/src/shaders.rs`, `rendering/src/pipelines.rs` | `NO_RT_DEFINE`, `nort_variant_path` |
| Control-plane checks | `control/src/commands_render.rs` | the `set-rt-shadows`, `set-restir`, `set-rt-reflections` registrations |

## Related

- [Acceleration structures](../raytracing-foundation/) — what the resolved entry points build
- [Ray-query shadows](../ray-query-shadows/) — the gated consumer in the mesh fragment
- [ReSTIR overview](../restir-overview/) — the other clamped consumer
- [Device and swapchain](../../vulkan-foundation/device-and-swapchain/) — where device selection lives
