//! The GPU selection-ID pick: one draw-record identity resolved from one viewport pixel.

use ash::vk;

/// Target format of the selection identity attachment: the tagged draw record behind the
/// picked pixel.
pub const SELECTION_ID_FORMAT: vk::Format = vk::Format::R32G32B32A32_UINT;

/// Target format of the selection surface attachments (world position, world normal).
pub const SELECTION_SURFACE_FORMAT: vk::Format = vk::Format::R32G32B32A32_SFLOAT;

/// What one picked pixel resolved to: the draw record the selection pass wrote there, plus the
/// surface point it was written from.
///
/// `representation` is the [`crate::GpuRepresentation`] discriminant; `instance_slot` names the
/// GPU-scene instance the record belongs to, which is the identity the scene mirror translates
/// back into an entity or a plant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionHit {
    /// The record's representation class.
    pub representation: crate::GpuRepresentation,
    /// The record's GPU-scene instance slot.
    pub instance_slot: u32,
    /// The record's content index — the micro candidate for a blade, the page for a voxel.
    pub content_index: u32,
    /// The record's assembly use, or [`crate::GPU_ASSEMBLY_NO_USE`] when the record places no use.
    pub assembly_use: u32,
    /// World-space position of the picked surface point, in meters.
    pub position: [f32; 3],
    /// World-space geometric normal at that point.
    pub normal: [f32; 3],
}

/// The words one selection readback copies back: the identity attachment, then the two surface
/// attachments, one texel each.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SelectionReadback {
    pub(crate) id: [u32; 4],
    pub(crate) position: [f32; 4],
    pub(crate) normal: [f32; 4],
}

impl SelectionReadback {
    /// Decodes the readback, or `None` where the pixel kept its cleared identity (nothing the
    /// selection pass drew covers it).
    pub(crate) fn decode(self) -> Option<SelectionHit> {
        let representation = match self.id[0].checked_sub(1)? {
            0 => crate::GpuRepresentation::TriangleCluster,
            1 => crate::GpuRepresentation::AggregateVoxel,
            2 => crate::GpuRepresentation::MicroBlade,
            3 => crate::GpuRepresentation::DisplacedMicro,
            _ => return None,
        };
        Some(SelectionHit {
            representation,
            instance_slot: self.id[1],
            content_index: self.id[2],
            assembly_use: self.id[3],
            position: [self.position[0], self.position[1], self.position[2]],
            normal: [self.normal[0], self.normal[1], self.normal[2]],
        })
    }
}
