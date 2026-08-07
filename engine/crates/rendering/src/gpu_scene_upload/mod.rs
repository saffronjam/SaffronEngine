//! Frame upload translation from the persistent GPU scene to its device tables.
//!
//! [`GpuSceneUploader`] owns the device half of the GPU-scene mirror: slot-indexed tables
//! for the shared prototype/material/deformation/SDF/page records, per-world instance and
//! light tables, and the sparse per-instance override arena. Each frame it drains
//! [`PersistentGpuScene::stage_upload_batch`], serializes every coalesced record into its
//! locked device layout, stages the bytes through the shared [`FrameUploadRing`], and
//! enqueues graph-owned transfer passes with exact byte ranges. Buffer growth enqueues its
//! preserving copy before any write that targets the grown buffer, and a slot's occupancy
//! header travels in the same write as its record, so a slot is never observable
//! half-published.
//!
//! [`GpuScenePendingUploads`] is the companion queue for resident asset records: the
//! journal-driven mirror queues record stages, retirements, and arena byte uploads
//! (vertex/index streams, material parameter blocks) as it resolves assets, and
//! [`record_pending_global_uploads`] drains the queue into the same frame's transfer
//! passes.

mod pending;
mod tables;
mod uploader;

#[cfg(test)]
mod tests;

use bytemuck::{Pod, Zeroable};

pub use pending::{GpuArenaUploadRequest, GpuScenePendingUploads, record_pending_global_uploads};
pub use tables::{
    GpuSceneTableDescriptors, GpuSceneTableStorage, GpuSceneWorldDescriptors, GpuSceneWorldTables,
};
pub use uploader::GpuSceneUploader;

/// Initial slot capacity of the shared scene tables.
const INITIAL_SHARED_SLOTS: u64 = 1_024;
/// Initial slot capacity of a world's instance table.
const INITIAL_INSTANCE_SLOTS: u64 = 4_096;
/// Initial slot capacity of a world's light table.
const INITIAL_LIGHT_SLOTS: u64 = 256;
/// Initial element capacity of the override arena.
const INITIAL_OVERRIDE_ELEMENTS: u64 = 4_096;
/// Stride of one per-frame address-block slice: the smallest multiple of the
/// spec's maximum `minUniformBufferOffsetAlignment` (256) that covers the block.
const ADDRESS_BLOCK_ALIGNMENT: u64 = 512;

const _: () = assert!(size_of::<GpuSceneAddressBlock>() as u64 <= ADDRESS_BLOCK_ALIGNMENT);
/// Entry capacity of ONE VIEW CLASS's region in a frame slot's missing-page request
/// buffer — a per-class ceiling, not a shared one.
pub const PAGE_REQUEST_CAPACITY: u32 = 4_096;
/// Bytes of one request entry: the resident page-table slot. The class is the region the
/// entry sits in, so carrying it again would be a second copy that could disagree with
/// the first.
const PAGE_REQUEST_ENTRY_BYTES: usize = 4;
/// Bytes before the first region: one count word per class.
const PAGE_REQUEST_HEADER_BYTES: usize = crate::SCENE_VIEW_CLASSES * 4;
/// Byte size of one frame slot's request slice: the counts plus one region per class.
const PAGE_REQUEST_SLOT_BYTES: u64 = PAGE_REQUEST_HEADER_BYTES as u64
    + crate::SCENE_VIEW_CLASSES as u64
        * PAGE_REQUEST_CAPACITY as u64
        * PAGE_REQUEST_ENTRY_BYTES as u64;

/// One frame's drained missing-page requests, and what the buffer could not hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageRequestDrain {
    /// Requested page-table slots, deduplicated, each carrying the most urgent class that
    /// missed it — one page cannot be streamed twice, and the cheapest reader must not be
    /// what its priority is set from. Most urgent class first.
    pub requests: Vec<(u32, crate::SceneViewClass)>,
    /// Requests no region had room for, summed over the classes: demand that was raised
    /// and lost, so the page faults again next frame, later than it needed to.
    pub dropped: u32,
    /// Bit per class whose region filled ([`crate::SceneViewClass::bit`]). WHICH class
    /// overflowed is the whole diagnostic — the camera's is a stall in the image, a
    /// gather's is a slightly thinner gather.
    pub overflow_classes: u32,
}

/// Buffer device addresses of every GPU-scene table for one frame, bound as one uniform
/// block so growth never rewrites a descriptor. A zero address with zero capacity marks a
/// table that holds nothing this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuSceneAddressBlock {
    /// Resident prototype table.
    pub prototypes: u64,
    /// Resident geometry table.
    pub geometries: u64,
    /// Resident material table.
    pub materials: u64,
    /// Resident texture table.
    pub textures: u64,
    /// Resident coverage table.
    pub coverage: u64,
    /// Resident skeleton table.
    pub skeletons: u64,
    /// Baked signed-distance-field table.
    pub sdfs: u64,
    /// Resident page table.
    pub pages: u64,
    /// Shared scene prototype table.
    pub scene_prototypes: u64,
    /// Shared scene material-reference table.
    pub scene_materials: u64,
    /// Shared scene deformation-reference table.
    pub scene_deformations: u64,
    /// Shared scene SDF-reference table.
    pub scene_sdfs: u64,
    /// Shared scene page table.
    pub scene_pages: u64,
    /// The bound world's instance table.
    pub instances: u64,
    /// The bound world's light table.
    pub lights: u64,
    /// Per-instance override arena.
    pub overrides: u64,
    /// Prototype material-handle arena.
    pub prototype_materials: u64,
    /// Per-prototype packed scene-SDF handle arena.
    pub prototype_sdfs: u64,
    /// Material parameter-block arena.
    pub material_parameters: u64,
    /// Global vertex byte arena.
    pub vertices: u64,
    /// Global index byte arena.
    pub indices: u64,
    /// Plant assembly-part records arena (per-prototype vertex bases + per-use
    /// transforms).
    pub parts: u64,
    /// Geometry submesh-record arena.
    pub submesh_table: u64,
    /// Global page-payload byte arena.
    pub page_bytes: u64,
    /// This frame slot's missing-page request buffer (count word + slot entries).
    pub page_requests: u64,
    /// Stable deformed-vertex output arena.
    pub deformed_vertices: u64,
    /// Previous-frame deformed-vertex arena.
    pub prev_deformed_vertices: u64,
    /// Deformation-provider records arena.
    pub deformation_providers: u64,
    /// Provider parameter words arena.
    pub deformation_parameters: u64,
    /// Micro vegetation field-tile arena (headers + density samples + directory).
    pub fields: u64,
    /// The frame-transient micro-blade candidates buffer.
    pub candidates: u64,
    /// Per-instance-slot wind sway records the wind deformation prepass writes.
    pub wind_records: u64,
    /// The world interaction field (header + damped-oscillator texel cascades).
    pub interaction_field: u64,
    /// Fragment-side coverage counters, or 0 when nothing is measuring.
    ///
    /// A helper invocation's stores and atomics are discarded by the spec, so an atomic here counts
    /// COVERED samples while the `FRAGMENT_SHADER_INVOCATIONS` pipeline statistic counts every lane
    /// including helpers. The ratio is quad utilization, which no pipeline statistic reports on its
    /// own. Zero disables the increment through the same idiom every other optional address uses,
    /// so an unprofiled frame pays nothing. Also keeps the block's 16-byte alignment.
    pub quad_counters: u64,
    /// The frame's amplified displaced micro-vertex arena, or 0 when nothing displaces.
    pub displaced_vertices: u64,
    /// The parallel previous-frame micro-vertex arena (the geomorph slide the motion pass reads).
    pub displaced_prev_vertices: u64,
    /// The amplified index stream, bound as the index buffer for the displaced draw buckets.
    pub displaced_indices: u64,
    /// Per-row `VkDrawIndexedIndirectCommand` seeds the binner builds displaced commands from.
    pub displaced_draws: u64,
    /// Slot-sorted (instance slot, arena row) pairs the traversal looks displaced instances up in.
    pub displaced_rows: u64,
    /// This frame's ray-instance records, indexed by a traced candidate's `instanceCustomIndex`.
    pub ray_instances: u64,
    /// Slot capacity of the bound world's instance table.
    pub instance_capacity: u32,
    /// Slot capacity of the bound world's light table.
    pub light_capacity: u32,
    /// Deterministic temporal phase for canonical-coverage classification this frame.
    pub coverage_temporal_phase: u32,
    /// Allocated entry capacity of one view class's request region — the region stride.
    pub page_request_capacity: u32,
    /// Entries a class may actually append this frame, at most the capacity. Addressing must
    /// never use it, or every region would move the moment it changed.
    pub page_request_budget: u32,
    /// Request regions in the buffer, one per [`crate::SceneViewClass`]. Carried rather
    /// than mirrored as a shader constant so the two halves cannot drift apart.
    pub page_request_classes: u32,
    /// Entries in [`GpuSceneAddressBlock::displaced_rows`].
    pub displaced_row_count: u32,
    /// Addressable entries in [`GpuSceneAddressBlock::ray_instances`]; a candidate index at or
    /// above it resolves nothing. The table and the structure that indexes it are written together
    /// per frame slot, so every index a traced candidate can carry names a record from that write.
    pub ray_instance_count: u32,
    /// Padding to the block's 16-byte alignment. Named because `bytemuck::Pod` will not
    /// accept a struct whose padding is implicit.
    pub reserved: [u32; 4],
}

const _: () = assert!(
    size_of::<GpuSceneAddressBlock>() == 368,
    "the GPU-scene address block must match the locked std430 layout"
);

/// Counters for one frame's upload translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuSceneUploadRunStats {
    /// Batches drained from the persistent scene's upload ring.
    pub batches: u32,
    /// Slot records staged.
    pub records: u32,
    /// Estimated table bytes staged.
    pub table_bytes: u64,
    /// Buffer growths enqueued.
    pub growths: u32,
    /// Whether coalesced writes remain for a later frame after the frame budget.
    pub budget_exhausted: bool,
}
