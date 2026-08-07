use super::*;

/// The device-type preference order is discrete > integrated > virtual >
/// cpu/other. This is the ranking
/// `select_physical_device` uses to prefer a discrete GPU over the software
/// rasterizer when both qualify (the NVIDIA ICD added next to llvmpipe).
#[test]
fn device_preference_ranks_discrete_above_software() {
    use vk::PhysicalDeviceType as T;
    assert_eq!(
        DevicePreference::from_type(T::DISCRETE_GPU),
        DevicePreference::Discrete
    );
    assert_eq!(
        DevicePreference::from_type(T::INTEGRATED_GPU),
        DevicePreference::Integrated
    );
    assert_eq!(
        DevicePreference::from_type(T::VIRTUAL_GPU),
        DevicePreference::Virtual
    );
    assert_eq!(DevicePreference::from_type(T::CPU), DevicePreference::Cpu);
    // `OTHER` (and any unknown type) is the last resort, same rank as CPU.
    assert_eq!(DevicePreference::from_type(T::OTHER), DevicePreference::Cpu);

    // The ordering is what `select_physical_device`'s `selection.preference >
    // current.preference` comparison relies on: a discrete GPU outranks every
    // softer type, and a CPU rasterizer is never preferred over a real GPU.
    assert!(DevicePreference::Discrete > DevicePreference::Integrated);
    assert!(DevicePreference::Integrated > DevicePreference::Virtual);
    assert!(DevicePreference::Virtual > DevicePreference::Cpu);
    assert!(DevicePreference::Discrete > DevicePreference::Cpu);
}

/// The feature-probe chain creates an offscreen device regardless of which optional features
/// are present. Linux may select llvmpipe or a host GPU, while macOS selects MoltenVK; none of
/// those choices changes the optional-feature invariants. Skips when no device is obtainable.
#[test]
fn offscreen_device_probe_does_not_gate_selection() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };

    // RT and software classification are probed properties, not selection gates. The offscreen
    // device carries no surface, and an idle wait returns cleanly on every backend.
    assert!(
        device.surface().is_none(),
        "the offscreen device creates no surface"
    );
    // The indirect-build capability is meaningful only with acceleration structures.
    assert!(
        !device.capabilities.acceleration_structure_indirect_build
            || device.capabilities.rt_supported,
        "accel-struct indirect build is never reported without RT support"
    );
    device.wait_idle().expect("an idle device waits cleanly");
}

/// The three indirect-capability fields default off and never gate selection — a device lacking
/// them (llvmpipe reports them however its driver does) is still selected and used.
#[test]
fn indirect_capability_fields_default_off() {
    let caps = Capabilities::default();
    assert!(!caps.multi_draw_indirect);
    assert!(!caps.draw_indirect_count);
    assert!(!caps.acceleration_structure_indirect_build);
}

#[test]
fn capability_resolution_keeps_independent_feature_bits_and_limits() {
    let mut props = vk::PhysicalDeviceProperties::default();
    props.limits.max_draw_indirect_count = 73;
    props.limits.min_uniform_buffer_offset_alignment = 256;
    let core = vk::PhysicalDeviceFeatures::default().multi_draw_indirect(true);
    let features11 = vk::PhysicalDeviceVulkan11Features::default().shader_draw_parameters(true);
    let features12 = vk::PhysicalDeviceVulkan12Features::default()
        .buffer_device_address(true)
        .draw_indirect_count(true)
        .runtime_descriptor_array(true)
        .descriptor_binding_partially_bound(true)
        .descriptor_binding_sampled_image_update_after_bind(true)
        .shader_sampled_image_array_non_uniform_indexing(true);
    let features13 = vk::PhysicalDeviceVulkan13Features::default()
        .subgroup_size_control(true)
        .compute_full_subgroups(true);
    let descriptor = vk::PhysicalDeviceDescriptorIndexingProperties::default()
        .max_update_after_bind_descriptors_in_all_pools(50_000)
        .max_per_stage_descriptor_update_after_bind_samplers(4_096)
        .max_per_stage_descriptor_update_after_bind_sampled_images(8_192)
        .max_per_stage_update_after_bind_resources(2_048)
        .max_descriptor_set_update_after_bind_samplers(12_000)
        .max_descriptor_set_update_after_bind_sampled_images(16_384);
    let subgroup = vk::PhysicalDeviceSubgroupProperties::default()
        .subgroup_size(32)
        .supported_stages(vk::ShaderStageFlags::COMPUTE | vk::ShaderStageFlags::MESH_EXT)
        .supported_operations(vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::BALLOT)
        .quad_operations_in_all_stages(true);
    let subgroup_size = vk::PhysicalDeviceSubgroupSizeControlProperties::default()
        .min_subgroup_size(16)
        .max_subgroup_size(64)
        .max_compute_workgroup_subgroups(8)
        .required_subgroup_size_stages(vk::ShaderStageFlags::COMPUTE);
    let mesh_features = vk::PhysicalDeviceMeshShaderFeaturesEXT::default()
        .mesh_shader(true)
        .task_shader(false);
    let mesh_properties = vk::PhysicalDeviceMeshShaderPropertiesEXT::default()
        .max_mesh_work_group_count([11, 12, 13])
        .max_mesh_work_group_invocations(128)
        .max_mesh_output_vertices(256)
        .max_mesh_output_primitives(128)
        .max_task_work_group_count([21, 22, 23])
        .max_task_work_group_invocations(64)
        .max_task_payload_size(4_096);

    let capabilities = resolve_capabilities(
        &props,
        &core,
        &features11,
        &features12,
        &features13,
        &descriptor,
        &subgroup,
        &subgroup_size,
        Some((&mesh_features, &mesh_properties)),
        true,
        true,
        true,
        [128, 256, 64, 128, 256],
        true,
        8_192,
        true,
        true,
        false,
    );

    assert!(capabilities.mesh_shader);
    assert!(capabilities.opacity_micromap);
    assert!(capabilities.cluster_acceleration_structure);
    assert_eq!(capabilities.cluster_as_limits, [128, 256, 64, 128, 256]);
    assert!(capabilities.partitioned_acceleration_structure);
    assert_eq!(capabilities.max_partition_count, 8_192);
    assert!(!capabilities.task_shader);
    assert_eq!(capabilities.max_mesh_work_group_count, [11, 12, 13]);
    assert_eq!(capabilities.max_task_payload_size, 4_096);
    assert!(capabilities.buffer_device_address);
    assert!(capabilities.shader_draw_parameters);
    assert!(capabilities.runtime_descriptor_array);
    assert_eq!(capabilities.max_draw_indirect_count, 73);
    assert_eq!(capabilities.subgroup_size, 32);
    assert!(capabilities.subgroup_size_control);
    assert!(capabilities.compute_full_subgroups);
    assert_eq!(capabilities.min_subgroup_size, 16);
    assert_eq!(capabilities.max_subgroup_size, 64);
    assert_eq!(capabilities.max_bindless_array_elements, 409);
    assert_eq!(
        capabilities.max_per_stage_descriptor_update_after_bind_samplers,
        4_096
    );
    assert_eq!(
        capabilities.max_descriptor_set_update_after_bind_samplers,
        12_000
    );
    assert_eq!(
        capabilities.max_per_stage_update_after_bind_resources,
        2_048
    );
    assert_eq!(
        capabilities.max_descriptor_set_update_after_bind_sampled_images,
        16_384
    );
}

#[test]
fn profiler_normalizes_queue_timestamp_widths_and_disables_zero_bit_compute() {
    let (common, graphics, compute) = normalized_timestamp_masks(64, Some(32));
    assert_eq!(common, u64::from(u32::MAX));
    assert_eq!(graphics, u64::MAX);
    assert_eq!(compute, Some(u64::from(u32::MAX)));

    let (common, graphics, compute) = normalized_timestamp_masks(48, Some(0));
    assert_eq!(common, (1_u64 << 48) - 1);
    assert_eq!(graphics, common);
    assert_eq!(compute, None);
}

#[test]
fn rt_resolution_requires_extensions_and_both_feature_bits() {
    assert_eq!(
        resolve_rt_capabilities(true, true, false, true),
        (false, false)
    );
    assert_eq!(
        resolve_rt_capabilities(true, false, true, true),
        (false, false)
    );
    assert_eq!(
        resolve_rt_capabilities(false, true, true, true),
        (false, false)
    );
    assert_eq!(
        resolve_rt_capabilities(true, true, true, true),
        (true, true)
    );
}

fn queue_family(
    flags: vk::QueueFlags,
    queue_count: u32,
    timestamp_valid_bits: u32,
) -> vk::QueueFamilyProperties {
    vk::QueueFamilyProperties {
        queue_flags: flags,
        queue_count,
        timestamp_valid_bits,
        ..Default::default()
    }
}

#[test]
fn async_compute_prefers_a_dedicated_family() {
    let families = [
        queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
        queue_family(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER, 4, 64),
        queue_family(vk::QueueFlags::COMPUTE, 1, 64),
    ];
    assert_eq!(
        choose_async_compute_queue(&families, 0),
        Some(AsyncComputeQueue {
            family: 2,
            index: 0,
        })
    );
}

#[test]
fn async_compute_uses_a_distinct_mixed_family_when_needed() {
    let families = [
        queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
        queue_family(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER, 1, 64),
    ];
    assert_eq!(
        choose_async_compute_queue(&families, 0),
        Some(AsyncComputeQueue {
            family: 1,
            index: 0,
        })
    );
}

#[test]
fn async_compute_falls_back_when_no_compatible_family_exists() {
    let graphics = queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64);
    assert_eq!(choose_async_compute_queue(&[graphics], 0), None);

    let different_timestamp_width = [graphics, queue_family(vk::QueueFlags::COMPUTE, 1, 32)];
    assert_eq!(
        choose_async_compute_queue(&different_timestamp_width, 0),
        Some(AsyncComputeQueue {
            family: 1,
            index: 0,
        })
    );
}

#[test]
fn async_compute_prefers_a_second_graphics_family_queue_over_a_mixed_family() {
    let families = [
        queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 2, 64),
        queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
    ];
    assert_eq!(
        choose_async_compute_queue(&families, 0),
        Some(AsyncComputeQueue {
            family: 0,
            index: 1,
        })
    );
}
