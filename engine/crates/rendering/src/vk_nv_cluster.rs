//! Hand-transcribed `VK_NV_cluster_acceleration_structure` bindings (spec revision 4),
//! private to this crate: only `device.rs` and `rt_cluster.rs` may import them.
//!
//! The pinned ash release carries no binding for this extension, so the structs below are
//! transcribed from the SDK's `vulkan_core.h`. Two belts keep them honest:
//! `the_pinned_ash_release_still_lacks_this_extension` fails on any ash bump, and the device probe
//! refuses to enable the extension on any other spec revision, since a revision bump can move
//! struct layouts and a hand-written binding has no generator following it.

use std::ffi::{CStr, c_void};

use ash::vk;

pub const NAME: &CStr = c"VK_NV_cluster_acceleration_structure";
pub const SPEC_VERSION: u32 = 4;

pub const STRUCTURE_TYPE_PHYSICAL_DEVICE_CLUSTER_ACCELERATION_STRUCTURE_FEATURES_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_569_000);
pub const STRUCTURE_TYPE_PHYSICAL_DEVICE_CLUSTER_ACCELERATION_STRUCTURE_PROPERTIES_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_569_001);
pub const STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_CLUSTERS_BOTTOM_LEVEL_INPUT_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_569_002);
pub const STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_TRIANGLE_CLUSTER_INPUT_NV:
    vk::StructureType = vk::StructureType::from_raw(1_000_569_003);
pub const STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_INPUT_INFO_NV: vk::StructureType =
    vk::StructureType::from_raw(1_000_569_005);
pub const STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_COMMANDS_INFO_NV: vk::StructureType =
    vk::StructureType::from_raw(1_000_569_006);

pub const OP_TYPE_BUILD_CLUSTERS_BOTTOM_LEVEL: u32 = 1;
pub const OP_TYPE_BUILD_TRIANGLE_CLUSTER: u32 = 2;

pub const OP_MODE_IMPLICIT_DESTINATIONS: u32 = 0;

pub const INDEX_FORMAT_8BIT: u32 = 0x1;

pub const GEOMETRY_OPAQUE_BIT: u32 = 0x4;

/// `VkPhysicalDeviceClusterAccelerationStructureFeaturesNV`.
#[repr(C)]
pub struct PhysicalDeviceClusterAccelerationStructureFeaturesNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub cluster_acceleration_structure: vk::Bool32,
}

impl Default for PhysicalDeviceClusterAccelerationStructureFeaturesNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_CLUSTER_ACCELERATION_STRUCTURE_FEATURES_NV,
            p_next: std::ptr::null_mut(),
            cluster_acceleration_structure: 0,
        }
    }
}

/// `VkPhysicalDeviceClusterAccelerationStructurePropertiesNV`.
#[repr(C)]
pub struct PhysicalDeviceClusterAccelerationStructurePropertiesNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub max_vertices_per_cluster: u32,
    pub max_triangles_per_cluster: u32,
    pub cluster_scratch_byte_alignment: u32,
    pub cluster_byte_alignment: u32,
    pub cluster_template_byte_alignment: u32,
    pub cluster_bottom_level_byte_alignment: u32,
    pub cluster_template_bounds_byte_alignment: u32,
    pub max_cluster_geometry_index: u32,
}

impl Default for PhysicalDeviceClusterAccelerationStructurePropertiesNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_CLUSTER_ACCELERATION_STRUCTURE_PROPERTIES_NV,
            p_next: std::ptr::null_mut(),
            max_vertices_per_cluster: 0,
            max_triangles_per_cluster: 0,
            cluster_scratch_byte_alignment: 0,
            cluster_byte_alignment: 0,
            cluster_template_byte_alignment: 0,
            cluster_bottom_level_byte_alignment: 0,
            cluster_template_bounds_byte_alignment: 0,
            max_cluster_geometry_index: 0,
        }
    }
}

/// `VkClusterAccelerationStructureClustersBottomLevelInputNV`.
#[repr(C)]
pub struct ClustersBottomLevelInputNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub max_total_cluster_count: u32,
    pub max_cluster_count_per_acceleration_structure: u32,
}

impl Default for ClustersBottomLevelInputNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_CLUSTERS_BOTTOM_LEVEL_INPUT_NV,
            p_next: std::ptr::null_mut(),
            max_total_cluster_count: 0,
            max_cluster_count_per_acceleration_structure: 0,
        }
    }
}

/// `VkClusterAccelerationStructureTriangleClusterInputNV`.
#[repr(C)]
pub struct TriangleClusterInputNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub vertex_format: vk::Format,
    pub max_geometry_index_value: u32,
    pub max_cluster_unique_geometry_count: u32,
    pub max_cluster_triangle_count: u32,
    pub max_cluster_vertex_count: u32,
    pub max_total_triangle_count: u32,
    pub max_total_vertex_count: u32,
    pub min_position_truncate_bit_count: u32,
}

impl Default for TriangleClusterInputNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_TRIANGLE_CLUSTER_INPUT_NV,
            p_next: std::ptr::null_mut(),
            vertex_format: vk::Format::UNDEFINED,
            max_geometry_index_value: 0,
            max_cluster_unique_geometry_count: 0,
            max_cluster_triangle_count: 0,
            max_cluster_vertex_count: 0,
            max_total_triangle_count: 0,
            max_total_vertex_count: 0,
            min_position_truncate_bit_count: 0,
        }
    }
}

/// `VkClusterAccelerationStructureInputInfoNV`. `op_input` is the C union of three
/// pointers, which is one pointer wide; the caller points it at the input matching
/// `op_type`.
#[repr(C)]
pub struct InputInfoNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub max_acceleration_structure_count: u32,
    pub flags: vk::BuildAccelerationStructureFlagsKHR,
    pub op_type: u32,
    pub op_mode: u32,
    pub op_input: *mut c_void,
}

impl Default for InputInfoNV {
    fn default() -> Self {
        Self {
            s_type: STRUCTURE_TYPE_CLUSTER_ACCELERATION_STRUCTURE_INPUT_INFO_NV,
            p_next: std::ptr::null_mut(),
            max_acceleration_structure_count: 0,
            flags: vk::BuildAccelerationStructureFlagsKHR::empty(),
            op_type: 0,
            op_mode: 0,
            op_input: std::ptr::null_mut(),
        }
    }
}

/// `VkClusterAccelerationStructureCommandsInfoNV`.
#[repr(C)]
pub struct CommandsInfoNV {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub input: InputInfoNV,
    pub dst_implicit_data: vk::DeviceAddress,
    pub scratch_data: vk::DeviceAddress,
    pub dst_addresses_array: vk::StridedDeviceAddressRegionKHR,
    pub dst_sizes_array: vk::StridedDeviceAddressRegionKHR,
    pub src_infos_array: vk::StridedDeviceAddressRegionKHR,
    pub src_infos_count: vk::DeviceAddress,
    pub address_resolution_flags: u32,
}

/// `VkClusterAccelerationStructureBuildClustersBottomLevelInfoNV` — one row of the
/// bottom-level op's source-infos array.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BuildClustersBottomLevelInfoNV {
    pub cluster_references_count: u32,
    pub cluster_references_stride: u32,
    pub cluster_references: vk::DeviceAddress,
}

/// `VkClusterAccelerationStructureBuildTriangleClusterInfoNV` — one row of the triangle-
/// cluster op's source-infos array. The C declaration packs two bitfield words; the
/// constructor packs them the way the header lays them out (LSB-first within each word).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BuildTriangleClusterInfoNV {
    pub cluster_id: u32,
    pub cluster_flags: u32,
    /// `triangleCount:9 | vertexCount:9 | positionTruncateBitCount:6 | indexType:4 |
    /// opacityMicromapIndexType:4`.
    pub counts_and_formats: u32,
    /// `geometryIndex:24 | reserved:5 | geometryFlags:3`.
    pub base_geometry_index_and_flags: u32,
    pub index_buffer_stride: u16,
    pub vertex_buffer_stride: u16,
    pub geometry_index_and_flags_buffer_stride: u16,
    pub opacity_micromap_index_buffer_stride: u16,
    pub index_buffer: vk::DeviceAddress,
    pub vertex_buffer: vk::DeviceAddress,
    pub geometry_index_and_flags_buffer: vk::DeviceAddress,
    pub opacity_micromap_array: vk::DeviceAddress,
    pub opacity_micromap_index_buffer: vk::DeviceAddress,
}

/// Packs the two bitfield words of [`BuildTriangleClusterInfoNV`].
#[must_use]
pub fn pack_triangle_cluster_words(
    triangle_count: u32,
    vertex_count: u32,
    index_type: u32,
    geometry_index: u32,
    geometry_flags: u32,
) -> (u32, u32) {
    debug_assert!(triangle_count < 1 << 9 && vertex_count < 1 << 9);
    debug_assert!(index_type < 1 << 4 && geometry_index < 1 << 24 && geometry_flags < 1 << 3);
    let counts =
        (triangle_count & 0x1ff) | ((vertex_count & 0x1ff) << 9) | ((index_type & 0xf) << 24);
    let geometry = (geometry_index & 0xff_ffff) | ((geometry_flags & 0x7) << 29);
    (counts, geometry)
}

type PfnGetClusterAccelerationStructureBuildSizes = unsafe extern "system" fn(
    device: vk::Device,
    info: *const InputInfoNV,
    size_info: *mut vk::AccelerationStructureBuildSizesInfoKHR<'_>,
);

type PfnCmdBuildClusterAccelerationStructureIndirect = unsafe extern "system" fn(
    command_buffer: vk::CommandBuffer,
    command_infos: *const CommandsInfoNV,
);

/// The two entry points, resolved through `vkGetDeviceProcAddr`. A null proc address
/// yields `None` rather than a panicking stub.
#[derive(Clone)]
pub struct Dispatch {
    device: vk::Device,
    get_build_sizes: PfnGetClusterAccelerationStructureBuildSizes,
    cmd_build_indirect: PfnCmdBuildClusterAccelerationStructureIndirect,
}

impl Dispatch {
    pub fn load(instance: &ash::Instance, device: &ash::Device) -> Option<Self> {
        // SAFETY: the ash seam; both names are the extension's entry points.
        let resolve = |name: &CStr| unsafe {
            (instance.fp_v1_0().get_device_proc_addr)(device.handle(), name.as_ptr())
        };
        let get_build_sizes = resolve(c"vkGetClusterAccelerationStructureBuildSizesNV")?;
        let cmd_build_indirect = resolve(c"vkCmdBuildClusterAccelerationStructureIndirectNV")?;
        // SAFETY: transmuting resolved non-null proc addresses to their typed signatures,
        // which is the contract `vkGetDeviceProcAddr` documents.
        unsafe {
            Some(Self {
                device: device.handle(),
                get_build_sizes: std::mem::transmute::<
                    unsafe extern "system" fn(),
                    PfnGetClusterAccelerationStructureBuildSizes,
                >(get_build_sizes),
                cmd_build_indirect: std::mem::transmute::<
                    unsafe extern "system" fn(),
                    PfnCmdBuildClusterAccelerationStructureIndirect,
                >(cmd_build_indirect),
            })
        }
    }

    /// `vkGetClusterAccelerationStructureBuildSizesNV`.
    pub fn get_build_sizes(
        &self,
        info: &InputInfoNV,
    ) -> vk::AccelerationStructureBuildSizesInfoKHR<'static> {
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the extension seam; `info` and `sizes` outlive the call.
        unsafe { (self.get_build_sizes)(self.device, info, &mut sizes) };
        sizes
    }

    /// `vkCmdBuildClusterAccelerationStructureIndirectNV`.
    ///
    /// # Safety
    ///
    /// The command buffer must be recording, and every device address in `info` must be
    /// valid for the build per the extension's valid-usage rules.
    pub unsafe fn cmd_build_indirect(&self, cmd: vk::CommandBuffer, info: &CommandsInfoNV) {
        // SAFETY: forwarded to the caller's contract.
        unsafe { (self.cmd_build_indirect)(cmd, info) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only way this extension can reach the tree natively is an ash bump, and this
    /// fails on every ash bump with instructions attached. It doubles as the record of
    /// which header revision the structs were transcribed against.
    #[test]
    fn the_pinned_ash_release_still_lacks_this_extension() {
        assert_eq!(
            ash::vk::HEADER_VERSION_COMPLETE,
            ash::vk::make_api_version(0, 1, 3, 281),
            "ash moved off 1.3.281 — check for ash::nv::cluster_acceleration_structure and \
             ash::nv::partitioned_acceleration_structure; delete src/vk_nv_cluster.rs and \
             src/vk_nv_ptlas.rs for whichever now exists, or repin this assertion in the \
             same commit"
        );
    }

    /// The C layouts these structs mirror, pinned by exact byte size so a reordered or
    /// mistyped field cannot compile silently.
    #[test]
    fn transcribed_struct_sizes_match_the_header_layout() {
        assert_eq!(
            std::mem::size_of::<PhysicalDeviceClusterAccelerationStructureFeaturesNV>(),
            24
        );
        assert_eq!(
            std::mem::size_of::<PhysicalDeviceClusterAccelerationStructurePropertiesNV>(),
            48
        );
        assert_eq!(std::mem::size_of::<ClustersBottomLevelInputNV>(), 24);
        assert_eq!(std::mem::size_of::<TriangleClusterInputNV>(), 48);
        assert_eq!(std::mem::size_of::<InputInfoNV>(), 40);
        assert_eq!(std::mem::size_of::<CommandsInfoNV>(), 160);
        assert_eq!(std::mem::size_of::<BuildTriangleClusterInfoNV>(), 64);
        assert_eq!(std::mem::size_of::<BuildClustersBottomLevelInfoNV>(), 16);
    }

    #[test]
    fn the_bitfield_packers_place_each_field_at_its_header_offset() {
        let (counts, geometry) = pack_triangle_cluster_words(0x1ff, 0x1ff, 0xf, 0xff_ffff, 0x7);
        assert_eq!(counts, 0x1ff | (0x1ff << 9) | (0xf << 24));
        assert_eq!(geometry, 0xff_ffff | (0x7 << 29));
        let (counts, geometry) = pack_triangle_cluster_words(3, 5, INDEX_FORMAT_8BIT, 2, 0);
        assert_eq!(counts & 0x1ff, 3);
        assert_eq!((counts >> 9) & 0x1ff, 5);
        assert_eq!((counts >> 24) & 0xf, INDEX_FORMAT_8BIT);
        assert_eq!(geometry & 0xff_ffff, 2);
    }
}
