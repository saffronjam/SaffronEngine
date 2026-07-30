//! Hand-transcribed `VK_NV_partitioned_acceleration_structure` bindings (spec revision 1),
//! private to this crate: only `device.rs`, `descriptors.rs`, and `rt_ptlas.rs` may import
//! them.
//!
//! The pinned ash release carries no binding for this extension, so the structs below are
//! transcribed from the SDK's `vulkan_core.h`. The same two belts as [`crate::vk_nv_cluster`] keep
//! them honest: the pinned-ash-release test there fails on any ash bump naming both modules, and
//! the device probe refuses to enable the extension on any spec revision other than the transcribed
//! one.

use std::ffi::{CStr, c_void};

use ash::vk;

pub const NAME: &CStr = c"VK_NV_partitioned_acceleration_structure";
pub const SPEC_VERSION: u32 = 1;

pub const STRUCTURE_TYPE_PHYSICAL_DEVICE_PARTITIONED_ACCELERATION_STRUCTURE_FEATURES_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_570_000);
pub const STRUCTURE_TYPE_PHYSICAL_DEVICE_PARTITIONED_ACCELERATION_STRUCTURE_PROPERTIES_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_570_001);
pub const STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET_PARTITIONED_ACCELERATION_STRUCTURE_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_570_002);
pub const STRUCTURE_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_INSTANCES_INPUT_NV: vk::StructureType =
    vk::StructureType::from_raw(1_000_570_003);
pub const STRUCTURE_TYPE_BUILD_PARTITIONED_ACCELERATION_STRUCTURE_INFO_NV: vk::StructureType =
    vk::StructureType::from_raw(1_000_570_004);

/// The descriptor type a partitioned structure binds as. Shader-side the declaration is an
/// ordinary acceleration structure; only the layout's descriptor type differs, which is why
/// set 6's layout is a per-device variant rather than a second binding.
pub const DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV: vk::DescriptorType =
    vk::DescriptorType::from_raw(1_000_570_000);

/// The partition an instance belongs to when it is not confined to one — traced for every
/// ray regardless of which partitions a query touches.
pub const PARTITION_INDEX_GLOBAL: u32 = u32::MAX;

pub const OP_TYPE_WRITE_INSTANCE: u32 = 0;
pub const OP_TYPE_UPDATE_INSTANCE: u32 = 1;

pub const INSTANCE_FLAG_TRIANGLE_FACING_CULL_DISABLE: u32 = 0x1;
pub const INSTANCE_FLAG_TRIANGLE_FLIP_FACING: u32 = 0x2;
pub const INSTANCE_FLAG_FORCE_OPAQUE: u32 = 0x4;
pub const INSTANCE_FLAG_FORCE_NO_OPAQUE: u32 = 0x8;

/// Every instance flag that carries over from a KHR instance. The KHR enumeration has one
/// more — the micromap disable — which this extension does not define; masking to these four
/// is what keeps an unmapped bit from being read as one of them.
pub const INSTANCE_FLAGS_CARRIED: u32 = INSTANCE_FLAG_TRIANGLE_FACING_CULL_DISABLE
    | INSTANCE_FLAG_TRIANGLE_FLIP_FACING
    | INSTANCE_FLAG_FORCE_OPAQUE
    | INSTANCE_FLAG_FORCE_NO_OPAQUE;

/// `VkPhysicalDevicePartitionedAccelerationStructureFeaturesNV`.
#[repr(C)]
pub struct PhysicalDevicePartitionedAccelerationStructureFeaturesNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub partitioned_acceleration_structure: vk::Bool32,
}

impl Default for PhysicalDevicePartitionedAccelerationStructureFeaturesNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_PARTITIONED_ACCELERATION_STRUCTURE_FEATURES_NV,
            p_next: std::ptr::null_mut(),
            partitioned_acceleration_structure: 0,
        }
    }
}

/// `VkPhysicalDevicePartitionedAccelerationStructurePropertiesNV`.
#[repr(C)]
pub struct PhysicalDevicePartitionedAccelerationStructurePropertiesNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub max_partition_count: u32,
}

impl Default for PhysicalDevicePartitionedAccelerationStructurePropertiesNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_PARTITIONED_ACCELERATION_STRUCTURE_PROPERTIES_NV,
            p_next: std::ptr::null_mut(),
            max_partition_count: 0,
        }
    }
}

/// `VkPartitionedAccelerationStructureInstancesInputNV` — the sizing and build shape.
#[repr(C)]
pub struct InstancesInputNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub flags: vk::BuildAccelerationStructureFlagsKHR,
    pub instance_count: u32,
    pub max_instance_per_partition_count: u32,
    pub partition_count: u32,
    pub max_instance_in_global_partition_count: u32,
}

impl Default for InstancesInputNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_INSTANCES_INPUT_NV,
            p_next: std::ptr::null_mut(),
            flags: vk::BuildAccelerationStructureFlagsKHR::empty(),
            instance_count: 0,
            max_instance_per_partition_count: 0,
            partition_count: 0,
            max_instance_in_global_partition_count: 0,
        }
    }
}

/// `VkBuildPartitionedAccelerationStructureInfoNV`.
#[repr(C)]
pub struct BuildInfoNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub input: InstancesInputNV,
    pub src_acceleration_structure_data: vk::DeviceAddress,
    pub dst_acceleration_structure_data: vk::DeviceAddress,
    pub scratch_data: vk::DeviceAddress,
    pub src_infos: vk::DeviceAddress,
    pub src_infos_count: vk::DeviceAddress,
}

/// `VkStridedDeviceAddressNV` — a start address plus a stride, with no size word (unlike the
/// KHR strided region).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StridedDeviceAddressNV {
    pub start_address: vk::DeviceAddress,
    pub stride_in_bytes: vk::DeviceSize,
}

/// `VkBuildPartitionedAccelerationStructureIndirectCommandNV` — one op-stream entry naming
/// an operation and the argument array it applies to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IndirectCommandNV {
    pub op_type: u32,
    pub arg_count: u32,
    pub arg_data: StridedDeviceAddressNV,
}

/// `VkPartitionedAccelerationStructureWriteInstanceDataNV` — one instance placed whole.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub struct WriteInstanceDataNV {
    /// Row-major 3×4, the same packing the KHR instance transform uses.
    pub transform: [f32; 12],
    pub explicit_aabb: [f32; 6],
    pub instance_id: u32,
    pub instance_mask: u32,
    pub instance_contribution_to_hit_group_index: u32,
    pub instance_flags: u32,
    pub instance_index: u32,
    pub partition_index: u32,
    pub acceleration_structure: vk::DeviceAddress,
}

/// `VkPartitionedAccelerationStructureUpdateInstanceDataNV` — a placed instance's structure
/// address swapped without rewriting its transform or partition.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UpdateInstanceDataNV {
    pub instance_index: u32,
    pub instance_contribution_to_hit_group_index: u32,
    pub acceleration_structure: vk::DeviceAddress,
}

/// `VkWriteDescriptorSetPartitionedAccelerationStructureNV` — the descriptor write's chained
/// payload, carrying structure device addresses rather than handles.
#[repr(C)]
pub struct WriteDescriptorSetPartitionedAccelerationStructureNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub acceleration_structure_count: u32,
    pub p_acceleration_structures: *const vk::DeviceAddress,
}

impl Default for WriteDescriptorSetPartitionedAccelerationStructureNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET_PARTITIONED_ACCELERATION_STRUCTURE_NV,
            p_next: std::ptr::null_mut(),
            acceleration_structure_count: 0,
            p_acceleration_structures: std::ptr::null(),
        }
    }
}

type PfnGetPartitionedAccelerationStructuresBuildSizes = unsafe extern "system" fn(
    device: vk::Device,
    info: *const InstancesInputNV,
    size_info: *mut vk::AccelerationStructureBuildSizesInfoKHR<'_>,
);

type PfnCmdBuildPartitionedAccelerationStructures =
    unsafe extern "system" fn(command_buffer: vk::CommandBuffer, build_info: *const BuildInfoNV);

/// The two entry points, resolved through `vkGetDeviceProcAddr`. A null proc address yields
/// `None` rather than a panicking stub.
#[derive(Clone)]
pub struct Dispatch {
    device: vk::Device,
    get_build_sizes: PfnGetPartitionedAccelerationStructuresBuildSizes,
    cmd_build: PfnCmdBuildPartitionedAccelerationStructures,
}

impl Dispatch {
    pub fn load(instance: &ash::Instance, device: &ash::Device) -> Option<Self> {
        // SAFETY: the ash seam; both names are the extension's entry points.
        let resolve = |name: &CStr| unsafe {
            (instance.fp_v1_0().get_device_proc_addr)(device.handle(), name.as_ptr())
        };
        let get_build_sizes = resolve(c"vkGetPartitionedAccelerationStructuresBuildSizesNV")?;
        let cmd_build = resolve(c"vkCmdBuildPartitionedAccelerationStructuresNV")?;
        // SAFETY: transmuting resolved non-null proc addresses to their typed signatures,
        // which is the contract `vkGetDeviceProcAddr` documents.
        unsafe {
            Some(Self {
                device: device.handle(),
                get_build_sizes: std::mem::transmute::<
                    unsafe extern "system" fn(),
                    PfnGetPartitionedAccelerationStructuresBuildSizes,
                >(get_build_sizes),
                cmd_build: std::mem::transmute::<
                    unsafe extern "system" fn(),
                    PfnCmdBuildPartitionedAccelerationStructures,
                >(cmd_build),
            })
        }
    }

    /// `vkGetPartitionedAccelerationStructuresBuildSizesNV`.
    pub fn get_build_sizes(
        &self,
        input: &InstancesInputNV,
    ) -> vk::AccelerationStructureBuildSizesInfoKHR<'static> {
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the extension seam; both structs outlive the call.
        unsafe { (self.get_build_sizes)(self.device, input, &mut sizes) };
        sizes
    }

    /// `vkCmdBuildPartitionedAccelerationStructuresNV`.
    ///
    /// # Safety
    ///
    /// The command buffer must be recording, and every device address in `info` must be
    /// valid for the build per the extension's valid-usage rules.
    pub unsafe fn cmd_build(&self, cmd: vk::CommandBuffer, info: &BuildInfoNV) {
        // SAFETY: forwarded to the caller's contract.
        unsafe { (self.cmd_build)(cmd, info) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C layouts these structs mirror, pinned by exact byte size and by the field
    /// offsets a mistyped or reordered member would move.
    /// The partitioned instance flags reuse the KHR bit values, which is what lets one
    /// placement describe an instance for either top-level form. A divergence here would
    /// silently flip an instance's opacity rather than fail to compile.
    #[test]
    fn instance_flag_bits_agree_with_the_khr_enumeration() {
        assert_eq!(
            INSTANCE_FLAG_FORCE_OPAQUE,
            vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE.as_raw()
        );
        assert_eq!(
            INSTANCE_FLAG_FORCE_NO_OPAQUE,
            vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE.as_raw()
        );
        assert_eq!(
            crate::rt_ptlas::instance_flags(
                vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE
                    | vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE
            ),
            vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE.as_raw()
                | INSTANCE_FLAG_FORCE_OPAQUE
        );
        // The KHR-only micromap-disable bit has no partitioned counterpart and is dropped
        // rather than misread as one of the four that do carry over.
        assert_eq!(
            crate::rt_ptlas::instance_flags(
                vk::GeometryInstanceFlagsKHR::DISABLE_OPACITY_MICROMAPS_EXT
            ),
            0
        );
    }

    #[test]
    fn transcribed_struct_sizes_match_the_header_layout() {
        assert_eq!(
            std::mem::size_of::<PhysicalDevicePartitionedAccelerationStructureFeaturesNV>(),
            24
        );
        assert_eq!(
            std::mem::size_of::<PhysicalDevicePartitionedAccelerationStructurePropertiesNV>(),
            24
        );
        assert_eq!(std::mem::size_of::<InstancesInputNV>(), 40);
        assert_eq!(std::mem::size_of::<BuildInfoNV>(), 96);
        assert_eq!(std::mem::size_of::<StridedDeviceAddressNV>(), 16);
        assert_eq!(std::mem::size_of::<IndirectCommandNV>(), 24);
        assert_eq!(std::mem::size_of::<WriteInstanceDataNV>(), 104);
        assert_eq!(std::mem::size_of::<UpdateInstanceDataNV>(), 16);
        assert_eq!(
            std::mem::size_of::<WriteDescriptorSetPartitionedAccelerationStructureNV>(),
            32
        );
        assert_eq!(std::mem::offset_of!(BuildInfoNV, input), 16);
        assert_eq!(
            std::mem::offset_of!(BuildInfoNV, src_acceleration_structure_data),
            56
        );
        assert_eq!(std::mem::offset_of!(BuildInfoNV, src_infos_count), 88);
        assert_eq!(std::mem::offset_of!(WriteInstanceDataNV, explicit_aabb), 48);
        assert_eq!(std::mem::offset_of!(WriteInstanceDataNV, instance_id), 72);
        assert_eq!(
            std::mem::offset_of!(WriteInstanceDataNV, instance_index),
            88
        );
        assert_eq!(
            std::mem::offset_of!(WriteInstanceDataNV, partition_index),
            92
        );
        assert_eq!(
            std::mem::offset_of!(WriteInstanceDataNV, acceleration_structure),
            96
        );
    }
}
