use super::dispatch::write_storage;
use ash::vk;

use super::*;
use crate::Result;
use crate::descriptors::Descriptors;
use crate::device::Device;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::Buffer;

impl SceneVisibilityView {
    /// Builds lists for `capacity` instance slots. `transparent_group_capacity` sizes
    /// the sorted transparent stream: one full-length command slice per live blend
    /// bucket (the caller rebuilds the view when more blend buckets go live).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] on buffer or set allocation failure.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        visibility: &SceneVisibility,
        capacity: u32,
        record_capacity: u32,
        transparent_group_capacity: u32,
    ) -> Result<Self> {
        let capacity = capacity.max(1);
        let record_capacity = record_capacity.max(1);
        let transparent_group_capacity = transparent_group_capacity.max(1);
        let list_bytes = u64::from(capacity) * 4;
        let record_bytes = u64::from(record_capacity) * size_of::<crate::GpuDrawRecord>() as u64;
        let storage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC
            // The counters are also reached by DEVICE ADDRESS: the geometry fragment shaders
            // increment the covered-sample word through the scene address block rather than
            // through a descriptor set, so no raster pass needs a binding it otherwise would not.
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let device_local = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let history = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
        let transitions = Buffer::new(
            device.resources(),
            u64::from(SCENE_TRANSITION_STATE_CAPACITY) * 16,
            storage,
            &device_local,
        )?;
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let counters = Buffer::new(
                device.resources(),
                SCENE_VISIBILITY_COUNTER_WORDS * 4,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let readback = Buffer::new(
                device.resources(),
                SCENE_VISIBILITY_COUNTER_WORDS * 4,
                vk::BufferUsageFlags::TRANSFER_DST,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            // SAFETY: HOST_VISIBLE + MAPPED, zeroed before any read.
            unsafe {
                std::ptr::write_bytes(readback.mapped_ptr(), 0, readback.size() as usize);
            }
            let visible = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
            let retest = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
            let records = Buffer::new(device.resources(), record_bytes, storage, &device_local)?;
            let bin_bytes = u64::from(SCENE_EXECUTOR_BUCKET_CAPACITY) * 4;
            let bin_counts = Buffer::new(
                device.resources(),
                bin_bytes,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let bin_cursors = Buffer::new(device.resources(), bin_bytes, storage, &device_local)?;
            let bucket_table = Buffer::new(
                device.resources(),
                (16 + 512 * 8 + 512 * 4 + 512 * 4) as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            // The table must never be read uninitialized: a garbage liveCount would
            // send the kernels' bucket indices out of bounds.
            // SAFETY: HOST_VISIBLE + MAPPED, zeroed before any submit reads it.
            unsafe {
                std::ptr::write_bytes(bucket_table.mapped_ptr(), 0, bucket_table.size() as usize);
            }
            let command_slots =
                u64::from(record_capacity) * u64::from(1 + transparent_group_capacity);
            let commands = Buffer::new(
                device.resources(),
                command_slots * 20,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            // One `VkDrawMeshTasksIndirectCommandEXT` (three u32s) per draw slot, parallel to
            // `commands`. The kernel that fills a slot knows that draw's triangle count, so it
            // writes the group count directly and no conversion pass is needed.
            let mesh_args = Buffer::new(
                device.resources(),
                command_slots * MESH_TASK_COMMAND_STRIDE,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let workgroups = record_capacity.div_ceil(SCENE_RADIX_WORKGROUP);
            let pairs = [
                Buffer::new(
                    device.resources(),
                    u64::from(record_capacity) * 8,
                    storage,
                    &device_local,
                )?,
                Buffer::new(
                    device.resources(),
                    u64::from(record_capacity) * 8,
                    storage,
                    &device_local,
                )?,
            ];
            let histograms = Buffer::new(
                device.resources(),
                u64::from(workgroups) * 256 * 4,
                storage,
                &device_local,
            )?;
            let micro_scratch = Buffer::new(
                device.resources(),
                SCENE_MICRO_SCRATCH_WORDS * 4,
                storage,
                &device_local,
            )?;
            let cull_set = descriptors.allocate_set(visibility.layout)?;
            let retest_set = descriptors.allocate_set(visibility.layout)?;
            let traversal_set = descriptors.allocate_set(visibility.traversal_layout)?;
            let bin_count_set = descriptors.allocate_set(visibility.bin_count_layout)?;
            let bin_seed_set = descriptors.allocate_set(visibility.bin_seed_layout)?;
            let bin_scatter_set = descriptors.allocate_set(visibility.bin_scatter_layout)?;
            let micro_set = descriptors.allocate_set(visibility.micro_layout)?;
            let raw = device.raw();
            for set in [cull_set, retest_set] {
                write_storage(raw, set, 0, &counters);
                write_storage(raw, set, 1, &visible);
                write_storage(raw, set, 2, &retest);
                write_storage(raw, set, 3, &history);
            }
            write_storage(raw, traversal_set, 0, &counters);
            write_storage(raw, traversal_set, 1, &visible);
            write_storage(raw, traversal_set, 2, &records);
            write_storage(raw, traversal_set, 4, &transitions);
            write_storage(raw, bin_count_set, 0, &counters);
            write_storage(raw, bin_count_set, 1, &records);
            write_storage(raw, bin_count_set, 2, &bin_counts);
            write_storage(raw, bin_count_set, 3, &bucket_table);
            write_storage(raw, bin_seed_set, 0, &bucket_table);
            write_storage(raw, bin_seed_set, 1, &bin_cursors);
            write_storage(raw, bin_scatter_set, 0, &counters);
            write_storage(raw, bin_scatter_set, 1, &records);
            write_storage(raw, bin_scatter_set, 2, &bin_cursors);
            write_storage(raw, bin_scatter_set, 3, &commands);
            write_storage(raw, bin_scatter_set, 5, &bucket_table);
            write_storage(raw, bin_scatter_set, 6, &mesh_args);
            write_storage(raw, micro_set, 0, &counters);
            write_storage(raw, micro_set, 1, &records);
            write_storage(raw, micro_set, 3, &micro_scratch);
            let transparent_keys_set =
                descriptors.allocate_set(visibility.transparent_keys_layout)?;
            let radix_histogram_sets = [
                descriptors.allocate_set(visibility.radix_histogram_layout)?,
                descriptors.allocate_set(visibility.radix_histogram_layout)?,
            ];
            let radix_scan_set = descriptors.allocate_set(visibility.radix_scan_layout)?;
            let radix_scatter_sets = [
                descriptors.allocate_set(visibility.radix_scatter_layout)?,
                descriptors.allocate_set(visibility.radix_scatter_layout)?,
            ];
            let transparent_reorder_set =
                descriptors.allocate_set(visibility.transparent_reorder_layout)?;
            write_storage(raw, transparent_keys_set, 0, &counters);
            write_storage(raw, transparent_keys_set, 1, &records);
            write_storage(raw, transparent_keys_set, 2, &pairs[0]);
            for (direction, set) in radix_histogram_sets.iter().enumerate() {
                write_storage(raw, *set, 0, &counters);
                write_storage(raw, *set, 1, &pairs[direction]);
                write_storage(raw, *set, 2, &histograms);
            }
            write_storage(raw, radix_scan_set, 0, &histograms);
            for (direction, set) in radix_scatter_sets.iter().enumerate() {
                write_storage(raw, *set, 0, &counters);
                write_storage(raw, *set, 1, &pairs[direction]);
                write_storage(raw, *set, 2, &pairs[direction ^ 1]);
                write_storage(raw, *set, 3, &histograms);
            }
            write_storage(raw, transparent_reorder_set, 0, &counters);
            write_storage(raw, transparent_reorder_set, 1, &pairs[0]);
            write_storage(raw, transparent_reorder_set, 2, &records);
            write_storage(raw, transparent_reorder_set, 3, &commands);
            write_storage(raw, transparent_reorder_set, 5, &mesh_args);
            frames.push(VisibilityFrame {
                counters,
                readback,
                visible,
                retest,
                records,
                bin_counts,
                bin_cursors,
                bucket_table,
                commands,
                mesh_args,
                pairs,
                histograms,
                micro_scratch,
                cull_set,
                retest_set,
                traversal_set,
                bin_count_set,
                bin_seed_set,
                bin_scatter_set,
                micro_set,
                transparent_keys_set,
                radix_histogram_sets,
                radix_scan_set,
                radix_scatter_sets,
                transparent_reorder_set,
            });
        }
        Ok(Self {
            frames,
            history,
            transitions,
            transitions_cleared: std::sync::atomic::AtomicBool::new(false),
            capacity,
            record_capacity,
            transparent_group_capacity,
        })
    }

    /// Element capacity of the semantic record stream.
    pub fn record_capacity(&self) -> u32 {
        self.record_capacity
    }

    /// Allocated blend-bucket slices in the sorted transparent stream.
    pub fn transparent_group_capacity(&self) -> u32 {
        self.transparent_group_capacity
    }

    /// Draw slots in the executor command arena: the binner's per-bucket region plus one
    /// full-length sorted slice per allocated blend bucket. The mesh executor reads the whole
    /// arena as data, so this is the range its descriptor binding must span.
    pub fn command_slots(&self) -> u32 {
        self.record_capacity * (1 + self.transparent_group_capacity)
    }

    /// The frame slot's semantic record stream.
    pub fn records(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].records.handle()
    }

    /// Element capacity of the lists.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The cross-frame per-slot history buffer (cleared on history invalidation).
    pub fn history(&self) -> vk::Buffer {
        self.history.handle()
    }

    /// The frame slot's counters buffer (visible / retest / overflow words).
    pub fn counters(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].counters.handle()
    }

    /// The frame slot's cull descriptor set, for a pass that reads only the GPU-scene address
    /// block this set already carries rather than owning a second set for the same block.
    pub fn cull_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].cull_set
    }

    /// The frame slot's counters buffer as a device address, for shaders with no binding for it.
    pub fn counters_address(&self, device: &Device, frame: usize) -> u64 {
        device.buffer_device_address(self.frames[frame].counters.handle())
    }

    /// The frame slot's visible slot list.
    pub fn visible(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].visible.handle()
    }

    /// The frame slot's retest slot list.
    pub fn retest(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].retest.handle()
    }

    /// The frame slot's micro-field pass set (counters, records, the address block, scratch).
    pub fn micro_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].micro_set
    }

    /// Returns the sets to the pool before replacement (capacity growth under an idle
    /// wait).
    pub fn free_sets(&mut self, descriptors: &Descriptors) {
        for frame in &mut self.frames {
            descriptors.free_sets(&[
                frame.cull_set,
                frame.retest_set,
                frame.traversal_set,
                frame.bin_count_set,
                frame.bin_seed_set,
                frame.bin_scatter_set,
                frame.transparent_keys_set,
                frame.radix_histogram_sets[0],
                frame.radix_histogram_sets[1],
                frame.radix_scan_set,
                frame.radix_scatter_sets[0],
                frame.radix_scatter_sets[1],
                frame.transparent_reorder_set,
            ]);
            frame.cull_set = vk::DescriptorSet::null();
            frame.retest_set = vk::DescriptorSet::null();
            frame.traversal_set = vk::DescriptorSet::null();
            frame.bin_count_set = vk::DescriptorSet::null();
            frame.bin_seed_set = vk::DescriptorSet::null();
            frame.bin_scatter_set = vk::DescriptorSet::null();
        }
    }

    /// Writes the frame slot's per-frame bindings: the HZB pyramids (previous for the
    /// cull set, current for the retest set) and the frame's address-block slice.
    pub fn write_frame_bindings(
        &self,
        device: &Device,
        visibility: &SceneVisibility,
        frame: usize,
        previous_hzb: vk::ImageView,
        current_hzb: vk::ImageView,
        address_block: (vk::Buffer, u64, u64),
    ) {
        let raw = device.raw();
        let slot = &self.frames[frame];
        for (set, binding) in [
            (slot.traversal_set, 3),
            (slot.bin_scatter_set, 4),
            (slot.micro_set, 2),
            (slot.transparent_keys_set, 3),
            (slot.transparent_reorder_set, 4),
        ] {
            let buffer = [vk::DescriptorBufferInfo {
                buffer: address_block.0,
                offset: address_block.1,
                range: address_block.2,
            }];
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(binding)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&buffer);
            // SAFETY: the ash seam. Written at the fence-waited frame build point.
            unsafe { raw.update_descriptor_sets(&[write], &[]) };
        }
        for (set, view) in [
            (slot.cull_set, previous_hzb),
            (slot.retest_set, current_hzb),
        ] {
            let image = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::GENERAL)
                .sampler(visibility.sampler)];
            let buffer = [vk::DescriptorBufferInfo {
                buffer: address_block.0,
                offset: address_block.1,
                range: address_block.2,
            }];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(4)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&image),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(5)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(&buffer),
            ];
            // SAFETY: the ash seam. Written at the fence-waited frame build point.
            unsafe { raw.update_descriptor_sets(&writes, &[]) };
        }
    }
}
