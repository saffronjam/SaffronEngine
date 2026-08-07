//! The optional-feature probe, the resolved [`Capabilities`], logical-device creation, and
//! the VMA allocator. Each probe degrades to the next-best path rather than failing bring-up.

use super::*;

/// Probes the optional features that never gate selection but tune the renderer.
pub(super) fn probe_optional_features(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    props: &vk::PhysicalDeviceProperties,
    name: &str,
) -> Capabilities {
    // SAFETY: the ash seam. Core feature + extension queries on the device.
    let core_features = unsafe { instance.get_physical_device_features(physical_device) };
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .unwrap_or_default();
    let has_ext = |needle: &CStr| {
        extensions.iter().any(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == needle)
                .unwrap_or(false)
        })
    };

    let has_as = has_ext(ash::khr::acceleration_structure::NAME);
    let has_rq = has_ext(ash::khr::ray_query::NAME);
    let has_deferred = has_ext(ash::khr::deferred_host_operations::NAME);
    let (rt_supported, acceleration_structure_indirect_build) = if has_as && has_rq && has_deferred
    {
        let mut as_feat = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
        let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
        let mut feat2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut as_feat)
            .push_next(&mut rq_feat);
        // SAFETY: the ash seam. Fills the chained RT feature structs.
        unsafe { instance.get_physical_device_features2(physical_device, &mut feat2) };
        resolve_rt_capabilities(
            true,
            as_feat.acceleration_structure != 0,
            rq_feat.ray_query != 0,
            as_feat.acceleration_structure_indirect_build != 0,
        )
    } else {
        (false, false)
    };

    // The micromap extension is only meaningful when acceleration structures are also present,
    // so it inherits the RT gate rather than being probed independently.
    let omm_extension = rt_supported && has_ext(ash::ext::opacity_micromap::NAME);
    let opacity_micromap = if omm_extension {
        let mut omm_feat = vk::PhysicalDeviceOpacityMicromapFeaturesEXT::default();
        let mut omm_feat2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut omm_feat);
        // SAFETY: the ash seam. Fills the chained micromap feature struct.
        unsafe { instance.get_physical_device_features2(physical_device, &mut omm_feat2) };
        omm_feat.micromap != 0
    } else {
        false
    };

    // Cluster acceleration structures inherit the RT gate the same way, plus the spec-revision
    // belt: the transcribed structs follow revision 4 of the header exactly, and a revision
    // bump can move layouts with no generator following it here.
    let cluster_spec = extensions
        .iter()
        .find(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == crate::vk_nv_cluster::NAME)
                .unwrap_or(false)
        })
        .map(|ext| ext.spec_version);
    let cluster_extension =
        rt_supported && cluster_spec == Some(crate::vk_nv_cluster::SPEC_VERSION);
    let (cluster_acceleration_structure, cluster_as_limits) = if cluster_extension {
        let mut cluster_feat =
            crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructureFeaturesNV::default();
        let mut cluster_feat2 = vk::PhysicalDeviceFeatures2 {
            p_next: (&raw mut cluster_feat).cast(),
            ..Default::default()
        };
        // SAFETY: the extension seam. The chained struct is the extension's feature query
        // shape at the probed spec revision, alive across the call.
        unsafe { instance.get_physical_device_features2(physical_device, &mut cluster_feat2) };
        if cluster_feat.cluster_acceleration_structure != 0 {
            let mut cluster_props =
                crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructurePropertiesNV::default();
            let mut props2 = vk::PhysicalDeviceProperties2 {
                p_next: (&raw mut cluster_props).cast(),
                ..Default::default()
            };
            // SAFETY: the extension seam, same shape contract as the feature query.
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            (
                true,
                [
                    cluster_props.max_triangles_per_cluster,
                    cluster_props.max_vertices_per_cluster,
                    cluster_props.cluster_scratch_byte_alignment,
                    cluster_props.cluster_byte_alignment,
                    cluster_props.cluster_bottom_level_byte_alignment,
                ],
            )
        } else {
            (false, [0; 5])
        }
    } else {
        (false, [0; 5])
    };

    // Partitioned top-level structures inherit the same RT gate and spec-revision belt, and
    // one more: they are taken only when asked for.
    //
    // The extension executes correctly here — a partitioned structure renders a frame
    // byte-identical to the KHR one — but the SDK's validation layers do not model it: they
    // report a descriptor-type mismatch for a shader variable that has no partitioned SPIR-V
    // form to declare (the extension defines no SPIR-V capability), and cannot resolve the
    // structure's address to an acceleration-structure object because a partitioned structure
    // is memory rather than an object. Neither is fixable from here, and a default-on path
    // that cannot be validated is worse than an opt-in one that can. Remove the flag when the
    // layers catch up.
    let ptlas_spec = extensions
        .iter()
        .find(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == crate::vk_nv_ptlas::NAME)
                .unwrap_or(false)
        })
        .map(|ext| ext.spec_version);
    let ptlas_extension = rt_supported
        && ptlas_spec == Some(crate::vk_nv_ptlas::SPEC_VERSION)
        && std::env::var_os("SAFFRON_PTLAS").is_some();
    let (partitioned_acceleration_structure, max_partition_count) = if ptlas_extension {
        let mut ptlas_feat =
            crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructureFeaturesNV::default();
        let mut ptlas_feat2 = vk::PhysicalDeviceFeatures2 {
            p_next: (&raw mut ptlas_feat).cast(),
            ..Default::default()
        };
        // SAFETY: the extension seam. The chained struct is the extension's feature query
        // shape at the probed spec revision, alive across the call.
        unsafe { instance.get_physical_device_features2(physical_device, &mut ptlas_feat2) };
        if ptlas_feat.partitioned_acceleration_structure != 0 {
            let mut ptlas_props =
                crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructurePropertiesNV::default();
            let mut props2 = vk::PhysicalDeviceProperties2 {
                p_next: (&raw mut ptlas_props).cast(),
                ..Default::default()
            };
            // SAFETY: the extension seam, same shape contract as the feature query.
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            (true, ptlas_props.max_partition_count)
        } else {
            (false, 0)
        }
    } else {
        (false, 0)
    };

    let mesh_extension = has_ext(ash::ext::mesh_shader::NAME);
    let mut features11 = vk::PhysicalDeviceVulkan11Features::default();
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut mesh_features = vk::PhysicalDeviceMeshShaderFeaturesEXT::default();
    let mut features2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut features11)
        .push_next(&mut features12)
        .push_next(&mut features13);
    if mesh_extension {
        features2 = features2.push_next(&mut mesh_features);
    }
    // SAFETY: the ash seam. Fills the chained core and advertised extension features.
    unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };

    let mut descriptor_properties = vk::PhysicalDeviceDescriptorIndexingProperties::default();
    let mut subgroup_properties = vk::PhysicalDeviceSubgroupProperties::default();
    let mut subgroup_size_properties = vk::PhysicalDeviceSubgroupSizeControlProperties::default();
    let mut mesh_properties = vk::PhysicalDeviceMeshShaderPropertiesEXT::default();
    let mut properties2 = vk::PhysicalDeviceProperties2::default()
        .push_next(&mut descriptor_properties)
        .push_next(&mut subgroup_properties)
        .push_next(&mut subgroup_size_properties);
    if mesh_extension {
        properties2 = properties2.push_next(&mut mesh_properties);
    }
    // SAFETY: the ash seam. Fills the chained core and advertised extension properties.
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };

    let lower = name.to_ascii_lowercase();
    let software_gpu = lower.contains("llvmpipe")
        || lower.contains("lavapipe")
        || lower.contains("swiftshader")
        || lower.contains("software")
        || props.device_type == vk::PhysicalDeviceType::CPU;

    resolve_capabilities(
        props,
        &core_features,
        &features11,
        &features12,
        &features13,
        &descriptor_properties,
        &subgroup_properties,
        &subgroup_size_properties,
        mesh_extension.then_some((&mesh_features, &mesh_properties)),
        rt_supported,
        opacity_micromap,
        cluster_acceleration_structure,
        cluster_as_limits,
        partitioned_acceleration_structure,
        max_partition_count,
        acceleration_structure_indirect_build,
        has_ext(ash::ext::memory_budget::NAME),
        software_gpu,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_capabilities(
    props: &vk::PhysicalDeviceProperties,
    core_features: &vk::PhysicalDeviceFeatures,
    features11: &vk::PhysicalDeviceVulkan11Features<'_>,
    features12: &vk::PhysicalDeviceVulkan12Features<'_>,
    features13: &vk::PhysicalDeviceVulkan13Features<'_>,
    descriptor_properties: &vk::PhysicalDeviceDescriptorIndexingProperties<'_>,
    subgroup_properties: &vk::PhysicalDeviceSubgroupProperties<'_>,
    subgroup_size_properties: &vk::PhysicalDeviceSubgroupSizeControlProperties<'_>,
    mesh: Option<(
        &vk::PhysicalDeviceMeshShaderFeaturesEXT<'_>,
        &vk::PhysicalDeviceMeshShaderPropertiesEXT<'_>,
    )>,
    rt_supported: bool,
    opacity_micromap: bool,
    cluster_acceleration_structure: bool,
    cluster_as_limits: [u32; 5],
    partitioned_acceleration_structure: bool,
    max_partition_count: u32,
    acceleration_structure_indirect_build: bool,
    memory_budget: bool,
    software_gpu: bool,
) -> Capabilities {
    let (mesh_features, mesh_properties) = mesh.unzip();
    let max_bindless_array_elements = [
        descriptor_properties.max_update_after_bind_descriptors_in_all_pools / 5,
        descriptor_properties.max_per_stage_descriptor_update_after_bind_samplers / 4,
        descriptor_properties.max_per_stage_descriptor_update_after_bind_sampled_images / 5,
        descriptor_properties.max_per_stage_update_after_bind_resources / 5,
        descriptor_properties.max_descriptor_set_update_after_bind_samplers / 4,
        descriptor_properties.max_descriptor_set_update_after_bind_sampled_images / 5,
    ]
    .into_iter()
    .min()
    .unwrap_or(0);
    Capabilities {
        rt_supported,
        opacity_micromap,
        // Filled from `VkPhysicalDeviceOpacityMicromapPropertiesEXT` once the device exists;
        // the selection pass only decides whether the extension is enabled at all.
        omm_max_subdivision: 0,
        cluster_acceleration_structure,
        cluster_as_limits,
        partitioned_acceleration_structure,
        max_partition_count,
        mesh_shader: mesh_features.is_some_and(|features| features.mesh_shader != 0),
        task_shader: mesh_features.is_some_and(|features| features.task_shader != 0),
        max_mesh_work_group_count: mesh_properties
            .map_or([0; 3], |properties| properties.max_mesh_work_group_count),
        max_mesh_work_group_invocations: mesh_properties
            .map_or(0, |properties| properties.max_mesh_work_group_invocations),
        max_mesh_output_vertices: mesh_properties
            .map_or(0, |properties| properties.max_mesh_output_vertices),
        max_mesh_output_primitives: mesh_properties
            .map_or(0, |properties| properties.max_mesh_output_primitives),
        max_task_work_group_count: mesh_properties
            .map_or([0; 3], |properties| properties.max_task_work_group_count),
        max_task_work_group_invocations: mesh_properties
            .map_or(0, |properties| properties.max_task_work_group_invocations),
        max_task_payload_size: mesh_properties
            .map_or(0, |properties| properties.max_task_payload_size),
        fill_mode_non_solid: core_features.fill_mode_non_solid != 0,
        memory_budget,
        pipeline_stats: core_features.pipeline_statistics_query != 0,
        software_gpu,
        capture_supported: false,
        max_anisotropy: if core_features.sampler_anisotropy != 0 {
            props.limits.max_sampler_anisotropy.min(16.0)
        } else {
            1.0
        },
        multi_draw_indirect: core_features.multi_draw_indirect != 0,
        draw_indirect_count: {
            let supported = features12.draw_indirect_count != 0;
            tracing::info!(
                "gpu-driven draws: drawIndirectCount {} (maxDrawIndirectCount {})",
                if supported {
                    "supported"
                } else {
                    "UNSUPPORTED — fixed-slice draws"
                },
                props.limits.max_draw_indirect_count
            );
            supported
        },
        max_draw_indirect_count: props.limits.max_draw_indirect_count,
        buffer_device_address: features12.buffer_device_address != 0,
        shader_draw_parameters: features11.shader_draw_parameters != 0,
        runtime_descriptor_array: features12.runtime_descriptor_array != 0,
        descriptor_binding_partially_bound: features12.descriptor_binding_partially_bound != 0,
        descriptor_binding_sampled_image_update_after_bind: features12
            .descriptor_binding_sampled_image_update_after_bind
            != 0,
        shader_sampled_image_array_non_uniform_indexing: features12
            .shader_sampled_image_array_non_uniform_indexing
            != 0,
        max_update_after_bind_descriptors_in_all_pools: descriptor_properties
            .max_update_after_bind_descriptors_in_all_pools,
        max_per_stage_descriptor_update_after_bind_sampled_images: descriptor_properties
            .max_per_stage_descriptor_update_after_bind_sampled_images,
        max_descriptor_set_update_after_bind_sampled_images: descriptor_properties
            .max_descriptor_set_update_after_bind_sampled_images,
        max_per_stage_descriptor_update_after_bind_samplers: descriptor_properties
            .max_per_stage_descriptor_update_after_bind_samplers,
        max_descriptor_set_update_after_bind_samplers: descriptor_properties
            .max_descriptor_set_update_after_bind_samplers,
        max_per_stage_update_after_bind_resources: descriptor_properties
            .max_per_stage_update_after_bind_resources,
        max_bindless_array_elements,
        subgroup_size: subgroup_properties.subgroup_size,
        subgroup_supported_stages: subgroup_properties.supported_stages,
        subgroup_supported_operations: subgroup_properties.supported_operations,
        subgroup_quad_operations_in_all_stages: subgroup_properties.quad_operations_in_all_stages
            != 0,
        subgroup_size_control: features13.subgroup_size_control != 0,
        compute_full_subgroups: features13.compute_full_subgroups != 0,
        min_subgroup_size: subgroup_size_properties.min_subgroup_size,
        max_subgroup_size: subgroup_size_properties.max_subgroup_size,
        max_compute_workgroup_subgroups: subgroup_size_properties.max_compute_workgroup_subgroups,
        required_subgroup_size_stages: subgroup_size_properties.required_subgroup_size_stages,
        acceleration_structure_indirect_build,
        min_uniform_buffer_offset_alignment: props.limits.min_uniform_buffer_offset_alignment,
    }
}

/// Creates the logical device with the required feature chain and (when present)
/// the RT extensions enabled.
///
/// `enable_swapchain` gates `VK_KHR_swapchain`: the windowed host presents through a
/// swapchain and enables it, while the offscreen host never presents and enables no
/// surface extension at instance level, so it must not enable the swapchain device
/// extension either (`VK_KHR_swapchain` requires the instance-level `VK_KHR_surface`,
/// and enabling it without that fails `VUID-vkCreateDevice-ppEnabledExtensionNames-01387`).
/// Returns the device and whether `VK_EXT_calibrated_timestamps` was enabled on it
/// (the caller resolves its dispatch + domain check from that flag).
pub(super) fn create_logical_device(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    graphics_queue_family: u32,
    compute_queue: Option<AsyncComputeQueue>,
    capabilities: &Capabilities,
    enable_swapchain: bool,
) -> Result<(ash::Device, bool, bool, bool)> {
    let single_queue_priority = [1.0_f32];
    let two_queue_priorities = [1.0_f32, 1.0_f32];
    let mut queue_infos = if compute_queue
        .is_some_and(|queue| queue.family == graphics_queue_family && queue.index == 1)
    {
        vec![
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_queue_family)
                .queue_priorities(&two_queue_priorities),
        ]
    } else {
        vec![
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_queue_family)
                .queue_priorities(&single_queue_priority),
        ]
    };
    if let Some(queue) = compute_queue
        && queue.family != graphics_queue_family
    {
        queue_infos.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue.family)
                .queue_priorities(&single_queue_priority),
        );
    }

    // Extension enumeration is retained only for extensions whose activation is not a
    // renderer feature policy (portability and optional clock calibration).
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .map_err(|result| Error::Vk {
            context: "enumerate_device_extension_properties",
            result,
        })?;
    let has_ext = |needle: &CStr| {
        extensions.iter().any(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == needle)
                .unwrap_or(false)
        })
    };
    let enable_rt = capabilities.rt_supported;
    let enable_mesh_extension = capabilities.mesh_shader || capabilities.task_shader;

    let mut device_extensions: Vec<*const c_char> = Vec::new();
    // The swapchain device extension requires the instance-level `VK_KHR_surface`,
    // which only the windowed host enables; the offscreen host presents nothing.
    if enable_swapchain {
        device_extensions.push(swapchain::NAME.as_ptr());
    }
    if enable_rt {
        device_extensions.push(ash::khr::acceleration_structure::NAME.as_ptr());
        device_extensions.push(ash::khr::ray_query::NAME.as_ptr());
        device_extensions.push(ash::khr::deferred_host_operations::NAME.as_ptr());
    }
    if capabilities.opacity_micromap {
        device_extensions.push(ash::ext::opacity_micromap::NAME.as_ptr());
    }
    if capabilities.cluster_acceleration_structure {
        device_extensions.push(crate::vk_nv_cluster::NAME.as_ptr());
    }
    if capabilities.partitioned_acceleration_structure {
        device_extensions.push(crate::vk_nv_ptlas::NAME.as_ptr());
    }
    if enable_mesh_extension {
        device_extensions.push(ash::ext::mesh_shader::NAME.as_ptr());
    }
    if capabilities.memory_budget {
        device_extensions.push(ash::ext::memory_budget::NAME.as_ptr());
    }
    // A portability physical device (MoltenVK) that advertises `VK_KHR_portability_subset` MUST
    // have it enabled at device creation (`VUID-VkDeviceCreateInfo-pProperties-04451`). It is
    // absent on native drivers, so the presence check keeps one code path across hosts.
    if has_ext(ash::khr::portability_subset::NAME) {
        device_extensions.push(ash::khr::portability_subset::NAME.as_ptr());
    }
    // VK_EXT_calibrated_timestamps lets the profiler project GPU spans onto the CPU clock.
    // The env var forces the own-axis fallback (testing it on hardware that supports it).
    let enable_calibrated_ts = has_ext(calibrated_timestamps::NAME)
        && std::env::var_os("SAFFRON_DISABLE_CALIBRATION").is_none();
    if enable_calibrated_ts {
        device_extensions.push(calibrated_timestamps::NAME.as_ptr());
    }
    // VK_NV_device_diagnostic_checkpoints: named per-pass progress markers so a device loss
    // reports which submission the GPU wedged in.
    let enable_checkpoints = has_ext(ash::nv::device_diagnostic_checkpoints::NAME);
    if enable_checkpoints {
        device_extensions.push(ash::nv::device_diagnostic_checkpoints::NAME.as_ptr());
    }
    // VK_EXT_device_fault: the driver's post-loss fault report (kind + faulting addresses).
    let mut fault_query = vk::PhysicalDeviceFaultFeaturesEXT::default();
    if has_ext(ash::ext::device_fault::NAME) {
        let mut features2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut fault_query);
        // SAFETY: the ash seam. Fills the chained fault feature struct.
        unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };
    }
    let enable_device_fault = fault_query.device_fault != 0;
    if enable_device_fault {
        device_extensions.push(ash::ext::device_fault::NAME.as_ptr());
    }

    let mut enabled_core = vk::PhysicalDeviceFeatures::default().shader_int64(true);
    if capabilities.pipeline_stats {
        enabled_core = enabled_core.pipeline_statistics_query(true);
    }
    if capabilities.fill_mode_non_solid {
        enabled_core = enabled_core.fill_mode_non_solid(true);
    }
    // Anisotropic filtering for the material sampler: the correct minification filter for
    // high-frequency albedo/AO textures at grazing angles, so they stay band-limited
    // rather than aliasing into the in-motion shimmer TAA would otherwise have to hide.
    if capabilities.max_anisotropy > 1.0 {
        enabled_core = enabled_core.sampler_anisotropy(true);
    }
    // `multiDrawIndirect` lets one indirect command issue more than one draw.
    if capabilities.multi_draw_indirect {
        enabled_core = enabled_core.multi_draw_indirect(true);
    }
    let mut features11 = vk::PhysicalDeviceVulkan11Features::default()
        .shader_draw_parameters(capabilities.shader_draw_parameters);
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default()
        .runtime_descriptor_array(capabilities.runtime_descriptor_array)
        .descriptor_binding_partially_bound(capabilities.descriptor_binding_partially_bound)
        .descriptor_binding_sampled_image_update_after_bind(
            capabilities.descriptor_binding_sampled_image_update_after_bind,
        )
        .shader_sampled_image_array_non_uniform_indexing(
            capabilities.shader_sampled_image_array_non_uniform_indexing,
        )
        .timeline_semaphore(true)
        .buffer_device_address(capabilities.buffer_device_address)
        .draw_indirect_count(capabilities.draw_indirect_count);
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true)
        .subgroup_size_control(capabilities.subgroup_size_control)
        .compute_full_subgroups(capabilities.compute_full_subgroups);
    let mut as_feat = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default()
        .acceleration_structure(enable_rt)
        .acceleration_structure_indirect_build(capabilities.acceleration_structure_indirect_build);
    let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(enable_rt);
    let mut omm_feat = vk::PhysicalDeviceOpacityMicromapFeaturesEXT::default()
        .micromap(capabilities.opacity_micromap);
    let mut ms_feat = vk::PhysicalDeviceMeshShaderFeaturesEXT::default()
        .mesh_shader(capabilities.mesh_shader)
        .task_shader(capabilities.task_shader);
    let mut fault_feat = vk::PhysicalDeviceFaultFeaturesEXT::default().device_fault(true);
    let mut cluster_feat =
        crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructureFeaturesNV {
            cluster_acceleration_structure: vk::TRUE,
            ..Default::default()
        };
    let mut ptlas_feat =
        crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructureFeaturesNV {
            partitioned_acceleration_structure: vk::TRUE,
            ..Default::default()
        };

    let mut create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_infos)
        .enabled_extension_names(&device_extensions)
        .enabled_features(&enabled_core)
        .push_next(&mut features11)
        .push_next(&mut features12)
        .push_next(&mut features13);
    if enable_rt {
        create_info = create_info.push_next(&mut as_feat).push_next(&mut rq_feat);
    }
    if capabilities.opacity_micromap {
        create_info = create_info.push_next(&mut omm_feat);
    }
    if enable_mesh_extension {
        create_info = create_info.push_next(&mut ms_feat);
    }
    if enable_device_fault {
        create_info = create_info.push_next(&mut fault_feat);
    }
    // The transcribed feature structs cannot ride ash's typed `push_next`, so each heads the
    // chain by hand: it points at whatever was built so far, and the create info points at it.
    if capabilities.cluster_acceleration_structure {
        cluster_feat.p_next = create_info.p_next.cast_mut();
        create_info.p_next = (&raw const cluster_feat).cast();
    }
    if capabilities.partitioned_acceleration_structure {
        ptlas_feat.p_next = create_info.p_next.cast_mut();
        create_info.p_next = (&raw const ptlas_feat).cast();
    }

    // SAFETY: the ash seam. The feature chain + extension pointers outlive the
    // call; the returned device is owned and destroyed in `Device::drop`.
    let device = unsafe { instance.create_device(physical_device, &create_info, None) }.map_err(
        |result| Error::Vk {
            context: "create_device",
            result,
        },
    )?;
    Ok((
        device,
        enable_calibrated_ts,
        enable_checkpoints,
        enable_device_fault,
    ))
}

/// Creates the VMA allocator over the ash instance/device. `memory_budget` says whether
/// `VK_EXT_memory_budget` was enabled on the device.
pub(super) fn create_allocator(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    memory_budget: bool,
) -> Result<vk_mem::Allocator> {
    let mut create_info = vk_mem::AllocatorCreateInfo::new(instance, device, physical_device);
    create_info.vulkan_api_version = API_VERSION;
    // The required feature set enables bufferDeviceAddress, which AS builds need —
    // and VMA must know about it to size BDA-flagged allocations.
    create_info.flags = vk_mem::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;
    // Without this bit `vmaGetHeapBudgets` reports VMA's own block totals against a fraction of
    // each heap's size; with it the figures come from the driver and cover every allocation on
    // the adapter, which is what a residency budget is held against.
    if memory_budget {
        create_info.flags |= vk_mem::AllocatorCreateFlags::EXT_MEMORY_BUDGET;
    }

    // SAFETY: the ash seam. The instance/device/physical-device handles are valid
    // for the allocator's whole lifetime (it is dropped before they are destroyed,
    // by the `Device` field order); the allocator captures ash's loaded function
    // pointers at creation.
    let allocator = unsafe { vk_mem::Allocator::new(create_info) }.map_err(|result| Error::Vk {
        context: "vmaCreateAllocator",
        result,
    })?;
    Ok(allocator)
}
