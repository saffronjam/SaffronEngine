use crate::global_gpu_data::{
    FrameUploadRing, GlobalGpuArena, GpuArenaRange, GpuBufferUpload, GpuTableDescriptor,
    GpuTableSlotHeader, SceneInstanceTable, SceneLightTable,
};
use crate::{Device, Error, Result};

/// One slot-indexed device table whose slot allocator is the persistent GPU scene.
///
/// Every slot is a 16-byte [`GpuTableSlotHeader`] followed by the record body, padded to a
/// 16-byte-aligned stride. Capacity follows the scene's high-water slot index; growth
/// preserves existing slots through a graph-owned copy.
pub struct GpuSceneTableStorage<K> {
    pub(super) storage: GlobalGpuArena<K>,
    slot_stride: u64,
    slots: u64,
}

impl<K> GpuSceneTableStorage<K> {
    /// Creates the device buffer for `record_bytes`-sized slot bodies.
    pub fn new(device: &Device, record_bytes: u64, initial_slots: u64) -> Result<Self> {
        let unaligned = 16_u64
            .checked_add(record_bytes)
            .ok_or_else(|| Error::InvalidUploadData("scene table stride overflow".to_owned()))?;
        let slot_stride = unaligned
            .checked_add(15)
            .ok_or_else(|| Error::InvalidUploadData("scene table stride overflow".to_owned()))?
            & !15;
        Ok(Self {
            storage: GlobalGpuArena::new(device, slot_stride, initial_slots.max(1))?,
            slot_stride,
            slots: 0,
        })
    }

    /// Reserves capacity through `slot_count` exclusive; growth is signalled via
    /// [`Self::prepare_growth`].
    pub fn ensure_slots(&mut self, slot_count: u64) -> Result<()> {
        while self.slots < slot_count {
            self.storage.allocate(1, 1)?;
            self.slots += 1;
        }
        Ok(())
    }

    /// Grows the device buffer when reserved slots exceed it, returning the preserving copy.
    pub fn prepare_growth(&mut self, device: &Device) -> Result<Option<crate::GpuArenaGrowth>> {
        self.storage.prepare_growth(device)
    }

    /// Stages one slot write: the occupancy header plus `body`, zero-padded to the stride.
    pub fn stage_slot(
        &self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        slot: u32,
        generation: u32,
        occupied: u32,
        body: &[u8],
    ) -> Result<GpuBufferUpload> {
        let stride = usize::try_from(self.slot_stride).map_err(|_| {
            Error::InvalidUploadData("scene table slot exceeds address space".to_owned())
        })?;
        if body.len() + 16 > stride {
            return Err(Error::InvalidUploadData(
                "scene table record exceeds its slot stride".to_owned(),
            ));
        }
        let mut bytes = vec![0_u8; stride];
        let header = GpuTableSlotHeader {
            generation,
            occupied,
            reserved: [0; 2],
        };
        bytes[..16].copy_from_slice(bytemuck::bytes_of(&header));
        bytes[16..16 + body.len()].copy_from_slice(body);
        self.storage.stage(
            uploads,
            frame_slot,
            GpuArenaRange {
                first: slot,
                count: 1,
            },
            &bytes,
        )
    }

    /// Descriptor-ready buffer identity for this table.
    pub fn descriptor(&self, device: &Device) -> GpuTableDescriptor {
        GpuTableDescriptor {
            buffer: self.storage.buffer(),
            offset: 0,
            range: self.storage.capacity() * self.slot_stride,
            address: self.storage.address(device),
            slot_stride: self.slot_stride,
        }
    }

    /// Byte stride of one header-plus-record slot.
    pub fn slot_stride(&self) -> u64 {
        self.slot_stride
    }

    /// Current physical slot capacity.
    pub fn slot_capacity(&self) -> u64 {
        self.storage.capacity()
    }

    /// Reclaims superseded physical buffers after a frame fence signals.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        self.storage.begin_frame(completed_frame_slot)
    }
}

/// The per-world device tables.
pub struct GpuSceneWorldTables {
    /// The world's instance table.
    pub instances: GpuSceneTableStorage<SceneInstanceTable>,
    /// The world's light table.
    pub lights: GpuSceneTableStorage<SceneLightTable>,
}

/// Descriptor-ready identities of the shared scene tables and the override arena.
#[derive(Clone, Copy, Debug)]
pub struct GpuSceneTableDescriptors {
    /// Shared prototype table.
    pub prototypes: GpuTableDescriptor,
    /// Shared material-reference table.
    pub materials: GpuTableDescriptor,
    /// Shared deformation-reference table.
    pub deformations: GpuTableDescriptor,
    /// Shared SDF-reference table.
    pub sdfs: GpuTableDescriptor,
    /// Shared page table.
    pub pages: GpuTableDescriptor,
    /// Per-instance override arena.
    pub overrides: GpuTableDescriptor,
}

/// Descriptor-ready identities of one world's tables.
#[derive(Clone, Copy, Debug)]
pub struct GpuSceneWorldDescriptors {
    /// The world's instance table.
    pub instances: GpuTableDescriptor,
    /// The world's light table.
    pub lights: GpuTableDescriptor,
}
