//! Cluster-acceleration-structure execution over canonical cooked triangle clusters.
//!
//! On a device with `VK_NV_cluster_acceleration_structure`, an assembly prototype's bottom-level
//! structure is composed from its cooked clusters: one CLAS per [`PortableTriangleCluster`]-shaped
//! input, then one bottom-level structure over the CLAS references, both through the extension's
//! indirect two-op batch. The inputs are the same canonical clusters every device cooks, and the
//! produced structure slots into TLAS packing by device address exactly like a KHR build.
//!
//! Only this module and `device.rs` may import the transcribed [`crate::vk_nv_cluster`] bindings.

use std::sync::Arc;

use ash::vk;

use crate::vk_nv_cluster as nvx;
use crate::{Buffer, DeviceResources, Error, Result};

/// One cluster's build input, in canonical cooked terms: cluster-local `f32` positions and
/// 8-bit corner indices, with the prototype-relative submesh as its geometry index.
pub struct ClusterBuildInput {
    pub cluster_id: u32,
    /// Prototype-relative submesh index — the ray-query geometry index, matching the KHR
    /// path's one-geometry-per-submesh layout so material resolution is representation-blind.
    pub geometry_index: u32,
    /// The submesh's cooked opacity; a non-opaque cluster surfaces ray candidates for the
    /// coverage classifier exactly as the KHR geometry flag does.
    pub opaque: bool,
    pub positions: Vec<[f32; 3]>,
    pub local_indices: Vec<u8>,
}

/// The finished cluster-composed bottom-level structure: the CLAS pool and the
/// bottom-level implicit data it references, addressed like any BLAS.
pub struct ClusterBlas {
    clas_data: Buffer,
    blas_data: Buffer,
    address: vk::DeviceAddress,
    size: vk::DeviceSize,
    cluster_count: u32,
}

impl ClusterBlas {
    /// The device address a TLAS instance references.
    #[must_use]
    pub fn address(&self) -> vk::DeviceAddress {
        self.address
    }

    /// Bytes of bottom-level structure storage actually used.
    #[must_use]
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Bytes reserved for the CLAS pool this structure references.
    #[must_use]
    pub fn clas_bytes(&self) -> vk::DeviceSize {
        self.clas_data.size()
    }

    /// Cluster structures composed into this bottom level.
    #[must_use]
    pub fn cluster_count(&self) -> u32 {
        self.cluster_count
    }
}

/// Everything a recorded build needs alive until its submit completes: the boxed op
/// inputs the commands-info points into, the input and destination buffers, and the
/// host-readable address/size words the finish step consumes.
pub struct ClusterBlasPlan {
    cluster_count: u32,
    triangle_input: Box<nvx::TriangleClusterInputNV>,
    bottom_input: Box<nvx::ClustersBottomLevelInputNV>,
    _vertices: Buffer,
    _indices: Buffer,
    _src_infos: Buffer,
    src_infos_address: vk::DeviceAddress,
    _src_count: Buffer,
    src_count_address: vk::DeviceAddress,
    clas_data: Buffer,
    clas_data_address: vk::DeviceAddress,
    _clas_addresses: Buffer,
    clas_addresses_address: vk::DeviceAddress,
    _clas_scratch: Buffer,
    clas_scratch_address: vk::DeviceAddress,
    blas_data: Buffer,
    blas_data_address: vk::DeviceAddress,
    _bl_src_infos: Buffer,
    bl_src_infos_address: vk::DeviceAddress,
    _bl_src_count: Buffer,
    bl_src_count_address: vk::DeviceAddress,
    _blas_scratch: Buffer,
    blas_scratch_address: vk::DeviceAddress,
    blas_address_out: Buffer,
    blas_size_out: Buffer,
    staging: Buffer,
}

/// The device seam: the resolved dispatch plus the property limits the plan validates
/// against and aligns to.
pub struct ClusterBlasBuilder {
    dispatch: nvx::Dispatch,
    max_triangles: u32,
    max_vertices: u32,
    scratch_alignment: u64,
    cluster_alignment: u64,
    bottom_level_alignment: u64,
}

impl ClusterBlasBuilder {
    /// `None` when the device did not enable the extension.
    pub fn new(device: &crate::Device) -> Option<Self> {
        let dispatch = device.cluster_as_dispatch()?.clone();
        let [
            max_triangles,
            max_vertices,
            scratch_alignment,
            cluster_alignment,
            bottom_level_alignment,
        ] = device.capabilities.cluster_as_limits;
        Some(Self {
            dispatch,
            max_triangles,
            max_vertices,
            scratch_alignment: u64::from(scratch_alignment.max(1)),
            cluster_alignment: u64::from(cluster_alignment.max(1)),
            bottom_level_alignment: u64::from(bottom_level_alignment.max(1)),
        })
    }

    /// Sizes and allocates everything the two-op batch needs and writes the host-side
    /// inputs. Returns `Ok(None)` when the inputs do not fit this device's cluster limits —
    /// the caller falls back to the KHR triangle build.
    pub fn plan(
        &self,
        resources: &Arc<DeviceResources>,
        clusters: &[ClusterBuildInput],
    ) -> Result<Option<ClusterBlasPlan>> {
        let cluster_count = u32::try_from(clusters.len())
            .map_err(|_| Error::InvalidUploadData("cluster count exceeds u32".to_owned()))?;
        if cluster_count == 0 {
            return Ok(None);
        }
        let fits = clusters.iter().all(|cluster| {
            let triangles = cluster.local_indices.len() / 3;
            !cluster.positions.is_empty()
                && cluster.local_indices.len() % 3 == 0
                && triangles <= self.max_triangles as usize
                && cluster.positions.len() <= self.max_vertices as usize
                && triangles < (1 << 9)
                && cluster.positions.len() < (1 << 9)
                && cluster.geometry_index < (1 << 24)
        });
        if !fits {
            return Ok(None);
        }

        // One tightly packed f32x3 blob and one 8-bit index blob, each cluster's run
        // 4-byte aligned so its device address is too.
        let mut vertex_bytes = Vec::new();
        let mut index_bytes = Vec::new();
        let mut vertex_offsets = Vec::with_capacity(clusters.len());
        let mut index_offsets = Vec::with_capacity(clusters.len());
        for cluster in clusters {
            vertex_offsets.push(vertex_bytes.len() as u64);
            for position in &cluster.positions {
                for component in position {
                    vertex_bytes.extend_from_slice(&component.to_le_bytes());
                }
            }
            while index_bytes.len() % 4 != 0 {
                index_bytes.push(0);
            }
            index_offsets.push(index_bytes.len() as u64);
            index_bytes.extend_from_slice(&cluster.local_indices);
        }
        let input_usage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
        let vertices = Buffer::from_slice_with_usage(resources, &vertex_bytes, input_usage)?;
        let indices = Buffer::from_slice_with_usage(resources, &index_bytes, input_usage)?;
        let vertices_address = resources.buffer_device_address(vertices.handle());
        let indices_address = resources.buffer_device_address(indices.handle());

        let mut src_infos = Vec::with_capacity(clusters.len());
        for (index, cluster) in clusters.iter().enumerate() {
            let (counts, geometry) = nvx::pack_triangle_cluster_words(
                (cluster.local_indices.len() / 3) as u32,
                cluster.positions.len() as u32,
                nvx::INDEX_FORMAT_8BIT,
                cluster.geometry_index,
                if cluster.opaque {
                    nvx::GEOMETRY_OPAQUE_BIT
                } else {
                    0
                },
            );
            src_infos.push(nvx::BuildTriangleClusterInfoNV {
                cluster_id: cluster.cluster_id,
                cluster_flags: 0,
                counts_and_formats: counts,
                base_geometry_index_and_flags: geometry,
                index_buffer_stride: 1,
                vertex_buffer_stride: 12,
                geometry_index_and_flags_buffer_stride: 0,
                opacity_micromap_index_buffer_stride: 0,
                index_buffer: indices_address + index_offsets[index],
                vertex_buffer: vertices_address + vertex_offsets[index],
                geometry_index_and_flags_buffer: 0,
                opacity_micromap_array: 0,
                opacity_micromap_index_buffer: 0,
            });
        }
        // SAFETY: `BuildTriangleClusterInfoNV` is `repr(C)` plain data; the byte view is
        // exactly what the device consumes.
        let src_info_bytes = unsafe {
            std::slice::from_raw_parts(
                src_infos.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&src_infos[..]),
            )
        };
        let src_infos = Buffer::from_slice_with_usage(resources, src_info_bytes, input_usage)?;
        let src_count =
            Buffer::from_slice_with_usage(resources, &cluster_count.to_le_bytes(), input_usage)?;

        let max_triangle_count = clusters
            .iter()
            .map(|cluster| (cluster.local_indices.len() / 3) as u32)
            .max()
            .unwrap_or(0);
        let max_vertex_count = clusters
            .iter()
            .map(|cluster| cluster.positions.len() as u32)
            .max()
            .unwrap_or(0);
        let total_triangles: u32 = clusters
            .iter()
            .map(|cluster| (cluster.local_indices.len() / 3) as u32)
            .sum();
        let total_vertices: u32 = clusters
            .iter()
            .map(|cluster| cluster.positions.len() as u32)
            .sum();
        let triangle_input = Box::new(nvx::TriangleClusterInputNV {
            vertex_format: vk::Format::R32G32B32_SFLOAT,
            max_geometry_index_value: clusters
                .iter()
                .map(|cluster| cluster.geometry_index)
                .max()
                .unwrap_or(0),
            max_cluster_unique_geometry_count: 1,
            max_cluster_triangle_count: max_triangle_count,
            max_cluster_vertex_count: max_vertex_count,
            max_total_triangle_count: total_triangles,
            max_total_vertex_count: total_vertices,
            min_position_truncate_bit_count: 0,
            ..Default::default()
        });
        let clas_sizes = self.dispatch.get_build_sizes(&self.input_info(
            nvx::OP_TYPE_BUILD_TRIANGLE_CLUSTER,
            cluster_count,
            std::ptr::from_ref(&*triangle_input).cast_mut().cast(),
        ));

        let bottom_input = Box::new(nvx::ClustersBottomLevelInputNV {
            max_total_cluster_count: cluster_count,
            max_cluster_count_per_acceleration_structure: cluster_count,
            ..Default::default()
        });
        let blas_sizes = self.dispatch.get_build_sizes(&self.input_info(
            nvx::OP_TYPE_BUILD_CLUSTERS_BOTTOM_LEVEL,
            1,
            std::ptr::from_ref(&*bottom_input).cast_mut().cast(),
        ));
        if clas_sizes.acceleration_structure_size == 0
            || blas_sizes.acceleration_structure_size == 0
        {
            return Ok(None);
        }

        // Implicit-destination storage is acceleration-structure storage: dedicated, like
        // every AS allocation (suballocating beside ordinary buffers wedges the GPU on a
        // later unrelated submission).
        let storage_usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let dedicated = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        let clas_data = Buffer::with_alignment(
            resources,
            clas_sizes.acceleration_structure_size,
            storage_usage,
            &dedicated,
            self.cluster_alignment,
        )?;
        let blas_data = Buffer::with_alignment(
            resources,
            blas_sizes.acceleration_structure_size,
            storage_usage,
            &dedicated,
            self.bottom_level_alignment,
        )?;
        let scratch_usage =
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let device_local = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let scratch_alignment = self.scratch_alignment.max(resources.scratch_alignment());
        let clas_scratch = Buffer::with_alignment(
            resources,
            clas_sizes.build_scratch_size.max(1),
            scratch_usage,
            &device_local,
            scratch_alignment,
        )?;
        let blas_scratch = Buffer::with_alignment(
            resources,
            blas_sizes.build_scratch_size.max(1),
            scratch_usage,
            &device_local,
            scratch_alignment,
        )?;
        // Every destination-address/size array must live in AS-storage-usage buffers
        // (VUID-vkCmdBuildClusterAccelerationStructureIndirectNV-pCommandInfos-12307).
        let clas_addresses = Buffer::with_alignment(
            resources,
            u64::from(cluster_count) * 8,
            scratch_usage
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR,
            &device_local,
            8,
        )?;
        let clas_addresses_address = resources.buffer_device_address(clas_addresses.handle());

        let bl_info = nvx::BuildClustersBottomLevelInfoNV {
            cluster_references_count: cluster_count,
            cluster_references_stride: 8,
            cluster_references: clas_addresses_address,
        };
        // SAFETY: `BuildClustersBottomLevelInfoNV` is `repr(C)` plain data.
        let bl_info_bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&bl_info).cast::<u8>(),
                std::mem::size_of::<nvx::BuildClustersBottomLevelInfoNV>(),
            )
        };
        let bl_src_infos = Buffer::from_slice_with_usage(resources, bl_info_bytes, input_usage)?;
        let bl_src_count =
            Buffer::from_slice_with_usage(resources, &1_u32.to_le_bytes(), input_usage)?;

        // The address/size outputs are device-local AS-storage buffers like every other
        // destination array (VUID-…-12307 requires the usage, and a host-visible one fails
        // address validation); one transfer after the builds lands both words in the
        // mapped staging buffer the finish step reads.
        let out_usage = scratch_usage
            | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
            | vk::BufferUsageFlags::TRANSFER_SRC;
        let blas_address_out = Buffer::with_alignment(resources, 8, out_usage, &device_local, 8)?;
        let blas_size_out = Buffer::with_alignment(resources, 4, out_usage, &device_local, 4)?;
        let readback = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let staging = Buffer::new(resources, 12, vk::BufferUsageFlags::TRANSFER_DST, &readback)?;

        Ok(Some(ClusterBlasPlan {
            cluster_count,
            src_infos_address: resources.buffer_device_address(src_infos.handle()),
            src_count_address: resources.buffer_device_address(src_count.handle()),
            clas_data_address: resources.buffer_device_address(clas_data.handle()),
            clas_addresses_address,
            clas_scratch_address: resources.buffer_device_address(clas_scratch.handle()),
            blas_data_address: resources.buffer_device_address(blas_data.handle()),
            bl_src_infos_address: resources.buffer_device_address(bl_src_infos.handle()),
            bl_src_count_address: resources.buffer_device_address(bl_src_count.handle()),
            blas_scratch_address: resources.buffer_device_address(blas_scratch.handle()),
            triangle_input,
            bottom_input,
            _vertices: vertices,
            _indices: indices,
            _src_infos: src_infos,
            _src_count: src_count,
            clas_data,
            _clas_addresses: clas_addresses,
            _clas_scratch: clas_scratch,
            blas_data,
            _bl_src_infos: bl_src_infos,
            _bl_src_count: bl_src_count,
            _blas_scratch: blas_scratch,
            blas_address_out,
            blas_size_out,
            staging,
        }))
    }

    /// Records the two-op batch: every CLAS, one barrier, then the bottom level over their
    /// written references. The caller submits and waits before [`Self::finish`].
    pub fn record(
        &self,
        resources: &DeviceResources,
        cmd: vk::CommandBuffer,
        plan: &ClusterBlasPlan,
    ) {
        let clas_commands = nvx::CommandsInfoNV {
            s_type: nvx::STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_COMMANDS_INFO_NV,
            p_next: std::ptr::null_mut(),
            input: self.input_info(
                nvx::OP_TYPE_BUILD_TRIANGLE_CLUSTER,
                plan.cluster_count,
                std::ptr::from_ref(&*plan.triangle_input).cast_mut().cast(),
            ),
            dst_implicit_data: plan.clas_data_address,
            scratch_data: plan.clas_scratch_address,
            dst_addresses_array: vk::StridedDeviceAddressRegionKHR {
                device_address: plan.clas_addresses_address,
                stride: 8,
                size: u64::from(plan.cluster_count) * 8,
            },
            dst_sizes_array: vk::StridedDeviceAddressRegionKHR::default(),
            src_infos_array: vk::StridedDeviceAddressRegionKHR {
                device_address: plan.src_infos_address,
                stride: 64,
                size: u64::from(plan.cluster_count) * 64,
            },
            src_infos_count: plan.src_count_address,
            address_resolution_flags: 0,
        };
        // SAFETY: the extension seam. The command buffer is recording; every address in the
        // batch points at a live plan buffer sized in `plan`.
        unsafe { self.dispatch.cmd_build_indirect(cmd, &clas_commands) };

        // The bottom-level op consumes the CLAS addresses the first op wrote, on the same
        // build stage.
        let barrier = vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
            .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
            .dst_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
            .dst_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR);
        let barriers = [barrier];
        let dependency = vk::DependencyInfo::default().memory_barriers(&barriers);
        // SAFETY: the ash seam. The command buffer is recording.
        unsafe { resources.device().cmd_pipeline_barrier2(cmd, &dependency) };

        let blas_commands = nvx::CommandsInfoNV {
            s_type: nvx::STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_COMMANDS_INFO_NV,
            p_next: std::ptr::null_mut(),
            input: self.input_info(
                nvx::OP_TYPE_BUILD_CLUSTERS_BOTTOM_LEVEL,
                1,
                std::ptr::from_ref(&*plan.bottom_input).cast_mut().cast(),
            ),
            dst_implicit_data: plan.blas_data_address,
            scratch_data: plan.blas_scratch_address,
            dst_addresses_array: vk::StridedDeviceAddressRegionKHR {
                device_address: resources.buffer_device_address(plan.blas_address_out.handle()),
                stride: 8,
                size: 8,
            },
            dst_sizes_array: vk::StridedDeviceAddressRegionKHR {
                device_address: resources.buffer_device_address(plan.blas_size_out.handle()),
                stride: 4,
                size: 4,
            },
            src_infos_array: vk::StridedDeviceAddressRegionKHR {
                device_address: plan.bl_src_infos_address,
                stride: 16,
                size: 16,
            },
            src_infos_count: plan.bl_src_count_address,
            address_resolution_flags: 0,
        };
        // SAFETY: as above — recording, addresses live.
        unsafe { self.dispatch.cmd_build_indirect(cmd, &blas_commands) };

        // Land the written address and size in the mapped staging buffer for the finish
        // step; the submit's fence makes the transfer host-visible.
        let to_transfer = vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
            .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
            .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
        let to_transfer = [to_transfer];
        let transfer_dependency = vk::DependencyInfo::default().memory_barriers(&to_transfer);
        let address_copy = [vk::BufferCopy::default().size(8)];
        let size_copy = [vk::BufferCopy::default().dst_offset(8).size(4)];
        // SAFETY: the ash seam. The command buffer is recording; the copies stay inside
        // the staging buffer's 12 bytes.
        unsafe {
            resources
                .device()
                .cmd_pipeline_barrier2(cmd, &transfer_dependency);
            resources.device().cmd_copy_buffer(
                cmd,
                plan.blas_address_out.handle(),
                plan.staging.handle(),
                &address_copy,
            );
            resources.device().cmd_copy_buffer(
                cmd,
                plan.blas_size_out.handle(),
                plan.staging.handle(),
                &size_copy,
            );
        }
    }

    /// Reads the built structure's address and size back and packages the retained
    /// storage. Call only after the recorded submit's fence completed.
    pub fn finish(&self, plan: ClusterBlasPlan) -> Result<ClusterBlas> {
        plan.staging.invalidate_mapped()?;
        // SAFETY: the staging buffer is HOST_VISIBLE + MAPPED, 12 bytes, written by the
        // waited submit's copies (address at 0, size at 8).
        let (address, size) = unsafe {
            (
                plan.staging.mapped_ptr().cast::<u64>().read_unaligned(),
                plan.staging
                    .mapped_ptr()
                    .add(8)
                    .cast::<u32>()
                    .read_unaligned(),
            )
        };
        if address == 0 {
            return Err(Error::InvalidUploadData(
                "cluster bottom-level build wrote no structure address".to_owned(),
            ));
        }
        Ok(ClusterBlas {
            clas_data: plan.clas_data,
            blas_data: plan.blas_data,
            address,
            size: u64::from(size),
            cluster_count: plan.cluster_count,
        })
    }

    fn input_info(
        &self,
        op_type: u32,
        count: u32,
        op_input: *mut std::ffi::c_void,
    ) -> nvx::InputInfoNV {
        nvx::InputInfoNV {
            max_acceleration_structure_count: count,
            flags: vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE,
            op_type,
            op_mode: nvx::OP_MODE_IMPLICIT_DESTINATIONS,
            op_input,
            ..Default::default()
        }
    }
}

// SAFETY: every field is shared read-only after construction; the buffers are `Send` and
// carry no thread-affine state.
unsafe impl Send for ClusterBlas {}
// SAFETY: as above — nothing is mutated after construction.
unsafe impl Sync for ClusterBlas {}

impl std::fmt::Debug for ClusterBlas {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterBlas")
            .field("address", &self.address)
            .field("size", &self.size)
            .field("clusters", &self.cluster_count)
            .finish()
    }
}

// The two data buffers keep their allocations alive for the structure's lifetime; the
// buffers' own `Drop` frees them.
const _: fn(&ClusterBlas) -> (&Buffer, &Buffer) = |blas| (&blas.clas_data, &blas.blas_data);
