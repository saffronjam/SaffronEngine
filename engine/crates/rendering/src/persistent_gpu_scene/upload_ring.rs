use std::collections::BTreeMap;

use super::table::frame_bit;
use super::*;
use crate::MAX_FRAMES_IN_FLIGHT;

/// Destination record class for one persistent GPU-scene upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpuSceneUploadTarget {
    /// Shared prototype table.
    Prototype,
    /// Shared material table.
    Material,
    /// Shared deformation table.
    Deformation,
    /// Shared signed-distance-field table.
    Sdf,
    /// Shared page table.
    Page,
    /// Instance table owned by one world.
    Instance(GpuSceneWorldId),
    /// Light table owned by one world.
    Light(GpuSceneWorldId),
}

/// Typed record payload retained until a bounded upload batch is staged.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuSceneUploadPayload {
    /// A slot tombstone retaining only its generation header.
    Tombstone,
    /// Shared prototype write.
    Prototype(GpuScenePrototypeRecord),
    /// Shared material write.
    Material(GpuSceneMaterialRecord),
    /// Shared deformation write.
    Deformation(GpuSceneDeformationRecord),
    /// Shared signed-distance-field write.
    Sdf(GpuSceneSdfRecord),
    /// Shared page write.
    Page(GpuScenePageRecord),
    /// Per-world instance write.
    Instance(GpuSceneInstanceRecord),
    /// Per-world light write.
    Light(GpuSceneLightRecord),
}

impl GpuSceneUploadPayload {
    pub(super) fn byte_len(&self) -> usize {
        const HEADER_BYTES: usize = 16;
        HEADER_BYTES
            + match self {
                Self::Tombstone => 0,
                Self::Prototype(record) => {
                    size_of::<crate::GpuScenePrototypeGpuRecord>()
                        + record.materials.len() * size_of::<GpuHandle>()
                }
                Self::Material(_) | Self::Deformation(_) | Self::Sdf(_) => {
                    size_of::<crate::GpuSceneReferenceGpuRecord>()
                }
                Self::Page(_) => size_of::<crate::GpuScenePageGpuRecord>(),
                Self::Instance(record) => {
                    size_of::<crate::GpuSceneInstanceGpuRecord>()
                        + record.material_overrides.len()
                            * size_of::<crate::GpuSceneOverrideGpuRecord>()
                }
                Self::Light(_) => size_of::<crate::GpuSceneLightGpuRecord>(),
            }
    }
}

/// One direct-index table record in a staged upload range.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneUploadRecord {
    /// Slot generation written alongside the payload.
    pub generation: u32,
    /// Monotonic scene revision that produced the final coalesced value.
    pub revision: u64,
    /// Typed record or tombstone.
    pub payload: GpuSceneUploadPayload,
}

/// Consecutive records for one shared or per-world destination table.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneUploadRange {
    /// Destination table.
    pub target: GpuSceneUploadTarget,
    /// First direct-index slot.
    pub first_slot: u32,
    /// Consecutive records beginning at `first_slot`.
    pub records: Vec<GpuSceneUploadRecord>,
}

/// Hard upload limits applied per batch and per frame slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneUploadLimits {
    /// Maximum records returned by one staging call.
    pub max_batch_records: usize,
    /// Maximum estimated bytes returned by one staging call.
    pub max_batch_bytes: usize,
    /// Maximum records staged into one frame slot.
    pub max_frame_records: usize,
    /// Maximum estimated bytes staged into one frame slot.
    pub max_frame_bytes: usize,
}

impl Default for GpuSceneUploadLimits {
    fn default() -> Self {
        Self {
            max_batch_records: 4_096,
            max_batch_bytes: 4 * 1024 * 1024,
            max_frame_records: 65_536,
            max_frame_bytes: 64 * 1024 * 1024,
        }
    }
}

impl GpuSceneUploadLimits {
    pub(super) fn validate(self) -> Result<Self, GpuSceneError> {
        if self.max_batch_records == 0
            || self.max_batch_bytes == 0
            || self.max_frame_records == 0
            || self.max_frame_bytes == 0
        {
            return Err(GpuSceneError::InvalidUploadLimits(
                "record and byte limits must be nonzero",
            ));
        }
        if self.max_batch_records > self.max_frame_records
            || self.max_batch_bytes > self.max_frame_bytes
        {
            return Err(GpuSceneError::InvalidUploadLimits(
                "batch limits must not exceed frame limits",
            ));
        }
        if self.max_batch_bytes < 192 {
            return Err(GpuSceneError::InvalidUploadLimits(
                "batch byte limit must fit one dynamic instance record",
            ));
        }
        Ok(self)
    }
}

/// One bounded frame-slot upload batch.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneUploadBatch {
    /// Owning in-flight frame slot.
    pub frame_slot: usize,
    /// Consecutive destination ranges.
    pub ranges: Vec<GpuSceneUploadRange>,
    /// Total records in `ranges`.
    pub record_count: usize,
    /// Estimated table bytes in `ranges`.
    pub byte_count: usize,
    /// Whether coalesced changes remain for a later batch or frame.
    pub more_pending: bool,
    /// Whether the current frame slot exhausted a frame-wide limit.
    pub frame_budget_exhausted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct UploadKey {
    target: GpuSceneUploadTarget,
    slot: u32,
}

#[derive(Clone, Debug)]
pub(super) struct PendingUpload {
    generation: u32,
    revision: u64,
    payload: GpuSceneUploadPayload,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct UploadFrameState {
    begun: bool,
    records: usize,
    bytes: usize,
}

pub(super) struct GpuSceneUploadRing {
    pub(super) limits: GpuSceneUploadLimits,
    pub(super) pending: BTreeMap<UploadKey, PendingUpload>,
    pub(super) frames: [UploadFrameState; MAX_FRAMES_IN_FLIGHT],
}

impl GpuSceneUploadRing {
    pub(super) fn new(limits: GpuSceneUploadLimits) -> Result<Self, GpuSceneError> {
        Ok(Self {
            limits: limits.validate()?,
            pending: BTreeMap::new(),
            frames: [UploadFrameState::default(); MAX_FRAMES_IN_FLIGHT],
        })
    }

    pub(super) fn enqueue(
        &mut self,
        target: GpuSceneUploadTarget,
        handle: GpuHandle,
        revision: u64,
        payload: GpuSceneUploadPayload,
    ) -> Result<(), GpuSceneError> {
        let bytes = payload.byte_len();
        if bytes > self.limits.max_batch_bytes {
            return Err(GpuSceneError::RecordExceedsBatch {
                target,
                bytes,
                limit: self.limits.max_batch_bytes,
            });
        }
        self.pending.insert(
            UploadKey {
                target,
                slot: handle.index,
            },
            PendingUpload {
                generation: handle.generation,
                revision,
                payload,
            },
        );
        Ok(())
    }

    pub(super) fn begin_frame(&mut self, frame_slot: usize) -> Result<(), GpuSceneError> {
        frame_bit(frame_slot)?;
        self.frames[frame_slot] = UploadFrameState {
            begun: true,
            records: 0,
            bytes: 0,
        };
        Ok(())
    }

    pub(super) fn stage(
        &mut self,
        frame_slot: usize,
    ) -> Result<GpuSceneUploadBatch, GpuSceneError> {
        frame_bit(frame_slot)?;
        let frame = &self.frames[frame_slot];
        if !frame.begun {
            return Err(GpuSceneError::FrameNotBegun(frame_slot));
        }
        let record_limit = self
            .limits
            .max_batch_records
            .min(self.limits.max_frame_records - frame.records);
        let byte_limit = self
            .limits
            .max_batch_bytes
            .min(self.limits.max_frame_bytes - frame.bytes);
        let mut selected = Vec::new();
        let mut bytes = 0_usize;
        for (&key, upload) in &self.pending {
            let next_bytes = upload.payload.byte_len();
            let Some(total_bytes) = bytes.checked_add(next_bytes) else {
                break;
            };
            if selected.len() == record_limit || total_bytes > byte_limit {
                break;
            }
            bytes = total_bytes;
            selected.push(key);
        }
        let mut ranges: Vec<GpuSceneUploadRange> = Vec::new();
        for key in &selected {
            let upload = self
                .pending
                .remove(key)
                .expect("selected upload remains pending until batch assembly");
            let record = GpuSceneUploadRecord {
                generation: upload.generation,
                revision: upload.revision,
                payload: upload.payload,
            };
            match ranges.last_mut() {
                Some(range)
                    if range.target == key.target
                        && u64::from(range.first_slot) + range.records.len() as u64
                            == u64::from(key.slot) =>
                {
                    range.records.push(record);
                }
                _ => ranges.push(GpuSceneUploadRange {
                    target: key.target,
                    first_slot: key.slot,
                    records: vec![record],
                }),
            }
        }
        let record_count = selected.len();
        let frame = &mut self.frames[frame_slot];
        frame.records += record_count;
        frame.bytes += bytes;
        let frame_budget_exhausted = !self.pending.is_empty()
            && (frame.records == self.limits.max_frame_records
                || frame.bytes == self.limits.max_frame_bytes
                || record_count == 0);
        Ok(GpuSceneUploadBatch {
            frame_slot,
            ranges,
            record_count,
            byte_count: bytes,
            more_pending: !self.pending.is_empty(),
            frame_budget_exhausted,
        })
    }
}
