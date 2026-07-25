//! Persistent renderer-derived scene records and bounded delta uploads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

use saffron_geometry::glam::Mat4;
use saffron_spatial::{DecisionScalar, QuantizedOrientation, WorldPosition};

use crate::{GpuHandle, GpuLight, MAX_FRAMES_IN_FLIGHT};

/// Error returned when a GPU-scene delta or upload request violates the scene contract.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GpuSceneError {
    /// A frame slot is outside the renderer's frame ring.
    #[error("GPU Scene frame slot {slot} exceeds the {count}-slot ring")]
    FrameSlot { slot: usize, count: usize },
    /// A frame slot must be released by its fence before staging more work into it.
    #[error("GPU Scene frame slot {0} has not begun after fence completion")]
    FrameNotBegun(usize),
    /// An upload limit is zero or internally inconsistent.
    #[error("invalid GPU Scene upload limits: {0}")]
    InvalidUploadLimits(&'static str),
    /// One record can never fit in a configured batch.
    #[error(
        "GPU Scene {target:?} record needs {bytes} bytes, exceeding the {limit}-byte batch limit"
    )]
    RecordExceedsBatch {
        /// Destination record class.
        target: GpuSceneUploadTarget,
        /// Required bytes.
        bytes: usize,
        /// Configured maximum bytes.
        limit: usize,
    },
    /// A supplied handle is stale or belongs to a vacant slot.
    #[error("stale GPU Scene {kind} handle {handle:?}")]
    StaleHandle {
        /// Record class.
        kind: &'static str,
        /// Raw handle value.
        handle: GpuHandle,
    },
    /// A caller-provided world identifier already exists.
    #[error("GPU Scene world {0:?} already exists")]
    DuplicateWorld(GpuSceneWorldId),
    /// A caller-provided world identifier is absent.
    #[error("GPU Scene world {0:?} does not exist")]
    MissingWorld(GpuSceneWorldId),
    /// A caller-provided view identifier already exists.
    #[error("GPU Scene view {0:?} already exists")]
    DuplicateView(GpuSceneViewId),
    /// A caller-provided view identifier is absent.
    #[error("GPU Scene view {0:?} does not exist")]
    MissingView(GpuSceneViewId),
    /// A record cannot be removed while another live record references it.
    #[error("GPU Scene {kind} handle {handle:?} is still referenced")]
    ReferencedHandle {
        /// Record class.
        kind: &'static str,
        /// Raw handle value.
        handle: GpuHandle,
    },
    /// Sparse material overrides must be strictly ordered and unique.
    #[error("GPU Scene material overrides must be strictly ordered by slot")]
    MaterialOverrideOrder,
    /// A sparse override addresses no slot in its prototype material set.
    #[error("GPU Scene material override slot {slot} exceeds the prototype's {count} slots")]
    MaterialOverrideSlot {
        /// Invalid material slot.
        slot: u32,
        /// Material-slot count.
        count: usize,
    },
    /// A transform or bounds payload contains a non-finite value.
    #[error("GPU Scene {0} contains a non-finite value")]
    NonFinite(&'static str),
    /// A bounding sphere carries a negative radius.
    #[error("GPU Scene prototype bounds radius must be non-negative")]
    NegativeBoundsRadius,
    /// A page would become its own ancestor.
    #[error("GPU Scene page hierarchy contains a cycle")]
    PageCycle,
    /// A snapshot contains an invalid or duplicate slot.
    #[error("invalid GPU Scene snapshot: {0}")]
    InvalidSnapshot(&'static str),
    /// The scene revision cannot advance further.
    #[error("GPU Scene revision overflowed")]
    RevisionOverflow,
    /// The table has no representable slot index left.
    #[error("GPU Scene {0} table exceeds u32 slots")]
    TableCapacity(&'static str),
    /// A world cannot be removed while it owns records or views.
    #[error("GPU Scene world {0:?} is not empty")]
    WorldNotEmpty(GpuSceneWorldId),
}

/// Typed stable slot and generation used by the persistent GPU Scene.
#[repr(transparent)]
pub struct GpuSceneHandle<K> {
    raw: GpuHandle,
    marker: PhantomData<fn() -> K>,
}

impl<K> GpuSceneHandle<K> {
    fn from_raw(raw: GpuHandle) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Returns the byte-locked GPU handle representation.
    #[must_use]
    pub const fn raw(self) -> GpuHandle {
        self.raw
    }
}

impl<K> Clone for GpuSceneHandle<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for GpuSceneHandle<K> {}

impl<K> Default for GpuSceneHandle<K> {
    fn default() -> Self {
        Self::from_raw(GpuHandle::INVALID)
    }
}

impl<K> fmt::Debug for GpuSceneHandle<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.raw.fmt(formatter)
    }
}

impl<K> PartialEq for GpuSceneHandle<K> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<K> Eq for GpuSceneHandle<K> {}

impl<K> PartialOrd for GpuSceneHandle<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K> Ord for GpuSceneHandle<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<K> Hash for GpuSceneHandle<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

/// Prototype-handle marker.
pub enum GpuScenePrototypeKind {}
/// Material-handle marker.
pub enum GpuSceneMaterialKind {}
/// Instance-handle marker.
pub enum GpuSceneInstanceKind {}
/// Deformation-handle marker.
pub enum GpuSceneDeformationKind {}
/// Light-handle marker.
pub enum GpuSceneLightKind {}
/// signed-distance-field-handle marker.
pub enum GpuSceneSdfKind {}
/// Page-handle marker.
pub enum GpuScenePageKind {}

/// Stable prototype handle.
pub type GpuScenePrototypeHandle = GpuSceneHandle<GpuScenePrototypeKind>;
/// Stable material handle.
pub type GpuSceneMaterialHandle = GpuSceneHandle<GpuSceneMaterialKind>;
/// Stable per-world instance handle.
pub type GpuSceneInstanceHandle = GpuSceneHandle<GpuSceneInstanceKind>;
/// Stable deformation-provider handle.
pub type GpuSceneDeformationHandle = GpuSceneHandle<GpuSceneDeformationKind>;
/// Stable per-world light handle.
pub type GpuSceneLightHandle = GpuSceneHandle<GpuSceneLightKind>;
/// Stable signed-distance-field handle.
pub type GpuSceneSdfHandle = GpuSceneHandle<GpuSceneSdfKind>;
/// Stable page handle.
pub type GpuScenePageHandle = GpuSceneHandle<GpuScenePageKind>;

/// Caller-owned world key; it never replaces entity or plant identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuSceneWorldId(pub u64);

/// Caller-owned view key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuSceneViewId(pub u64);

/// A compact exact transform for static placement points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C, align(16))]
pub struct GpuSceneStaticTransform {
    /// Signed level-zero world-cell coordinates.
    pub cell: [i64; 3],
    /// Half-open cell-local position ticks.
    pub local_ticks: [u32; 3],
    /// Quantized quaternion in XYZW order.
    pub orientation: [i16; 4],
    /// Q15.16 three-axis scale.
    pub scale: [i32; 3],
    /// Renderer-derived transform flags.
    pub flags: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

const _: () = assert!(std::mem::size_of::<GpuSceneStaticTransform>() == 64);

impl GpuSceneStaticTransform {
    /// Packs canonical large-world placement values without float conversion.
    #[must_use]
    pub fn new(
        position: WorldPosition,
        orientation: QuantizedOrientation,
        scale: [DecisionScalar; 3],
        flags: u32,
    ) -> Self {
        Self {
            cell: position.cell().coordinates(),
            local_ticks: position.local().ticks(),
            orientation: orientation.bits(),
            scale: scale.map(DecisionScalar::bits),
            flags,
            reserved: 0,
        }
    }
}

/// Separate current and previous transforms for dynamic instances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuSceneDynamicTransform {
    /// Current world transform.
    pub current: Mat4,
    /// Previous published world transform.
    pub previous: Mat4,
}

impl GpuSceneDynamicTransform {
    /// Starts a dynamic transform without initial motion.
    pub fn stationary(current: Mat4) -> Result<Self, GpuSceneError> {
        Self::new(current, current)
    }

    /// Constructs an explicitly paired current/previous payload.
    pub fn new(current: Mat4, previous: Mat4) -> Result<Self, GpuSceneError> {
        if !matrix_is_finite(current) || !matrix_is_finite(previous) {
            return Err(GpuSceneError::NonFinite("dynamic transform"));
        }
        Ok(Self { current, previous })
    }

    /// Advances current to previous and publishes a new current transform.
    pub fn advance(&mut self, current: Mat4) -> Result<(), GpuSceneError> {
        if !matrix_is_finite(current) {
            return Err(GpuSceneError::NonFinite("dynamic transform"));
        }
        self.previous = self.current;
        self.current = current;
        Ok(())
    }
}

fn matrix_is_finite(matrix: Mat4) -> bool {
    matrix.to_cols_array().into_iter().all(f32::is_finite)
}

/// Static compact or dynamic temporal transform storage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuSceneTransform {
    /// Exact compact point placement.
    Static(GpuSceneStaticTransform),
    /// Current/previous float matrices for moving objects.
    Dynamic(GpuSceneDynamicTransform),
}

/// One sparse per-object material replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneMaterialOverride {
    /// Prototype material slot.
    pub slot: u32,
    /// Replacement immutable material reference.
    pub material: GpuSceneMaterialHandle,
}

/// Immutable material-table reference shared by any number of instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneMaterialRecord {
    /// Device-global material-table handle.
    pub table: GpuHandle,
    /// Source asset revision represented by this record.
    pub source_revision: u64,
}

/// Immutable deformation-provider reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneDeformationRecord {
    /// Device-global deformation provider handle.
    pub provider: GpuHandle,
    /// Source asset revision represented by this record.
    pub source_revision: u64,
}

/// Immutable signed-distance-field reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneSdfRecord {
    /// Device-global SDF resource handle.
    pub resource: GpuHandle,
    /// Source asset revision represented by this record.
    pub source_revision: u64,
}

/// Immutable resident-page reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuScenePageRecord {
    /// Device-global page-table handle.
    pub table: GpuHandle,
    /// Resident parent required before this page can publish.
    pub parent: Option<GpuScenePageHandle>,
    /// Source content generation.
    pub source_generation: u32,
    /// Page flags.
    pub flags: u32,
}

/// Immutable render prototype shared across worlds.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuScenePrototypeRecord {
    /// Device-global geometry-table handle.
    pub geometry: GpuHandle,
    /// Ordered material-set references.
    pub materials: Arc<[GpuSceneMaterialHandle]>,
    /// Optional shared deformation definition.
    pub deformation: Option<GpuSceneDeformationHandle>,
    /// Optional shared SDF resource.
    pub sdf: Option<GpuSceneSdfHandle>,
    /// Guaranteed-resident hierarchy root.
    pub root_page: GpuScenePageHandle,
    /// Conservative object-space bounding sphere.
    pub bounds: [f32; 4],
    /// Source content generation.
    pub source_generation: u32,
    /// Prototype flags.
    pub flags: u32,
}

/// Mutable per-world instance record.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneInstanceRecord {
    /// Shared immutable prototype.
    pub prototype: GpuScenePrototypeHandle,
    /// Static or dynamic transform payload.
    pub transform: GpuSceneTransform,
    /// Strictly ordered sparse material replacements.
    pub material_overrides: Arc<[GpuSceneMaterialOverride]>,
    /// Optional instance-specific deformation output.
    pub deformation: Option<GpuSceneDeformationHandle>,
    /// Optional instance-specific SDF.
    pub sdf: Option<GpuSceneSdfHandle>,
    /// Source scene/cell generation.
    pub source_generation: u32,
    /// Instance flags.
    pub flags: u32,
    /// The assembly (variation, phenotype) combination index masking the prototype's
    /// uses (`0` for every non-assembly instance).
    pub combination: u32,
    /// Vegetation columns packed into the static payload's free words (`None` for
    /// every non-vegetation instance).
    pub vegetation: Option<GpuSceneVegetationColumns>,
}

/// Per-plant columns a static vegetation instance uploads beside its compact
/// transform: conservative bounds spheres and the stable surface attachment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuSceneVegetationColumns {
    /// Current conservative bounds sphere in instance-local pre-scale space
    /// (center xyz + radius); the cull composes it through the transform.
    pub bounds_current: [f32; 4],
    /// Previous conservative bounds sphere in the same space.
    pub bounds_previous: [f32; 4],
    /// Stable surface attachment identity when the point is surface-attached.
    pub attachment: Option<GpuSceneAttachmentColumns>,
    /// The combination this instance rendered before its latest flip; equals the
    /// record's combination outside a crossfade.
    pub combination_previous: u32,
    /// The frame stamp of the latest combination flip (the traversal derives the
    /// crossfade phase from `frame_stamp - flip_stamp`).
    pub flip_stamp: u32,
}

/// The packed identity of one point-to-surface attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneAttachmentColumns {
    /// Surface provider identity.
    pub provider: u64,
    /// Primitive identity within the provider.
    pub primitive: u64,
    /// Canonical triangle barycentrics (the three sum to `u16::MAX`).
    pub barycentric: [u16; 3],
}

/// Mutable per-world punctual-light record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuSceneLightRecord {
    /// Byte-locked renderer light payload.
    pub light: GpuLight,
    /// Source scene revision.
    pub source_revision: u64,
}

struct SceneSlot<T> {
    generation: u32,
    value: Option<T>,
}

struct RetiredSceneSlot {
    index: u32,
    pending_frames: u64,
}

struct SceneTable<T, K> {
    slots: Vec<SceneSlot<T>>,
    reusable: Vec<u32>,
    retired: Vec<RetiredSceneSlot>,
    marker: PhantomData<fn() -> K>,
}

impl<T, K> Default for SceneTable<T, K> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            reusable: Vec::new(),
            retired: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<T, K> SceneTable<T, K> {
    fn insert(&mut self, value: T, kind: &'static str) -> Result<GpuSceneHandle<K>, GpuSceneError> {
        let index = if let Some(index) = self.reusable.pop() {
            index
        } else {
            let index =
                u32::try_from(self.slots.len()).map_err(|_| GpuSceneError::TableCapacity(kind))?;
            self.slots.push(SceneSlot {
                generation: 1,
                value: None,
            });
            index
        };
        let slot = &mut self.slots[index as usize];
        debug_assert!(slot.value.is_none());
        slot.value = Some(value);
        Ok(GpuSceneHandle::from_raw(GpuHandle {
            index,
            generation: slot.generation,
        }))
    }

    fn get(&self, handle: GpuSceneHandle<K>) -> Option<&T> {
        let slot = self.slots.get(handle.raw.index as usize)?;
        (slot.generation == handle.raw.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    fn update(
        &mut self,
        handle: GpuSceneHandle<K>,
        value: T,
        kind: &'static str,
    ) -> Result<(), GpuSceneError> {
        let slot = self
            .slots
            .get_mut(handle.raw.index as usize)
            .filter(|slot| slot.generation == handle.raw.generation)
            .ok_or(GpuSceneError::StaleHandle {
                kind,
                handle: handle.raw,
            })?;
        let stored = slot.value.as_mut().ok_or(GpuSceneError::StaleHandle {
            kind,
            handle: handle.raw,
        })?;
        *stored = value;
        Ok(())
    }

    fn remove(
        &mut self,
        handle: GpuSceneHandle<K>,
        kind: &'static str,
    ) -> Result<T, GpuSceneError> {
        let slot = self
            .slots
            .get_mut(handle.raw.index as usize)
            .filter(|slot| slot.generation == handle.raw.generation)
            .ok_or(GpuSceneError::StaleHandle {
                kind,
                handle: handle.raw,
            })?;
        let value = slot.value.take().ok_or(GpuSceneError::StaleHandle {
            kind,
            handle: handle.raw,
        })?;
        self.retired.push(RetiredSceneSlot {
            index: handle.raw.index,
            pending_frames: live_frame_mask(),
        });
        Ok(value)
    }

    fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<(), GpuSceneError> {
        let completed = frame_bit(completed_frame_slot)?;
        let mut ready = Vec::new();
        self.retired.retain_mut(|retired| {
            retired.pending_frames &= !completed;
            if retired.pending_frames == 0 {
                ready.push(retired.index);
                false
            } else {
                true
            }
        });
        for index in ready {
            let slot = &mut self.slots[index as usize];
            if let Some(generation) = slot.generation.checked_add(1) {
                slot.generation = generation;
                self.reusable.push(index);
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.slots.iter().all(|slot| slot.value.is_none())
    }

    fn iter(&self) -> impl Iterator<Item = (GpuSceneHandle<K>, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    GpuSceneHandle::from_raw(GpuHandle {
                        index: index as u32,
                        generation: slot.generation,
                    }),
                    value,
                )
            })
        })
    }
}

fn live_frame_mask() -> u64 {
    (1_u64 << MAX_FRAMES_IN_FLIGHT) - 1
}

fn frame_bit(frame_slot: usize) -> Result<u64, GpuSceneError> {
    if frame_slot >= MAX_FRAMES_IN_FLIGHT || frame_slot >= u64::BITS as usize {
        return Err(GpuSceneError::FrameSlot {
            slot: frame_slot,
            count: MAX_FRAMES_IN_FLIGHT,
        });
    }
    Ok(1_u64 << frame_slot)
}

/// One slot in a complete reconstructible table snapshot.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuSceneSnapshotSlot<T> {
    /// Vacant slot carrying the last generation that occupied it.
    Vacant { generation: u32 },
    /// Live slot and immutable snapshot value.
    Occupied { generation: u32, value: T },
}

/// Complete table snapshot preserving live handles and stale-handle rejection.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSceneTableSnapshot<T> {
    /// Direct-index slots.
    pub slots: Vec<GpuSceneSnapshotSlot<T>>,
}

impl<T> Default for GpuSceneTableSnapshot<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T: Clone, K> SceneTable<T, K> {
    fn snapshot(&self) -> GpuSceneTableSnapshot<T> {
        GpuSceneTableSnapshot {
            slots: self
                .slots
                .iter()
                .map(|slot| match &slot.value {
                    Some(value) => GpuSceneSnapshotSlot::Occupied {
                        generation: slot.generation,
                        value: value.clone(),
                    },
                    None => GpuSceneSnapshotSlot::Vacant {
                        generation: slot.generation,
                    },
                })
                .collect(),
        }
    }

    fn from_snapshot(snapshot: GpuSceneTableSnapshot<T>) -> Result<Self, GpuSceneError> {
        let mut table = Self::default();
        for (index, snapshot_slot) in snapshot.slots.into_iter().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| GpuSceneError::InvalidSnapshot("table exceeds u32 slots"))?;
            match snapshot_slot {
                GpuSceneSnapshotSlot::Vacant { generation } => {
                    if generation == 0 {
                        return Err(GpuSceneError::InvalidSnapshot("zero slot generation"));
                    }
                    let reusable_generation = generation.checked_add(1);
                    table.slots.push(SceneSlot {
                        generation: reusable_generation.unwrap_or(generation),
                        value: None,
                    });
                    if reusable_generation.is_some() {
                        table.reusable.push(index);
                    }
                }
                GpuSceneSnapshotSlot::Occupied { generation, value } => {
                    if generation == 0 {
                        return Err(GpuSceneError::InvalidSnapshot("zero slot generation"));
                    }
                    table.slots.push(SceneSlot {
                        generation,
                        value: Some(value),
                    });
                }
            }
        }
        table.reusable.reverse();
        Ok(table)
    }
}

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
    fn byte_len(&self) -> usize {
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
    fn validate(self) -> Result<Self, GpuSceneError> {
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
struct UploadKey {
    target: GpuSceneUploadTarget,
    slot: u32,
}

#[derive(Clone, Debug)]
struct PendingUpload {
    generation: u32,
    revision: u64,
    payload: GpuSceneUploadPayload,
}

#[derive(Clone, Copy, Debug, Default)]
struct UploadFrameState {
    begun: bool,
    records: usize,
    bytes: usize,
}

struct GpuSceneUploadRing {
    limits: GpuSceneUploadLimits,
    pending: BTreeMap<UploadKey, PendingUpload>,
    frames: [UploadFrameState; MAX_FRAMES_IN_FLIGHT],
}

impl GpuSceneUploadRing {
    fn new(limits: GpuSceneUploadLimits) -> Result<Self, GpuSceneError> {
        Ok(Self {
            limits: limits.validate()?,
            pending: BTreeMap::new(),
            frames: [UploadFrameState::default(); MAX_FRAMES_IN_FLIGHT],
        })
    }

    fn enqueue(
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

    fn begin_frame(&mut self, frame_slot: usize) -> Result<(), GpuSceneError> {
        frame_bit(frame_slot)?;
        self.frames[frame_slot] = UploadFrameState {
            begun: true,
            records: 0,
            bytes: 0,
        };
        Ok(())
    }

    fn stage(&mut self, frame_slot: usize) -> Result<GpuSceneUploadBatch, GpuSceneError> {
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

/// Delta for shared immutable GPU-scene records.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuSceneSharedDelta {
    /// Creates a prototype.
    CreatePrototype(GpuScenePrototypeRecord),
    /// Rewrites one stable prototype slot.
    UpdatePrototype {
        /// Existing handle.
        handle: GpuScenePrototypeHandle,
        /// Complete replacement value.
        record: GpuScenePrototypeRecord,
    },
    /// Removes an unreferenced prototype.
    RemovePrototype(GpuScenePrototypeHandle),
    /// Creates a material reference.
    CreateMaterial(GpuSceneMaterialRecord),
    /// Rewrites one stable material slot.
    UpdateMaterial {
        /// Existing handle.
        handle: GpuSceneMaterialHandle,
        /// Complete replacement value.
        record: GpuSceneMaterialRecord,
    },
    /// Removes an unreferenced material.
    RemoveMaterial(GpuSceneMaterialHandle),
    /// Creates a deformation reference.
    CreateDeformation(GpuSceneDeformationRecord),
    /// Rewrites one stable deformation slot.
    UpdateDeformation {
        /// Existing handle.
        handle: GpuSceneDeformationHandle,
        /// Complete replacement value.
        record: GpuSceneDeformationRecord,
    },
    /// Removes an unreferenced deformation.
    RemoveDeformation(GpuSceneDeformationHandle),
    /// Creates an SDF reference.
    CreateSdf(GpuSceneSdfRecord),
    /// Rewrites one stable SDF slot.
    UpdateSdf {
        /// Existing handle.
        handle: GpuSceneSdfHandle,
        /// Complete replacement value.
        record: GpuSceneSdfRecord,
    },
    /// Removes an unreferenced SDF.
    RemoveSdf(GpuSceneSdfHandle),
    /// Creates a page reference.
    CreatePage(GpuScenePageRecord),
    /// Rewrites one stable page slot.
    UpdatePage {
        /// Existing handle.
        handle: GpuScenePageHandle,
        /// Complete replacement value.
        record: GpuScenePageRecord,
    },
    /// Removes an unreferenced page.
    RemovePage(GpuScenePageHandle),
}

/// Result of applying one shared delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuSceneSharedDeltaResult {
    /// Prototype creation result.
    PrototypeCreated(GpuScenePrototypeHandle),
    /// Material creation result.
    MaterialCreated(GpuSceneMaterialHandle),
    /// Deformation creation result.
    DeformationCreated(GpuSceneDeformationHandle),
    /// SDF creation result.
    SdfCreated(GpuSceneSdfHandle),
    /// Page creation result.
    PageCreated(GpuScenePageHandle),
    /// An existing slot was rewritten.
    Updated,
    /// An existing slot was retired.
    Removed,
}

/// Delta for records in one caller-selected world.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuSceneWorldDelta {
    /// Creates an instance.
    CreateInstance(GpuSceneInstanceRecord),
    /// Rewrites one stable instance slot.
    UpdateInstance {
        /// Existing handle.
        handle: GpuSceneInstanceHandle,
        /// Complete replacement value.
        record: GpuSceneInstanceRecord,
    },
    /// Removes an instance.
    RemoveInstance(GpuSceneInstanceHandle),
    /// Creates a punctual light.
    CreateLight(GpuSceneLightRecord),
    /// Rewrites one stable light slot.
    UpdateLight {
        /// Existing handle.
        handle: GpuSceneLightHandle,
        /// Complete replacement value.
        record: GpuSceneLightRecord,
    },
    /// Removes a light.
    RemoveLight(GpuSceneLightHandle),
}

/// Result of applying one per-world delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuSceneWorldDeltaResult {
    /// Instance creation result.
    InstanceCreated(GpuSceneInstanceHandle),
    /// Light creation result.
    LightCreated(GpuSceneLightHandle),
    /// An existing slot was rewritten.
    Updated,
    /// An existing slot was retired.
    Removed,
}

#[derive(Default)]
struct GpuSceneSharedRecords {
    prototypes: SceneTable<GpuScenePrototypeRecord, GpuScenePrototypeKind>,
    materials: SceneTable<GpuSceneMaterialRecord, GpuSceneMaterialKind>,
    deformations: SceneTable<GpuSceneDeformationRecord, GpuSceneDeformationKind>,
    sdfs: SceneTable<GpuSceneSdfRecord, GpuSceneSdfKind>,
    pages: SceneTable<GpuScenePageRecord, GpuScenePageKind>,
}

#[derive(Default)]
struct GpuSceneWorld {
    instances: SceneTable<GpuSceneInstanceRecord, GpuSceneInstanceKind>,
    lights: SceneTable<GpuSceneLightRecord, GpuSceneLightKind>,
    revision: u64,
}

/// Reason a view discards temporal visibility and HZB history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuSceneHistoryInvalidation {
    /// The view has no prior frame.
    NewView,
    /// Camera projection or pose discontinuity.
    CameraCut,
    /// Render extent changed.
    Resize,
    /// Exact render origin changed.
    OriginShift,
    /// Page or representation generations changed.
    RepresentationChange,
    /// The shared GPU Scene was rebuilt.
    SceneRebuild,
}

/// View-local temporal, visibility, HZB, and command preparation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneViewState {
    /// World observed by this view.
    pub world: GpuSceneWorldId,
    /// Shared-scene revision consumed by the latest preparation.
    pub shared_revision: u64,
    /// World revision consumed by the latest preparation.
    pub world_revision: u64,
    /// Monotonic visibility-output revision.
    pub visibility_revision: u64,
    /// Monotonic HZB publication revision.
    pub hzb_revision: u64,
    /// Monotonic indirect-command publication revision.
    pub command_revision: u64,
    /// Temporal history generation.
    pub history_generation: u64,
    /// Whether previous visibility/HZB data may be consumed.
    pub history_valid: bool,
    /// Most recent invalidation reason.
    pub invalidation: GpuSceneHistoryInvalidation,
}

impl GpuSceneViewState {
    fn new(world: GpuSceneWorldId) -> Self {
        Self {
            world,
            shared_revision: 0,
            world_revision: 0,
            visibility_revision: 0,
            hzb_revision: 0,
            command_revision: 0,
            history_generation: 1,
            history_valid: false,
            invalidation: GpuSceneHistoryInvalidation::NewView,
        }
    }

    /// Invalidates all temporal data while preserving shared scene records.
    pub fn invalidate(&mut self, reason: GpuSceneHistoryInvalidation) -> Result<(), GpuSceneError> {
        self.history_generation = self
            .history_generation
            .checked_add(1)
            .ok_or(GpuSceneError::RevisionOverflow)?;
        self.history_valid = false;
        self.invalidation = reason;
        Ok(())
    }

    /// Publishes independently versioned visibility, HZB, and command state.
    pub fn publish(
        &mut self,
        shared_revision: u64,
        world_revision: u64,
    ) -> Result<(), GpuSceneError> {
        self.shared_revision = shared_revision;
        self.world_revision = world_revision;
        self.visibility_revision = increment(self.visibility_revision)?;
        self.hzb_revision = increment(self.hzb_revision)?;
        self.command_revision = increment(self.command_revision)?;
        self.history_valid = true;
        Ok(())
    }
}

fn increment(value: u64) -> Result<u64, GpuSceneError> {
    value.checked_add(1).ok_or(GpuSceneError::RevisionOverflow)
}

/// Complete shared-table snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSceneSharedSnapshot {
    /// Prototype slots.
    pub prototypes: GpuSceneTableSnapshot<GpuScenePrototypeRecord>,
    /// Material slots.
    pub materials: GpuSceneTableSnapshot<GpuSceneMaterialRecord>,
    /// Deformation slots.
    pub deformations: GpuSceneTableSnapshot<GpuSceneDeformationRecord>,
    /// SDF slots.
    pub sdfs: GpuSceneTableSnapshot<GpuSceneSdfRecord>,
    /// Page slots.
    pub pages: GpuSceneTableSnapshot<GpuScenePageRecord>,
}

/// Complete snapshot of one per-world store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSceneWorldSnapshot {
    /// Caller-owned world identifier.
    pub id: GpuSceneWorldId,
    /// Instance slots.
    pub instances: GpuSceneTableSnapshot<GpuSceneInstanceRecord>,
    /// Light slots.
    pub lights: GpuSceneTableSnapshot<GpuSceneLightRecord>,
    /// Last derived world revision.
    pub revision: u64,
}

/// Reconstructible persistent GPU-scene snapshot; view history is deliberately excluded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistentGpuSceneSnapshot {
    /// Shared immutable records.
    pub shared: GpuSceneSharedSnapshot,
    /// Independent per-world records.
    pub worlds: Vec<GpuSceneWorldSnapshot>,
    /// Last derived shared revision.
    pub revision: u64,
}

/// One moved instance's conservative swept world AABB (min, max).
pub type GpuSceneMovedBounds = ([f32; 3], [f32; 3]);

/// Persistent derived render mirror over canonical ECS, asset, and vegetation state.
pub struct PersistentGpuScene {
    shared: GpuSceneSharedRecords,
    worlds: BTreeMap<GpuSceneWorldId, GpuSceneWorld>,
    views: BTreeMap<GpuSceneViewId, GpuSceneViewState>,
    uploads: GpuSceneUploadRing,
    revision: u64,
    /// Conservative world AABBs of instances created, updated, or removed since the
    /// last [`PersistentGpuScene::take_moved_bounds`] — the shadow system dirties the
    /// virtual pages these overlap. Capped; past the cap only the overflow flag grows.
    moved_bounds: Vec<GpuSceneMovedBounds>,
    moved_overflow: bool,
}

impl PersistentGpuScene {
    /// Creates an empty mirror with explicit bounded upload limits.
    pub fn new(upload_limits: GpuSceneUploadLimits) -> Result<Self, GpuSceneError> {
        Ok(Self {
            shared: GpuSceneSharedRecords::default(),
            worlds: BTreeMap::new(),
            views: BTreeMap::new(),
            uploads: GpuSceneUploadRing::new(upload_limits)?,
            revision: 0,
            moved_bounds: Vec::new(),
            moved_overflow: false,
        })
    }

    /// Reconstructs the complete derived mirror and queues a full table upload.
    pub fn from_snapshot(
        snapshot: PersistentGpuSceneSnapshot,
        upload_limits: GpuSceneUploadLimits,
    ) -> Result<Self, GpuSceneError> {
        let shared = GpuSceneSharedRecords {
            prototypes: SceneTable::from_snapshot(snapshot.shared.prototypes)?,
            materials: SceneTable::from_snapshot(snapshot.shared.materials)?,
            deformations: SceneTable::from_snapshot(snapshot.shared.deformations)?,
            sdfs: SceneTable::from_snapshot(snapshot.shared.sdfs)?,
            pages: SceneTable::from_snapshot(snapshot.shared.pages)?,
        };
        let mut worlds = BTreeMap::new();
        for world in snapshot.worlds {
            let id = world.id;
            if world.revision > snapshot.revision {
                return Err(GpuSceneError::InvalidSnapshot(
                    "world revision exceeds shared revision",
                ));
            }
            if worlds
                .insert(
                    id,
                    GpuSceneWorld {
                        instances: SceneTable::from_snapshot(world.instances)?,
                        lights: SceneTable::from_snapshot(world.lights)?,
                        revision: world.revision,
                    },
                )
                .is_some()
            {
                return Err(GpuSceneError::InvalidSnapshot("duplicate world identifier"));
            }
        }
        let mut scene = Self {
            shared,
            worlds,
            views: BTreeMap::new(),
            uploads: GpuSceneUploadRing::new(upload_limits)?,
            revision: snapshot.revision,
            moved_bounds: Vec::new(),
            moved_overflow: false,
        };
        scene.validate_all_references()?;
        scene.queue_full_snapshot()?;
        Ok(scene)
    }

    /// Captures all derived shared and per-world records without view-local history.
    #[must_use]
    pub fn snapshot(&self) -> PersistentGpuSceneSnapshot {
        PersistentGpuSceneSnapshot {
            shared: GpuSceneSharedSnapshot {
                prototypes: self.shared.prototypes.snapshot(),
                materials: self.shared.materials.snapshot(),
                deformations: self.shared.deformations.snapshot(),
                sdfs: self.shared.sdfs.snapshot(),
                pages: self.shared.pages.snapshot(),
            },
            worlds: self
                .worlds
                .iter()
                .map(|(&id, world)| GpuSceneWorldSnapshot {
                    id,
                    instances: world.instances.snapshot(),
                    lights: world.lights.snapshot(),
                    revision: world.revision,
                })
                .collect(),
            revision: self.revision,
        }
    }

    /// Current shared-scene revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current revision of one world.
    pub fn world_revision(&self, world: GpuSceneWorldId) -> Result<u64, GpuSceneError> {
        self.worlds
            .get(&world)
            .map(|world| world.revision)
            .ok_or(GpuSceneError::MissingWorld(world))
    }

    /// Creates an empty caller-keyed world store.
    pub fn create_world(&mut self, world: GpuSceneWorldId) -> Result<(), GpuSceneError> {
        if self.worlds.contains_key(&world) {
            return Err(GpuSceneError::DuplicateWorld(world));
        }
        self.worlds.insert(world, GpuSceneWorld::default());
        Ok(())
    }

    /// Removes an empty world that has no attached view.
    pub fn remove_world(&mut self, world: GpuSceneWorldId) -> Result<(), GpuSceneError> {
        let value = self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?;
        if !value.instances.is_empty()
            || !value.lights.is_empty()
            || self.views.values().any(|view| view.world == world)
        {
            return Err(GpuSceneError::WorldNotEmpty(world));
        }
        self.worlds.remove(&world);
        Ok(())
    }

    /// Creates independent temporal state for one view over an existing world.
    pub fn create_view(
        &mut self,
        view: GpuSceneViewId,
        world: GpuSceneWorldId,
    ) -> Result<(), GpuSceneError> {
        if !self.worlds.contains_key(&world) {
            return Err(GpuSceneError::MissingWorld(world));
        }
        if self.views.contains_key(&view) {
            return Err(GpuSceneError::DuplicateView(view));
        }
        self.views.insert(view, GpuSceneViewState::new(world));
        Ok(())
    }

    /// Removes view-local state without touching shared or per-world records.
    pub fn remove_view(&mut self, view: GpuSceneViewId) -> Result<(), GpuSceneError> {
        self.views
            .remove(&view)
            .map(|_| ())
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns immutable view-local state.
    pub fn view(&self, view: GpuSceneViewId) -> Result<&GpuSceneViewState, GpuSceneError> {
        self.views
            .get(&view)
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns mutable view-local state without exposing scene-table mutation.
    pub fn view_mut(
        &mut self,
        view: GpuSceneViewId,
    ) -> Result<&mut GpuSceneViewState, GpuSceneError> {
        self.views
            .get_mut(&view)
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns one live prototype after generation validation.
    #[must_use]
    pub fn prototype(&self, handle: GpuScenePrototypeHandle) -> Option<&GpuScenePrototypeRecord> {
        self.shared.prototypes.get(handle)
    }

    /// Returns one live material after generation validation.
    #[must_use]
    pub fn material(&self, handle: GpuSceneMaterialHandle) -> Option<&GpuSceneMaterialRecord> {
        self.shared.materials.get(handle)
    }

    /// Returns one live deformation provider after generation validation.
    #[must_use]
    pub fn deformation(
        &self,
        handle: GpuSceneDeformationHandle,
    ) -> Option<&GpuSceneDeformationRecord> {
        self.shared.deformations.get(handle)
    }

    /// Returns one live SDF reference after generation validation.
    #[must_use]
    pub fn sdf(&self, handle: GpuSceneSdfHandle) -> Option<&GpuSceneSdfRecord> {
        self.shared.sdfs.get(handle)
    }

    /// Returns one live page after generation validation.
    #[must_use]
    pub fn page(&self, handle: GpuScenePageHandle) -> Option<&GpuScenePageRecord> {
        self.shared.pages.get(handle)
    }

    /// Iterates prototypes in stable slot order.
    pub fn prototypes(
        &self,
    ) -> impl Iterator<Item = (GpuScenePrototypeHandle, &GpuScenePrototypeRecord)> {
        self.shared.prototypes.iter()
    }

    /// Iterates materials in stable slot order.
    pub fn materials(
        &self,
    ) -> impl Iterator<Item = (GpuSceneMaterialHandle, &GpuSceneMaterialRecord)> {
        self.shared.materials.iter()
    }

    /// Iterates pages in stable slot order.
    pub fn pages(&self) -> impl Iterator<Item = (GpuScenePageHandle, &GpuScenePageRecord)> {
        self.shared.pages.iter()
    }

    /// Returns one live instance from a selected world.
    #[must_use]
    pub fn instance(
        &self,
        world: GpuSceneWorldId,
        handle: GpuSceneInstanceHandle,
    ) -> Option<&GpuSceneInstanceRecord> {
        self.worlds
            .get(&world)
            .and_then(|world| world.instances.get(handle))
    }

    /// Returns one live light from a selected world.
    #[must_use]
    pub fn light(
        &self,
        world: GpuSceneWorldId,
        handle: GpuSceneLightHandle,
    ) -> Option<&GpuSceneLightRecord> {
        self.worlds
            .get(&world)
            .and_then(|world| world.lights.get(handle))
    }

    /// Iterates instances in one world in stable slot order.
    pub fn instances(
        &self,
        world: GpuSceneWorldId,
    ) -> Result<
        impl Iterator<Item = (GpuSceneInstanceHandle, &GpuSceneInstanceRecord)>,
        GpuSceneError,
    > {
        Ok(self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?
            .instances
            .iter())
    }

    /// Iterates lights in one world in stable slot order.
    pub fn lights(
        &self,
        world: GpuSceneWorldId,
    ) -> Result<impl Iterator<Item = (GpuSceneLightHandle, &GpuSceneLightRecord)>, GpuSceneError>
    {
        Ok(self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?
            .lights
            .iter())
    }

    /// Applies one validated shared-record delta and queues its coalesced upload.
    pub fn apply_shared_delta(
        &mut self,
        delta: GpuSceneSharedDelta,
    ) -> Result<GpuSceneSharedDeltaResult, GpuSceneError> {
        let revision = increment(self.revision)?;
        let result = match delta {
            GpuSceneSharedDelta::CreatePrototype(record) => {
                self.validate_prototype(&record)?;
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Prototype,
                    &GpuSceneUploadPayload::Prototype(record.clone()),
                )?;
                let handle = self.shared.prototypes.insert(record.clone(), "prototype")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Prototype,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Prototype(record),
                )?;
                GpuSceneSharedDeltaResult::PrototypeCreated(handle)
            }
            GpuSceneSharedDelta::UpdatePrototype { handle, record } => {
                self.validate_prototype(&record)?;
                for instance in self
                    .worlds
                    .values()
                    .flat_map(|world| world.instances.iter())
                    .filter_map(|(_, instance)| (instance.prototype == handle).then_some(instance))
                {
                    self.validate_instance_against_prototype(instance, &record)?;
                }
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Prototype,
                    &GpuSceneUploadPayload::Prototype(record.clone()),
                )?;
                self.shared
                    .prototypes
                    .update(handle, record.clone(), "prototype")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Prototype,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Prototype(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemovePrototype(handle) => {
                if self
                    .worlds
                    .values()
                    .flat_map(|world| world.instances.iter())
                    .any(|(_, instance)| instance.prototype == handle)
                {
                    return Err(referenced("prototype", handle.raw()));
                }
                self.shared.prototypes.remove(handle, "prototype")?;
                self.queue_tombstone(GpuSceneUploadTarget::Prototype, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateMaterial(record) => {
                validate_device_handle(record.table, "material")?;
                let handle = self.shared.materials.insert(record, "material")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Material,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Material(record),
                )?;
                GpuSceneSharedDeltaResult::MaterialCreated(handle)
            }
            GpuSceneSharedDelta::UpdateMaterial { handle, record } => {
                validate_device_handle(record.table, "material")?;
                self.shared.materials.update(handle, record, "material")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Material,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Material(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveMaterial(handle) => {
                if self.material_is_referenced(handle) {
                    return Err(referenced("material", handle.raw()));
                }
                self.shared.materials.remove(handle, "material")?;
                self.queue_tombstone(GpuSceneUploadTarget::Material, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateDeformation(record) => {
                validate_device_handle(record.provider, "deformation")?;
                let handle = self.shared.deformations.insert(record, "deformation")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Deformation,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Deformation(record),
                )?;
                GpuSceneSharedDeltaResult::DeformationCreated(handle)
            }
            GpuSceneSharedDelta::UpdateDeformation { handle, record } => {
                validate_device_handle(record.provider, "deformation")?;
                self.shared
                    .deformations
                    .update(handle, record, "deformation")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Deformation,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Deformation(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveDeformation(handle) => {
                if self.deformation_is_referenced(handle) {
                    return Err(referenced("deformation", handle.raw()));
                }
                self.shared.deformations.remove(handle, "deformation")?;
                self.queue_tombstone(GpuSceneUploadTarget::Deformation, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateSdf(record) => {
                validate_device_handle(record.resource, "SDF")?;
                let handle = self.shared.sdfs.insert(record, "SDF")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Sdf,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Sdf(record),
                )?;
                GpuSceneSharedDeltaResult::SdfCreated(handle)
            }
            GpuSceneSharedDelta::UpdateSdf { handle, record } => {
                validate_device_handle(record.resource, "SDF")?;
                self.shared.sdfs.update(handle, record, "SDF")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Sdf,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Sdf(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveSdf(handle) => {
                if self.sdf_is_referenced(handle) {
                    return Err(referenced("SDF", handle.raw()));
                }
                self.shared.sdfs.remove(handle, "SDF")?;
                self.queue_tombstone(GpuSceneUploadTarget::Sdf, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreatePage(record) => {
                self.validate_page(None, &record)?;
                let handle = self.shared.pages.insert(record, "page")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Page,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Page(record),
                )?;
                GpuSceneSharedDeltaResult::PageCreated(handle)
            }
            GpuSceneSharedDelta::UpdatePage { handle, record } => {
                self.validate_page(Some(handle), &record)?;
                self.shared.pages.update(handle, record, "page")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Page,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Page(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemovePage(handle) => {
                if self.page_is_referenced(handle) {
                    return Err(referenced("page", handle.raw()));
                }
                self.shared.pages.remove(handle, "page")?;
                self.queue_tombstone(GpuSceneUploadTarget::Page, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
        };
        self.revision = revision;
        Ok(result)
    }

    /// Applies one validated per-world delta and queues its coalesced upload.
    /// Conservative world AABB of one instance: the prototype's object-space bounds
    /// sphere pushed through the transform (both matrices of a dynamic transform, so
    /// the box is swept over the frame's motion).
    fn instance_world_bounds(
        &self,
        record: &GpuSceneInstanceRecord,
    ) -> Option<GpuSceneMovedBounds> {
        let prototype = self.shared.prototypes.get(record.prototype)?;
        let bounds = prototype.bounds;
        let reach = (bounds[0].powi(2) + bounds[1].powi(2) + bounds[2].powi(2)).sqrt() + bounds[3];
        match &record.transform {
            GpuSceneTransform::Static(transform) => {
                let ticks = f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
                let center: [f32; 3] = std::array::from_fn(|axis| {
                    (transform.cell[axis] as f64 * saffron_spatial::BASE_CELL_EDGE_METERS
                        + f64::from(transform.local_ticks[axis]) / ticks) as f32
                });
                let scale = transform
                    .scale
                    .iter()
                    .map(|bits| (f64::from(*bits) / 65_536.0).abs() as f32)
                    .fold(0.0_f32, f32::max);
                let half = reach * scale.max(1e-6);
                Some((
                    std::array::from_fn(|axis| center[axis] - half),
                    std::array::from_fn(|axis| center[axis] + half),
                ))
            }
            GpuSceneTransform::Dynamic(transform) => {
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                for matrix in [&transform.current, &transform.previous] {
                    let scale = matrix.x_axis.truncate().length().max(
                        matrix
                            .y_axis
                            .truncate()
                            .length()
                            .max(matrix.z_axis.truncate().length()),
                    );
                    let center = matrix.transform_point3(saffron_geometry::glam::Vec3::new(
                        bounds[0], bounds[1], bounds[2],
                    ));
                    let half = reach * scale.max(1e-6);
                    for axis in 0..3 {
                        min[axis] = min[axis].min(center[axis] - half);
                        max[axis] = max[axis].max(center[axis] + half);
                    }
                }
                (min[0].is_finite() && max[0].is_finite()).then_some((min, max))
            }
        }
    }

    /// Records one instance's world bounds into the frame's moved list (capped).
    fn note_moved(&mut self, bounds: Option<GpuSceneMovedBounds>) {
        const MOVED_BOUNDS_CAP: usize = 4_096;
        let Some(bounds) = bounds else {
            return;
        };
        if self.moved_bounds.len() >= MOVED_BOUNDS_CAP {
            self.moved_overflow = true;
        } else {
            self.moved_bounds.push(bounds);
        }
    }

    /// Notes live instances whose GPU-deformed content changed this frame (compute
    /// skinning writes new vertices without a scene delta), so their shadow pages
    /// re-render.
    pub fn note_instances_moved(
        &mut self,
        world_id: GpuSceneWorldId,
        handles: &[GpuSceneInstanceHandle],
    ) {
        for handle in handles {
            let bounds = self
                .worlds
                .get(&world_id)
                .and_then(|world| world.instances.get(*handle))
                .and_then(|record| self.instance_world_bounds(record));
            self.note_moved(bounds);
        }
    }

    /// Drains the accumulated moved-instance bounds and the overflow flag.
    pub fn take_moved_bounds(&mut self) -> (Vec<GpuSceneMovedBounds>, bool) {
        let overflow = std::mem::take(&mut self.moved_overflow);
        (std::mem::take(&mut self.moved_bounds), overflow)
    }

    pub fn apply_world_delta(
        &mut self,
        world_id: GpuSceneWorldId,
        delta: GpuSceneWorldDelta,
    ) -> Result<GpuSceneWorldDeltaResult, GpuSceneError> {
        match &delta {
            GpuSceneWorldDelta::CreateInstance(record)
            | GpuSceneWorldDelta::UpdateInstance { record, .. } => {
                self.validate_instance(record)?;
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Instance(world_id),
                    &GpuSceneUploadPayload::Instance(record.clone()),
                )?;
            }
            GpuSceneWorldDelta::CreateLight(record)
            | GpuSceneWorldDelta::UpdateLight { record, .. } => validate_light(record)?,
            GpuSceneWorldDelta::RemoveInstance(_) | GpuSceneWorldDelta::RemoveLight(_) => {}
        }
        let revision = increment(self.revision)?;
        // The shadow pages an instance mutation overlaps re-render: note the OLD
        // record's bounds (update/remove reveal or vacate coverage) and the NEW
        // record's (create/update cast fresh coverage).
        let mut moved: Vec<Option<GpuSceneMovedBounds>> = Vec::new();
        match &delta {
            GpuSceneWorldDelta::CreateInstance(record) => {
                moved.push(self.instance_world_bounds(record));
            }
            GpuSceneWorldDelta::UpdateInstance { handle, record } => {
                if let Some(old) = self
                    .worlds
                    .get(&world_id)
                    .and_then(|world| world.instances.get(*handle))
                {
                    moved.push(self.instance_world_bounds(old));
                }
                moved.push(self.instance_world_bounds(record));
            }
            GpuSceneWorldDelta::RemoveInstance(handle) => {
                if let Some(old) = self
                    .worlds
                    .get(&world_id)
                    .and_then(|world| world.instances.get(*handle))
                {
                    moved.push(self.instance_world_bounds(old));
                }
            }
            GpuSceneWorldDelta::CreateLight(_)
            | GpuSceneWorldDelta::UpdateLight { .. }
            | GpuSceneWorldDelta::RemoveLight(_) => {}
        }
        for bounds in moved {
            self.note_moved(bounds);
        }
        let world = self
            .worlds
            .get_mut(&world_id)
            .ok_or(GpuSceneError::MissingWorld(world_id))?;
        let result = match delta {
            GpuSceneWorldDelta::CreateInstance(record) => {
                let handle = world.instances.insert(record.clone(), "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
                GpuSceneWorldDeltaResult::InstanceCreated(handle)
            }
            GpuSceneWorldDelta::UpdateInstance { handle, record } => {
                world.instances.update(handle, record.clone(), "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
                GpuSceneWorldDeltaResult::Updated
            }
            GpuSceneWorldDelta::RemoveInstance(handle) => {
                world.instances.remove(handle, "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Tombstone,
                )?;
                GpuSceneWorldDeltaResult::Removed
            }
            GpuSceneWorldDelta::CreateLight(record) => {
                let handle = world.lights.insert(record, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
                GpuSceneWorldDeltaResult::LightCreated(handle)
            }
            GpuSceneWorldDelta::UpdateLight { handle, record } => {
                world.lights.update(handle, record, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
                GpuSceneWorldDeltaResult::Updated
            }
            GpuSceneWorldDelta::RemoveLight(handle) => {
                world.lights.remove(handle, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Tombstone,
                )?;
                GpuSceneWorldDeltaResult::Removed
            }
        };
        world.revision = revision;
        self.revision = revision;
        Ok(result)
    }

    /// Releases one completed frame slot for handle reuse and bounded upload staging.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<(), GpuSceneError> {
        frame_bit(completed_frame_slot)?;
        self.shared.prototypes.begin_frame(completed_frame_slot)?;
        self.shared.materials.begin_frame(completed_frame_slot)?;
        self.shared.deformations.begin_frame(completed_frame_slot)?;
        self.shared.sdfs.begin_frame(completed_frame_slot)?;
        self.shared.pages.begin_frame(completed_frame_slot)?;
        for world in self.worlds.values_mut() {
            world.instances.begin_frame(completed_frame_slot)?;
            world.lights.begin_frame(completed_frame_slot)?;
        }
        self.uploads.begin_frame(completed_frame_slot)
    }

    /// Stages the next coalesced batch within batch and frame-slot limits.
    pub fn stage_upload_batch(
        &mut self,
        frame_slot: usize,
    ) -> Result<GpuSceneUploadBatch, GpuSceneError> {
        self.uploads.stage(frame_slot)
    }

    fn validate_prototype(&self, record: &GpuScenePrototypeRecord) -> Result<(), GpuSceneError> {
        validate_device_handle(record.geometry, "geometry")?;
        for &material in record.materials.iter() {
            require(&self.shared.materials, material, "material")?;
        }
        if let Some(deformation) = record.deformation {
            require(&self.shared.deformations, deformation, "deformation")?;
        }
        if let Some(sdf) = record.sdf {
            require(&self.shared.sdfs, sdf, "SDF")?;
        }
        require(&self.shared.pages, record.root_page, "page")?;
        if record.bounds.into_iter().any(|value| !value.is_finite()) {
            return Err(GpuSceneError::NonFinite("prototype bounds"));
        }
        if record.bounds[3] < 0.0 {
            return Err(GpuSceneError::NegativeBoundsRadius);
        }
        Ok(())
    }

    fn validate_instance(&self, record: &GpuSceneInstanceRecord) -> Result<(), GpuSceneError> {
        let prototype = require(&self.shared.prototypes, record.prototype, "prototype")?;
        if let Some(deformation) = record.deformation {
            require(&self.shared.deformations, deformation, "deformation")?;
        }
        if let Some(sdf) = record.sdf {
            require(&self.shared.sdfs, sdf, "SDF")?;
        }
        self.validate_instance_against_prototype(record, prototype)
    }

    fn validate_instance_against_prototype(
        &self,
        record: &GpuSceneInstanceRecord,
        prototype: &GpuScenePrototypeRecord,
    ) -> Result<(), GpuSceneError> {
        let mut prior = None;
        for material_override in record.material_overrides.iter() {
            if prior.is_some_and(|slot| slot >= material_override.slot) {
                return Err(GpuSceneError::MaterialOverrideOrder);
            }
            let slot = material_override.slot as usize;
            if slot >= prototype.materials.len() {
                return Err(GpuSceneError::MaterialOverrideSlot {
                    slot: material_override.slot,
                    count: prototype.materials.len(),
                });
            }
            require(
                &self.shared.materials,
                material_override.material,
                "material",
            )?;
            prior = Some(material_override.slot);
        }
        if let GpuSceneTransform::Dynamic(transform) = record.transform
            && (!matrix_is_finite(transform.current) || !matrix_is_finite(transform.previous))
        {
            return Err(GpuSceneError::NonFinite("dynamic transform"));
        }
        Ok(())
    }

    fn validate_page(
        &self,
        updating: Option<GpuScenePageHandle>,
        record: &GpuScenePageRecord,
    ) -> Result<(), GpuSceneError> {
        validate_device_handle(record.table, "page")?;
        let Some(mut parent) = record.parent else {
            return Ok(());
        };
        let mut seen = BTreeSet::new();
        while let Some(page) = self.shared.pages.get(parent) {
            if Some(parent) == updating || !seen.insert(parent) {
                return Err(GpuSceneError::PageCycle);
            }
            match page.parent {
                Some(next) => parent = next,
                None => return Ok(()),
            }
        }
        Err(GpuSceneError::StaleHandle {
            kind: "page",
            handle: parent.raw(),
        })
    }

    fn material_is_referenced(&self, handle: GpuSceneMaterialHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.materials.contains(&handle))
            || self
                .worlds
                .values()
                .flat_map(|world| world.instances.iter())
                .any(|(_, instance)| {
                    instance
                        .material_overrides
                        .iter()
                        .any(|material_override| material_override.material == handle)
                })
    }

    fn deformation_is_referenced(&self, handle: GpuSceneDeformationHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.deformation == Some(handle))
            || self
                .worlds
                .values()
                .flat_map(|world| world.instances.iter())
                .any(|(_, instance)| instance.deformation == Some(handle))
    }

    fn sdf_is_referenced(&self, handle: GpuSceneSdfHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.sdf == Some(handle))
            || self
                .worlds
                .values()
                .flat_map(|world| world.instances.iter())
                .any(|(_, instance)| instance.sdf == Some(handle))
    }

    fn page_is_referenced(&self, handle: GpuScenePageHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.root_page == handle)
            || self
                .shared
                .pages
                .iter()
                .any(|(_, page)| page.parent == Some(handle))
    }

    fn queue_tombstone(
        &mut self,
        target: GpuSceneUploadTarget,
        handle: GpuHandle,
        revision: u64,
    ) -> Result<(), GpuSceneError> {
        self.uploads
            .enqueue(target, handle, revision, GpuSceneUploadPayload::Tombstone)
    }

    fn ensure_payload_fits(
        &self,
        target: GpuSceneUploadTarget,
        payload: &GpuSceneUploadPayload,
    ) -> Result<(), GpuSceneError> {
        let bytes = payload.byte_len();
        if bytes > self.uploads.limits.max_batch_bytes {
            return Err(GpuSceneError::RecordExceedsBatch {
                target,
                bytes,
                limit: self.uploads.limits.max_batch_bytes,
            });
        }
        Ok(())
    }

    fn validate_all_references(&self) -> Result<(), GpuSceneError> {
        for (_, material) in self.shared.materials.iter() {
            validate_device_handle(material.table, "material")?;
        }
        for (_, deformation) in self.shared.deformations.iter() {
            validate_device_handle(deformation.provider, "deformation")?;
        }
        for (_, sdf) in self.shared.sdfs.iter() {
            validate_device_handle(sdf.resource, "SDF")?;
        }
        for (_, page) in self.shared.pages.iter() {
            self.validate_page(None, page)?;
        }
        for (_, prototype) in self.shared.prototypes.iter() {
            self.validate_prototype(prototype)?;
        }
        for world in self.worlds.values() {
            for (_, instance) in world.instances.iter() {
                self.validate_instance(instance)?;
            }
            for (_, light) in world.lights.iter() {
                validate_light(light)?;
            }
        }
        Ok(())
    }

    fn queue_full_snapshot(&mut self) -> Result<(), GpuSceneError> {
        let revision = self.revision;
        let prototypes: Vec<_> = self
            .shared
            .prototypes
            .iter()
            .map(|(handle, record)| (handle.raw(), record.clone()))
            .collect();
        let materials: Vec<_> = self
            .shared
            .materials
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let deformations: Vec<_> = self
            .shared
            .deformations
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let sdfs: Vec<_> = self
            .shared
            .sdfs
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let pages: Vec<_> = self
            .shared
            .pages
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        for (handle, record) in prototypes {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Prototype,
                handle,
                revision,
                GpuSceneUploadPayload::Prototype(record),
            )?;
        }
        for (handle, record) in materials {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Material,
                handle,
                revision,
                GpuSceneUploadPayload::Material(record),
            )?;
        }
        for (handle, record) in deformations {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Deformation,
                handle,
                revision,
                GpuSceneUploadPayload::Deformation(record),
            )?;
        }
        for (handle, record) in sdfs {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Sdf,
                handle,
                revision,
                GpuSceneUploadPayload::Sdf(record),
            )?;
        }
        for (handle, record) in pages {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Page,
                handle,
                revision,
                GpuSceneUploadPayload::Page(record),
            )?;
        }
        let worlds: Vec<_> = self
            .worlds
            .iter()
            .map(|(&id, world)| {
                let instances = world
                    .instances
                    .iter()
                    .map(|(handle, record)| (handle.raw(), record.clone()))
                    .collect::<Vec<_>>();
                let lights = world
                    .lights
                    .iter()
                    .map(|(handle, record)| (handle.raw(), *record))
                    .collect::<Vec<_>>();
                (id, instances, lights)
            })
            .collect();
        for (world, instances, lights) in worlds {
            for (handle, record) in instances {
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world),
                    handle,
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
            }
            for (handle, record) in lights {
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world),
                    handle,
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
            }
        }
        Ok(())
    }
}

fn validate_device_handle(handle: GpuHandle, kind: &'static str) -> Result<(), GpuSceneError> {
    if handle == GpuHandle::INVALID || handle.generation == 0 {
        return Err(GpuSceneError::StaleHandle { kind, handle });
    }
    Ok(())
}

fn require<'a, T, K>(
    table: &'a SceneTable<T, K>,
    handle: GpuSceneHandle<K>,
    kind: &'static str,
) -> Result<&'a T, GpuSceneError> {
    table.get(handle).ok_or(GpuSceneError::StaleHandle {
        kind,
        handle: handle.raw(),
    })
}

fn referenced(kind: &'static str, handle: GpuHandle) -> GpuSceneError {
    GpuSceneError::ReferencedHandle { kind, handle }
}

fn validate_light(record: &GpuSceneLightRecord) -> Result<(), GpuSceneError> {
    let finite = record
        .light
        .position_range
        .to_array()
        .into_iter()
        .chain(record.light.color_intensity.to_array())
        .chain(record.light.direction_type.to_array())
        .chain(record.light.spot_cos.to_array())
        .all(f32::is_finite);
    if !finite {
        return Err(GpuSceneError::NonFinite("light"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use saffron_geometry::glam::{Mat4, Vec3, Vec4};
    use saffron_spatial::{DecisionScalar, QuantizedLocalPosition, WorldCellKey};

    use super::*;

    fn device_handle(index: u32) -> GpuHandle {
        GpuHandle {
            index,
            generation: 1,
        }
    }

    fn material(index: u32) -> GpuSceneMaterialRecord {
        GpuSceneMaterialRecord {
            table: device_handle(index),
            source_revision: 1,
        }
    }

    fn page(index: u32, parent: Option<GpuScenePageHandle>) -> GpuScenePageRecord {
        GpuScenePageRecord {
            table: device_handle(index),
            parent,
            source_generation: 1,
            flags: 0,
        }
    }

    fn prototype(
        material: GpuSceneMaterialHandle,
        root_page: GpuScenePageHandle,
    ) -> GpuScenePrototypeRecord {
        GpuScenePrototypeRecord {
            geometry: device_handle(20),
            materials: Arc::from([material]),
            deformation: None,
            sdf: None,
            root_page,
            bounds: [0.0, 0.0, 0.0, 1.0],
            source_generation: 1,
            flags: 0,
        }
    }

    fn static_transform() -> GpuSceneTransform {
        let position = WorldPosition::new(
            WorldCellKey::base(-4, 2, 9),
            QuantizedLocalPosition::new([10, 20, 30]).unwrap(),
        )
        .unwrap();
        let scale = [
            DecisionScalar::from_integer(1).unwrap(),
            DecisionScalar::from_integer(2).unwrap(),
            DecisionScalar::from_integer(1).unwrap(),
        ];
        GpuSceneTransform::Static(GpuSceneStaticTransform::new(
            position,
            QuantizedOrientation::identity(),
            scale,
            0,
        ))
    }

    fn instance(prototype: GpuScenePrototypeHandle) -> GpuSceneInstanceRecord {
        GpuSceneInstanceRecord {
            prototype,
            transform: static_transform(),
            material_overrides: Arc::from([]),
            deformation: None,
            sdf: None,
            source_generation: 1,
            flags: 0,
            combination: 0,
            vegetation: None,
        }
    }

    fn insert_shared(
        scene: &mut PersistentGpuScene,
    ) -> (
        GpuSceneMaterialHandle,
        GpuScenePageHandle,
        GpuScenePrototypeHandle,
    ) {
        let GpuSceneSharedDeltaResult::MaterialCreated(material) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(self::material(1)))
            .unwrap()
        else {
            panic!("material create result")
        };
        let GpuSceneSharedDeltaResult::PageCreated(page) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(self::page(2, None)))
            .unwrap()
        else {
            panic!("page create result")
        };
        let GpuSceneSharedDeltaResult::PrototypeCreated(prototype_handle) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(prototype(
                material, page,
            )))
            .unwrap()
        else {
            panic!("prototype create result")
        };
        (material, page, prototype_handle)
    }

    #[test]
    fn static_transform_is_compact_and_exact() {
        let GpuSceneTransform::Static(transform) = static_transform() else {
            unreachable!()
        };
        assert_eq!(std::mem::size_of::<GpuSceneStaticTransform>(), 64);
        assert_eq!(transform.cell, [-4, 2, 9]);
        assert_eq!(transform.local_ticks, [10, 20, 30]);
        assert_eq!(transform.orientation, [0, 0, 0, i16::MAX]);
        assert_eq!(transform.scale, [65_536, 131_072, 65_536]);
    }

    #[test]
    fn dynamic_transform_advances_current_to_previous() {
        let mut transform = GpuSceneDynamicTransform::stationary(Mat4::IDENTITY).unwrap();
        let next = Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0));
        transform.advance(next).unwrap();
        assert_eq!(transform.previous, Mat4::IDENTITY);
        assert_eq!(transform.current, next);
        assert!(
            GpuSceneDynamicTransform::stationary(Mat4::from_cols_array(&[
                f32::NAN,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
            ]))
            .is_err()
        );
    }

    #[test]
    fn handles_reuse_only_after_every_frame_slot_completes() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let GpuSceneSharedDeltaResult::MaterialCreated(first) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(1)))
            .unwrap()
        else {
            unreachable!()
        };
        scene
            .apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(first))
            .unwrap();
        let GpuSceneSharedDeltaResult::MaterialCreated(second) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(2)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_ne!(first.raw().index, second.raw().index);
        scene.begin_frame(0).unwrap();
        let GpuSceneSharedDeltaResult::MaterialCreated(third) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(3)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_ne!(first.raw().index, third.raw().index);
        scene.begin_frame(1).unwrap();
        let GpuSceneSharedDeltaResult::MaterialCreated(reused) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(4)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(first.raw().index, reused.raw().index);
        assert_eq!(first.raw().generation + 1, reused.raw().generation);
        assert!(scene.material(first).is_none());
    }

    #[test]
    fn updates_coalesce_and_batches_merge_consecutive_slots() {
        let limits = GpuSceneUploadLimits {
            max_batch_records: 2,
            max_batch_bytes: 1_024,
            max_frame_records: 4,
            max_frame_bytes: 2_048,
        };
        let mut scene = PersistentGpuScene::new(limits).unwrap();
        let GpuSceneSharedDeltaResult::MaterialCreated(first) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(1)))
            .unwrap()
        else {
            unreachable!()
        };
        let GpuSceneSharedDeltaResult::MaterialCreated(second) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(2)))
            .unwrap()
        else {
            unreachable!()
        };
        scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdateMaterial {
                handle: first,
                record: material(9),
            })
            .unwrap();
        scene.begin_frame(0).unwrap();
        let batch = scene.stage_upload_batch(0).unwrap();
        assert_eq!(batch.record_count, 2);
        assert_eq!(batch.ranges.len(), 1);
        assert_eq!(batch.ranges[0].first_slot, first.raw().index);
        assert_eq!(batch.ranges[0].records.len(), 2);
        assert_eq!(batch.ranges[0].records[0].revision, 3);
        assert_eq!(second.raw().index, first.raw().index + 1);
        assert!(!batch.more_pending);
    }

    #[test]
    fn sparse_overrides_are_references_and_strictly_ordered() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let (base, page, _) = insert_shared(&mut scene);
        let GpuSceneSharedDeltaResult::MaterialCreated(replacement) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(material(7)))
            .unwrap()
        else {
            unreachable!()
        };
        let GpuSceneSharedDeltaResult::PrototypeCreated(two_slots) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    materials: Arc::from([base, base]),
                    ..prototype(base, page)
                },
            ))
            .unwrap()
        else {
            unreachable!()
        };
        scene.create_world(GpuSceneWorldId(4)).unwrap();
        let mut record = instance(two_slots);
        record.material_overrides = Arc::from([
            GpuSceneMaterialOverride {
                slot: 1,
                material: replacement,
            },
            GpuSceneMaterialOverride {
                slot: 0,
                material: replacement,
            },
        ]);
        assert_eq!(
            scene
                .apply_world_delta(
                    GpuSceneWorldId(4),
                    GpuSceneWorldDelta::CreateInstance(record)
                )
                .unwrap_err(),
            GpuSceneError::MaterialOverrideOrder
        );
    }

    #[test]
    fn prototype_update_preserves_live_override_slots() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let (base, page, _) = insert_shared(&mut scene);
        let GpuSceneSharedDeltaResult::PrototypeCreated(two_slots) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    materials: Arc::from([base, base]),
                    ..prototype(base, page)
                },
            ))
            .unwrap()
        else {
            unreachable!()
        };
        let world = GpuSceneWorldId(4);
        scene.create_world(world).unwrap();
        let mut record = instance(two_slots);
        record.material_overrides = Arc::from([GpuSceneMaterialOverride {
            slot: 1,
            material: base,
        }]);
        scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
            .unwrap();
        assert_eq!(
            scene
                .apply_shared_delta(GpuSceneSharedDelta::UpdatePrototype {
                    handle: two_slots,
                    record: prototype(base, page),
                })
                .unwrap_err(),
            GpuSceneError::MaterialOverrideSlot { slot: 1, count: 1 }
        );
    }

    #[test]
    fn referenced_shared_records_cannot_be_removed() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let (material, _, prototype) = insert_shared(&mut scene);
        scene.create_world(GpuSceneWorldId(1)).unwrap();
        scene
            .apply_world_delta(
                GpuSceneWorldId(1),
                GpuSceneWorldDelta::CreateInstance(instance(prototype)),
            )
            .unwrap();
        assert!(matches!(
            scene.apply_shared_delta(GpuSceneSharedDelta::RemovePrototype(prototype)),
            Err(GpuSceneError::ReferencedHandle {
                kind: "prototype",
                ..
            })
        ));
        assert!(matches!(
            scene.apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(material)),
            Err(GpuSceneError::ReferencedHandle {
                kind: "material",
                ..
            })
        ));
    }

    #[test]
    fn snapshot_rebuild_preserves_live_handles_and_queues_full_upload() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let (_, _, prototype) = insert_shared(&mut scene);
        let world = GpuSceneWorldId(8);
        scene.create_world(world).unwrap();
        let GpuSceneWorldDeltaResult::InstanceCreated(instance_handle) = scene
            .apply_world_delta(
                world,
                GpuSceneWorldDelta::CreateInstance(instance(prototype)),
            )
            .unwrap()
        else {
            unreachable!()
        };
        let snapshot = scene.snapshot();
        let mut rebuilt =
            PersistentGpuScene::from_snapshot(snapshot, GpuSceneUploadLimits::default()).unwrap();
        assert!(rebuilt.prototype(prototype).is_some());
        assert!(rebuilt.instance(world, instance_handle).is_some());
        rebuilt.begin_frame(0).unwrap();
        let batch = rebuilt.stage_upload_batch(0).unwrap();
        assert_eq!(batch.record_count, 4);
        assert!(!batch.more_pending);
        assert!(rebuilt.views.is_empty());
    }

    #[test]
    fn view_state_is_independent_from_shared_and_world_records() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        scene.create_world(GpuSceneWorldId(1)).unwrap();
        scene.create_world(GpuSceneWorldId(2)).unwrap();
        scene
            .create_view(GpuSceneViewId(10), GpuSceneWorldId(1))
            .unwrap();
        scene
            .create_view(GpuSceneViewId(11), GpuSceneWorldId(1))
            .unwrap();
        scene
            .view_mut(GpuSceneViewId(10))
            .unwrap()
            .publish(3, 4)
            .unwrap();
        assert!(scene.view(GpuSceneViewId(10)).unwrap().history_valid);
        assert!(!scene.view(GpuSceneViewId(11)).unwrap().history_valid);
        scene
            .view_mut(GpuSceneViewId(10))
            .unwrap()
            .invalidate(GpuSceneHistoryInvalidation::CameraCut)
            .unwrap();
        assert_eq!(
            scene.view(GpuSceneViewId(10)).unwrap().invalidation,
            GpuSceneHistoryInvalidation::CameraCut
        );
        assert_eq!(scene.world_revision(GpuSceneWorldId(2)).unwrap(), 0);
    }

    #[test]
    fn page_updates_reject_cycles() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let GpuSceneSharedDeltaResult::PageCreated(root) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(page(1, None)))
            .unwrap()
        else {
            unreachable!()
        };
        let GpuSceneSharedDeltaResult::PageCreated(child) = scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(page(2, Some(root))))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            scene
                .apply_shared_delta(GpuSceneSharedDelta::UpdatePage {
                    handle: root,
                    record: page(1, Some(child)),
                })
                .unwrap_err(),
            GpuSceneError::PageCycle
        );
    }

    #[test]
    fn light_validation_rejects_non_finite_payloads() {
        let mut scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).unwrap();
        let world = GpuSceneWorldId(1);
        scene.create_world(world).unwrap();
        let light = GpuSceneLightRecord {
            light: GpuLight {
                position_range: Vec4::new(0.0, 0.0, 0.0, f32::INFINITY),
                color_intensity: Vec4::ONE,
                direction_type: Vec4::ZERO,
                spot_cos: Vec4::ZERO,
            },
            source_revision: 1,
        };
        assert_eq!(
            scene
                .apply_world_delta(world, GpuSceneWorldDelta::CreateLight(light))
                .unwrap_err(),
            GpuSceneError::NonFinite("light")
        );
    }
}
