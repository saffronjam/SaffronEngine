//! The partitioned top-level acceleration structure.
//!
//! `VK_NV_partitioned_acceleration_structure` builds a top-level structure whose instances live in
//! partitions, driven by an op stream that names only what changed, so a frame costs the instances
//! it touched rather than the instances that exist.
//!
//! Partitions are the world's own base cells ([`saffron_spatial::BASE_CELL_EDGE_METERS`]), hashed
//! into the partition table, because that is the granularity content changes at. An instance with
//! no fixed home — a deforming one, refit every frame anyway — goes in the global partition.
//!
//! Instance indices are stable across frames, which is what makes the diff expressible: each placed
//! instance owns a slot keyed by its scene identity, so a frame emits a write only for instances
//! that appeared or moved, and a cheaper update for one whose structure address changed under an
//! unmoved transform.
//!
//! Only this module, `device.rs`, and `descriptors.rs` may import the transcribed
//! [`crate::vk_nv_ptlas`] bindings.

use std::collections::BTreeMap;

use ash::vk;

use crate::vk_nv_ptlas as nvx;
use crate::{Buffer, DeviceResources, Result};

/// Partitions the instance table is spread across. The world is unbounded and the partition
/// index is not, so cells hash into a fixed table: a collision merges two cells into one
/// partition, which costs granularity and never correctness.
const PARTITION_COUNT: u32 = 256;

/// Instance slots the structure is sized for initially, doubled on demand.
const INITIAL_INSTANCE_CAPACITY: u32 = 256;

/// The stable identity of one placed instance across frames — a GPU-scene slot or entity,
/// plus the sub-placement within it (an assembly use, or zero).
///
/// Stability is the whole basis of the diff: without it every frame would renumber the
/// table and every instance would look new.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PtlasKey {
    pub primary: u64,
    pub sub: u32,
}

/// One instance as the caller describes it, before slot assignment.
pub struct PtlasInstance {
    pub key: PtlasKey,
    /// Row-major 3×4 world transform, the same packing the KHR instance uses.
    pub transform: [f32; 12],
    pub mask: u32,
    pub flags: u32,
    /// The bottom-level structure this instance places. Held rather than reduced to its
    /// address because the structure must outlive every frame the instance stays placed —
    /// an instance the diff leaves alone is never rewritten, so nothing else would keep it.
    pub blas: crate::RtBlas,
    /// The partition this instance belongs to, from [`partition_for_translation`] or
    /// [`nvx::PARTITION_INDEX_GLOBAL`].
    pub partition: u32,
}

/// The partition for an instance with no fixed cell — a deforming one, rewritten every
/// frame regardless.
pub const GLOBAL_PARTITION: u32 = nvx::PARTITION_INDEX_GLOBAL;

/// The partitioned instance flags matching a KHR instance's.
///
/// The low four bits carry the same meanings in both enumerations. The KHR-only
/// micromap-disable bit has no partitioned counterpart, and needs none: it accompanies a
/// forced opacity, and a forced opacity already overrides whatever a micromap would have
/// resolved.
#[must_use]
pub fn instance_flags(khr: vk::GeometryInstanceFlagsKHR) -> u32 {
    khr.as_raw() & nvx::INSTANCE_FLAGS_CARRIED
}

/// The base cell a world position falls in, hashed into the partition table.
#[must_use]
pub fn partition_for_translation(translation: [f32; 3]) -> u32 {
    let edge = saffron_spatial::BASE_CELL_EDGE_METERS as f32;
    let cell = translation.map(|axis| (axis / edge).floor() as i64);
    // A 64-bit mix over the three cell coordinates, finalized so adjacent cells land in
    // different partitions: only the spread matters, since a collision merges partitions
    // rather than misplacing an instance, but a mix that leaves neighbours together would
    // hand the whole camera view to one partition and undo the point of partitioning.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for coordinate in cell {
        hash ^= coordinate as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= hash >> 29;
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    (hash as u32) % PARTITION_COUNT
}

/// What one frame's build did, for telemetry: the placed instances and the ops it took to
/// reach them. `writes + updates` far below `instances` is the incrementality working.
#[derive(Clone, Copy, Default)]
pub struct PtlasStats {
    pub instances: u32,
    pub partitions: u32,
    pub writes: u32,
    pub updates: u32,
    /// Whether this frame rebuilt from nothing rather than from the previous structure.
    pub full_rebuild: bool,
}

/// The device-side buffers one frame slot owns.
struct PtlasFrameBuffers {
    data: Buffer,
    address: vk::DeviceAddress,
    scratch: Buffer,
    scratch_address: vk::DeviceAddress,
    write_args: Buffer,
    write_args_address: vk::DeviceAddress,
    update_args: Buffer,
    update_args_address: vk::DeviceAddress,
    ops: Buffer,
    ops_address: vk::DeviceAddress,
    ops_count: Buffer,
    ops_count_address: vk::DeviceAddress,
}

/// The recorded build: everything `vkCmdBuildPartitionedAccelerationStructuresNV` needs,
/// resolved so recording issues one call through live addresses.
pub struct PtlasBuildOp {
    dispatch: nvx::Dispatch,
    info: nvx::BuildInfoNV,
}

// SAFETY: the info holds device addresses and plain data, and the dispatch is a Clone
// fn-pointer table; the plan crosses into the `'static` graph closure like the KHR one.
unsafe impl Send for PtlasBuildOp {}

impl PtlasBuildOp {
    /// Issues the build.
    ///
    /// # Safety
    ///
    /// The command buffer must be recording, and every address in the plan must still
    /// reference the live per-frame buffers it was resolved from.
    pub unsafe fn record(&self, cmd: vk::CommandBuffer) {
        // SAFETY: forwarded to the caller's contract.
        unsafe { self.dispatch.cmd_build(cmd, &self.info) };
    }
}

/// The persistent partitioned structure: the slot map, the previous frame's placed state,
/// and one buffer set per frame in flight.
pub struct Ptlas {
    dispatch: nvx::Dispatch,
    partition_count: u32,
    slots: BTreeMap<PtlasKey, u32>,
    /// The placed state of every slot as the newest structure holds it; `None` is a free
    /// slot. This is the diff's left-hand side, so it must track what was actually built.
    placed: Vec<Option<nvx::WriteInstanceDataNV>>,
    /// The structure each placed slot references, held for exactly as long as the slot
    /// does. The KHR path retains per frame because it rewrites every instance per frame;
    /// a partitioned structure keeps instances across frames, so retention has to follow
    /// the slot rather than the frame.
    retained: Vec<Option<crate::RtBlas>>,
    free: Vec<u32>,
    frames: Vec<PtlasFrameBuffers>,
    /// The frame slot holding the newest structure, and so the source of the next build.
    /// `None` until the first build lands, which forces a full rebuild.
    newest: Option<usize>,
    instance_capacity: u32,
    stats: PtlasStats,
}

impl Ptlas {
    /// `None` on a device without the extension.
    pub fn new(
        device: &crate::Device,
        resources: &std::sync::Arc<DeviceResources>,
    ) -> Option<Self> {
        let dispatch = device.ptlas_dispatch()?.clone();
        let partition_count = PARTITION_COUNT.min(device.max_partition_count().max(1));
        let mut ptlas = Self {
            dispatch,
            partition_count,
            slots: BTreeMap::new(),
            placed: Vec::new(),
            retained: Vec::new(),
            free: Vec::new(),
            frames: Vec::new(),
            newest: None,
            instance_capacity: 0,
            stats: PtlasStats::default(),
        };
        match ptlas.grow(resources, INITIAL_INSTANCE_CAPACITY) {
            Ok(()) => Some(ptlas),
            Err(err) => {
                tracing::error!("ptlas: initial allocation failed: {err}");
                None
            }
        }
    }

    /// This frame's build statistics.
    #[must_use]
    pub fn stats(&self) -> PtlasStats {
        self.stats
    }

    /// The device address of the newest structure — what set 6 binds. `None` before the
    /// first build.
    #[must_use]
    pub fn current_address(&self) -> Option<vk::DeviceAddress> {
        self.newest.map(|slot| self.frames[slot].address)
    }

    /// The device address of `frame`'s structure, whether or not it is the newest — what
    /// the seed writes into each frame's set 6 before any build has run.
    #[must_use]
    pub fn frame_address(&self, frame: usize) -> Option<vk::DeviceAddress> {
        self.frames.get(frame).map(|buffers| buffers.address)
    }

    /// AS-storage bytes one frame's structure occupies.
    #[must_use]
    pub fn structure_bytes(&self) -> u64 {
        self.frames.first().map_or(0, |buffers| buffers.data.size())
    }

    /// Build-scratch bytes one frame's build holds.
    #[must_use]
    pub fn scratch_bytes(&self) -> u64 {
        self.frames
            .first()
            .map_or(0, |buffers| buffers.scratch.size())
    }

    /// The sizing input every call shares, so a size query and a build cannot disagree.
    fn input(&self, instance_count: u32) -> nvx::InstancesInputNV {
        nvx::InstancesInputNV {
            flags: vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE,
            instance_count,
            // The partition table is spatial, so a worst-case scene could put every
            // instance in one cell; sizing for that is what keeps a build from failing on
            // content rather than on capacity.
            max_instance_per_partition_count: instance_count,
            partition_count: self.partition_count,
            max_instance_in_global_partition_count: instance_count,
            ..Default::default()
        }
    }

    /// Allocates one buffer set per frame in flight for `capacity` instances, discarding
    /// what was there. Any resize invalidates the structures, so the next build is full.
    fn grow(&mut self, resources: &std::sync::Arc<DeviceResources>, capacity: u32) -> Result<()> {
        let sizes = self.dispatch.get_build_sizes(&self.input(capacity));
        let storage_usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        // Acceleration-structure storage takes a dedicated allocation: sharing a memory
        // block with ordinary buffers wedges the GPU on a later unrelated submission.
        let dedicated = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        let device_local = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let host_input = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let input_usage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
        let mut frames = Vec::with_capacity(crate::MAX_FRAMES_IN_FLIGHT);
        for _ in 0..crate::MAX_FRAMES_IN_FLIGHT {
            let data = Buffer::with_alignment(
                resources,
                sizes.acceleration_structure_size.max(256),
                storage_usage,
                &dedicated,
                256,
            )?;
            let scratch = Buffer::with_alignment(
                resources,
                sizes.build_scratch_size.max(256),
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &device_local,
                resources.scratch_alignment(),
            )?;
            let write_args = Buffer::with_alignment(
                resources,
                u64::from(capacity) * std::mem::size_of::<nvx::WriteInstanceDataNV>() as u64,
                input_usage,
                &host_input,
                256,
            )?;
            let update_args = Buffer::with_alignment(
                resources,
                u64::from(capacity) * std::mem::size_of::<nvx::UpdateInstanceDataNV>() as u64,
                input_usage,
                &host_input,
                256,
            )?;
            let ops = Buffer::with_alignment(
                resources,
                2 * std::mem::size_of::<nvx::IndirectCommandNV>() as u64,
                input_usage,
                &host_input,
                256,
            )?;
            let ops_count = Buffer::with_alignment(resources, 4, input_usage, &host_input, 256)?;
            frames.push(PtlasFrameBuffers {
                address: resources.buffer_device_address(data.handle()),
                scratch_address: resources.buffer_device_address(scratch.handle()),
                write_args_address: resources.buffer_device_address(write_args.handle()),
                update_args_address: resources.buffer_device_address(update_args.handle()),
                ops_address: resources.buffer_device_address(ops.handle()),
                ops_count_address: resources.buffer_device_address(ops_count.handle()),
                data,
                scratch,
                write_args,
                update_args,
                ops,
                ops_count,
            });
        }
        self.frames = frames;
        self.instance_capacity = capacity;
        // The old structures are gone, so nothing can be a source and no slot's placed
        // state describes device memory any more.
        self.newest = None;
        self.placed.iter_mut().for_each(|slot| *slot = None);
        self.retained.iter_mut().for_each(|slot| *slot = None);
        Ok(())
    }

    /// Assigns `key` a stable slot, reusing a freed one before extending the table.
    fn slot_for(&mut self, key: PtlasKey) -> u32 {
        if let Some(index) = self.slots.get(&key) {
            return *index;
        }
        let index = self.free.pop().unwrap_or_else(|| {
            let index = self.placed.len() as u32;
            self.placed.push(None);
            self.retained.push(None);
            index
        });
        self.slots.insert(key, index);
        index
    }

    /// Plans one frame slot's seed build: a single inert instance, from no source.
    ///
    /// Set 6 binds a structure before any scene exists — the mesh fragment statically binds
    /// it whether or not the ray-query flag is on — so every slot must hold a structure that
    /// is defined rather than merely allocated. One written instance with a zero mask is the
    /// smallest such structure; an unwritten one would leave the slot's contents undefined.
    pub fn plan_seed(&mut self, frame: usize) -> Option<PtlasBuildOp> {
        let inert = nvx::WriteInstanceDataNV {
            transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            explicit_aabb: [0.0; 6],
            instance_id: 0,
            instance_mask: 0,
            instance_contribution_to_hit_group_index: 0,
            instance_flags: 0,
            instance_index: 0,
            partition_index: 0,
            acceleration_structure: 0,
        };
        let buffers = self.frames.get_mut(frame)?;
        write_slice(&mut buffers.write_args, std::slice::from_ref(&inert));
        let ops = [nvx::IndirectCommandNV {
            op_type: nvx::OP_TYPE_WRITE_INSTANCE,
            arg_count: 1,
            arg_data: nvx::StridedDeviceAddressNV {
                start_address: buffers.write_args_address,
                stride_in_bytes: std::mem::size_of::<nvx::WriteInstanceDataNV>() as u64,
            },
        }];
        write_slice(&mut buffers.ops, &ops);
        write_slice(&mut buffers.ops_count, &[1_u32]);
        let info = nvx::BuildInfoNV {
            s_type: nvx::STRUCTURE_TYPE_BUILD_PARTITIONED_ACCELERATION_STRUCTURE_INFO_NV,
            p_next: std::ptr::null_mut(),
            input: self.input(1),
            src_acceleration_structure_data: 0,
            dst_acceleration_structure_data: self.frames[frame].address,
            scratch_data: self.frames[frame].scratch_address,
            src_infos: self.frames[frame].ops_address,
            src_infos_count: self.frames[frame].ops_count_address,
        };
        Some(PtlasBuildOp {
            dispatch: self.dispatch.clone(),
            info,
        })
    }

    /// Frame slots the structure spans, so the seed can cover every one.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Diffs `instances` against the newest structure and plans the build that reaches
    /// them. Returns `None` when the plan cannot be prepared, leaving the structure and the
    /// tracked state untouched.
    ///
    /// The slot each input landed in comes back with the build, in input order: it is the
    /// instance id a candidate resolves through, so the caller writes its resolution record
    /// at that index rather than at the instance's position in the list.
    pub fn plan(
        &mut self,
        resources: &std::sync::Arc<DeviceResources>,
        frame: usize,
        instances: &[PtlasInstance],
    ) -> Option<(PtlasBuildOp, Vec<u32>)> {
        let needed = u32::try_from(instances.len()).ok()?;
        if needed > self.instance_capacity {
            let mut capacity = self.instance_capacity.max(INITIAL_INSTANCE_CAPACITY);
            while capacity < needed {
                capacity = capacity.saturating_mul(2);
            }
            if let Err(err) = self.grow(resources, capacity) {
                tracing::error!("ptlas: grow to {capacity} instances failed: {err}");
                return None;
            }
        }
        // A frame slot's own buffers are reused as this build's destination, so the source
        // must be the OTHER slot — the one the previous frame wrote. With the frame ring's
        // fence already waited, that makes the destination free and the source current.
        let source = self.newest.filter(|slot| *slot != frame);
        let full_rebuild = source.is_none();
        if full_rebuild {
            // Nothing survives a rebuild from no source, so the table renumbers densely:
            // every slot is written this build, and no hole can be left undefined.
            self.slots.clear();
            self.placed.clear();
            self.retained.clear();
            self.free.clear();
        }

        let mut writes: Vec<nvx::WriteInstanceDataNV> = Vec::new();
        let mut updates: Vec<nvx::UpdateInstanceDataNV> = Vec::new();
        let mut seen: BTreeMap<PtlasKey, u32> = BTreeMap::new();
        let mut partitions: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        let mut assigned: Vec<u32> = Vec::with_capacity(instances.len());
        for instance in instances {
            let index = self.slot_for(instance.key);
            assigned.push(index);
            seen.insert(instance.key, index);
            partitions.insert(instance.partition);
            let desired = nvx::WriteInstanceDataNV {
                transform: instance.transform,
                explicit_aabb: [0.0; 6],
                // The candidate-resolution index is the structure's own slot, not the
                // instance's position in this frame's list: a positional id changes for
                // every instance after an insertion, and the diff would then rewrite the
                // tail of the table for a scene that gained one object.
                instance_id: index,
                instance_mask: instance.mask,
                instance_contribution_to_hit_group_index: 0,
                instance_flags: instance.flags,
                instance_index: index,
                partition_index: instance.partition,
                acceleration_structure: instance.blas.address(),
            };
            match self
                .placed
                .get(index as usize)
                .and_then(|slot| slot.as_ref())
            {
                Some(current) if *current == desired => {}
                // Only the structure address moved under an unchanged placement — the
                // cheaper op, and the common one for a plant whose representation swapped
                // between its fine expansion and its aggregate.
                Some(current)
                    if nvx::WriteInstanceDataNV {
                        acceleration_structure: desired.acceleration_structure,
                        ..*current
                    } == desired =>
                {
                    updates.push(nvx::UpdateInstanceDataNV {
                        instance_index: index,
                        instance_contribution_to_hit_group_index: 0,
                        acceleration_structure: desired.acceleration_structure,
                    });
                    self.placed[index as usize] = Some(desired);
                    self.retained[index as usize] = Some(instance.blas.clone());
                }
                _ => {
                    writes.push(desired);
                    self.placed[index as usize] = Some(desired);
                    self.retained[index as usize] = Some(instance.blas.clone());
                }
            }
        }

        // Instances that left the scene keep their slot in the table until something
        // reclaims it, so they are written inert rather than left tracing: a zero mask
        // matches no ray, and the slot returns to the free list.
        let departed: Vec<PtlasKey> = self
            .slots
            .keys()
            .filter(|key| !seen.contains_key(key))
            .copied()
            .collect();
        for key in departed {
            let Some(index) = self.slots.remove(&key) else {
                continue;
            };
            if let Some(Some(current)) = self.placed.get(index as usize).cloned() {
                let inert = nvx::WriteInstanceDataNV {
                    instance_mask: 0,
                    acceleration_structure: 0,
                    ..current
                };
                writes.push(inert);
            }
            if let Some(slot) = self.placed.get_mut(index as usize) {
                *slot = None;
            }
            // The inert write lands this frame, so the structure stops referencing it and
            // the last hold on it can go.
            if let Some(slot) = self.retained.get_mut(index as usize) {
                *slot = None;
            }
            self.free.push(index);
        }

        let instance_count = self.placed.len() as u32;
        if instance_count > self.instance_capacity {
            // The high-water table outgrew the structure even though the live set did not.
            if let Err(err) = self.grow(resources, instance_count.next_power_of_two()) {
                tracing::error!("ptlas: grow to {instance_count} slots failed: {err}");
                return None;
            }
            return None;
        }

        self.stats = PtlasStats {
            instances: seen.len() as u32,
            partitions: partitions.len() as u32,
            writes: writes.len() as u32,
            updates: updates.len() as u32,
            full_rebuild,
        };

        let buffers = &mut self.frames[frame];
        write_slice(&mut buffers.write_args, &writes);
        write_slice(&mut buffers.update_args, &updates);
        let mut ops: Vec<nvx::IndirectCommandNV> = Vec::with_capacity(2);
        if !writes.is_empty() {
            ops.push(nvx::IndirectCommandNV {
                op_type: nvx::OP_TYPE_WRITE_INSTANCE,
                arg_count: writes.len() as u32,
                arg_data: nvx::StridedDeviceAddressNV {
                    start_address: buffers.write_args_address,
                    stride_in_bytes: std::mem::size_of::<nvx::WriteInstanceDataNV>() as u64,
                },
            });
        }
        if !updates.is_empty() {
            ops.push(nvx::IndirectCommandNV {
                op_type: nvx::OP_TYPE_UPDATE_INSTANCE,
                arg_count: updates.len() as u32,
                arg_data: nvx::StridedDeviceAddressNV {
                    start_address: buffers.update_args_address,
                    stride_in_bytes: std::mem::size_of::<nvx::UpdateInstanceDataNV>() as u64,
                },
            });
        }
        let op_count = ops.len() as u32;
        write_slice(&mut buffers.ops, &ops);
        write_slice(&mut buffers.ops_count, std::slice::from_ref(&op_count));

        let info = nvx::BuildInfoNV {
            s_type: nvx::STRUCTURE_TYPE_BUILD_PARTITIONED_ACCELERATION_STRUCTURE_INFO_NV,
            p_next: std::ptr::null_mut(),
            input: self.input(instance_count.max(1)),
            src_acceleration_structure_data: source.map_or(0, |slot| self.frames[slot].address),
            dst_acceleration_structure_data: self.frames[frame].address,
            scratch_data: self.frames[frame].scratch_address,
            src_infos: self.frames[frame].ops_address,
            src_infos_count: self.frames[frame].ops_count_address,
        };
        self.newest = Some(frame);
        Some((
            PtlasBuildOp {
                dispatch: self.dispatch.clone(),
                info,
            },
            assigned,
        ))
    }

    /// Slots the table spans, live and free alike — the bound a slot-indexed side table
    /// must cover, since a reused slot keeps its index rather than compacting.
    #[must_use]
    pub fn slot_count(&self) -> u32 {
        self.placed.len() as u32
    }
}

/// Copies `values` into a persistently mapped buffer.
fn write_slice<T: Copy>(buffer: &mut Buffer, values: &[T]) {
    let Some(bytes) = buffer.mapped_bytes() else {
        return;
    };
    let span = std::mem::size_of_val(values);
    if span > bytes.len() {
        return;
    }
    // SAFETY: `T` is a `repr(C)` plain-data build input; the destination is a mapped
    // allocation of at least `span` bytes and cannot overlap the source.
    unsafe {
        std::ptr::copy_nonoverlapping(values.as_ptr().cast::<u8>(), bytes.as_mut_ptr(), span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Partitions follow the world's base cells: a position and its neighbours inside one
    /// cell share a partition, and a position a cell away generally does not.
    #[test]
    fn positions_in_one_base_cell_share_a_partition() {
        let edge = saffron_spatial::BASE_CELL_EDGE_METERS as f32;
        let near = partition_for_translation([1.0, 2.0, 3.0]);
        assert_eq!(near, partition_for_translation([edge - 1.0, 5.0, 9.0]));
        // Negative coordinates floor into the cell below zero rather than truncating
        // toward it, so the cell either side of the origin is a different partition.
        assert_ne!(near, partition_for_translation([-1.0, 2.0, 3.0]));
        // Every partition is addressable.
        for step in 0..64 {
            let x = step as f32 * edge;
            assert!(partition_for_translation([x, 0.0, 0.0]) < PARTITION_COUNT);
        }
    }

    /// The cell hash spreads rather than clustering: a run of adjacent cells must not all
    /// land in one partition, or the diff granularity the partitions exist for is lost.
    #[test]
    fn adjacent_cells_spread_across_the_partition_table() {
        let edge = saffron_spatial::BASE_CELL_EDGE_METERS as f32;
        let distinct: std::collections::BTreeSet<u32> = (0..64)
            .map(|step| partition_for_translation([step as f32 * edge, 0.0, 0.0]))
            .collect();
        assert!(
            distinct.len() > 32,
            "cell hash clustered: {}",
            distinct.len()
        );
    }
}
