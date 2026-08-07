//! Wiring one skin dispatch: the per-instance descriptor-set writes, the grow-only deformed
//! buffer, and the set layout / pool they come from.

use super::*;

/// Allocates one skin descriptor set from `pool` and writes its four storage-buffer
/// bindings (static vertices, skin, palette, deformed output). Returns `None` on an
/// allocation failure (logged), so the caller drops the dispatches.
#[allow(clippy::too_many_arguments)]
pub fn wire_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    mesh: &GpuMesh,
    palette: vk::Buffer,
    palette_size: vk::DeviceSize,
    deformed: vk::Buffer,
    deformed_size: vk::DeviceSize,
) -> Option<vk::DescriptorSet> {
    let skin = mesh.skin_buffer()?;
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is
    // reset (next frame) or destroyed.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("skinning: allocate skin set failed: {result:?}");
            return None;
        }
    };
    let infos = [
        vk::DescriptorBufferInfo {
            buffer: mesh.vertex_buffer(),
            offset: 0,
            range: vk::WHOLE_SIZE,
        },
        vk::DescriptorBufferInfo {
            buffer: skin,
            offset: 0,
            range: vk::WHOLE_SIZE,
        },
        vk::DescriptorBufferInfo {
            buffer: palette,
            offset: 0,
            range: palette_size,
        },
        vk::DescriptorBufferInfo {
            buffer: deformed,
            offset: 0,
            range: deformed_size,
        },
    ];
    let writes: Vec<vk::WriteDescriptorSet> = (0..4)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets a
    // single binding the layout declares.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// Grows `current` (a [`Vertex`]-element capacity) to the next power of two that holds
/// `count`, seeding from the initial capacity when empty and never shrinking.
pub fn grow_capacity(current: u32, count: u32) -> u32 {
    let mut capacity = if current == 0 {
        INITIAL_DEFORMED_CAPACITY
    } else {
        current
    };
    while capacity < count {
        capacity *= 2;
    }
    capacity
}

/// Allocates a device-local deformed-vertex buffer of `capacity` [`Vertex`] elements with
/// `STORAGE|VERTEX` usage. When `rt_supported`, the buffer also feeds the per-frame
/// skinned BLAS refit, so it adds shader-device-address + AS-build-input usage.
pub(super) fn make_deformed_buffer(
    resources: &Arc<DeviceResources>,
    capacity: u32,
    rt_supported: bool,
) -> Result<Buffer> {
    let size = u64::from(capacity) * size_of::<Vertex>() as u64;
    // SHADER_DEVICE_ADDRESS always: the frame's GPU-scene address block carries the
    // buffer for the executor vertex pull.
    let mut usage = vk::BufferUsageFlags::STORAGE_BUFFER
        | vk::BufferUsageFlags::VERTEX_BUFFER
        | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
    if rt_supported {
        usage |= vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
    }
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(resources, size, usage, &alloc_info)
}

/// The skin set layout: four compute-stage storage buffers (static vertices, skin,
/// palette, deformed output) matching `skin.slang`'s bindings 0-3.
pub(super) fn create_skin_set_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..4)
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "skinSetLayout",
    )
}

/// Creates a per-frame skin descriptor pool sized for [`SKIN_POOL_SET_CAPACITY`] sets,
/// each with four storage buffers.
pub(super) fn create_skin_pool(raw: &ash::Device) -> Result<vk::DescriptorPool> {
    // Each set holds up to 6 storage buffers (a morph set; a skin set uses 4) out of the
    // shared cur+prev budget, so the descriptor count covers the morph worst case.
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(SKIN_POOL_SET_CAPACITY * 6)];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(SKIN_POOL_SET_CAPACITY)
        .pool_sizes(&sizes);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "skinPool",
    )
}

/// Clamps the parallel dispatch / prev-dispatch / bucket / RT lists to
/// [`SKIN_MAX_SETS_PER_FRAME`], logging the overflow (a clamp, not an error). Returns the
/// retained count.
pub fn clamp_to_set_budget(count: usize) -> usize {
    if count > SKIN_MAX_SETS_PER_FRAME as usize {
        tracing::warn!(
            "skinning: {count} skinned instances exceed the {SKIN_MAX_SETS_PER_FRAME}-set frame budget; clamping"
        );
        SKIN_MAX_SETS_PER_FRAME as usize
    } else {
        count
    }
}

/// Requests the `skin` compute PSO from `pipelines`, returning `None` on a build failure
/// (logged). The PSO binds the skin set layout owned by [`Skinning`] and a 16-byte push.
pub fn request_skin_pipeline(
    pipelines: &mut Pipelines,
    skinning: &Skinning,
) -> Option<Arc<crate::Pipeline>> {
    pipelines.request_skin(skinning.set_layout)
}

/// Requests the `morph` compute PSO from `pipelines`, binding the morph set layout owned by
/// [`Skinning`] and a 20-byte push.
pub fn request_morph_pipeline(
    pipelines: &mut Pipelines,
    skinning: &Skinning,
) -> Option<Arc<crate::Pipeline>> {
    pipelines.request_morph(skinning.morph_set_layout)
}
