//! Persistent renderer-derived scene records and bounded delta uploads.

mod apply;
mod delta;
mod handles;
mod records;
mod scene;
mod table;
mod upload_ring;
mod validate;

#[cfg(test)]
mod tests;

use crate::GpuHandle;

pub use delta::*;
pub use handles::*;
pub use records::*;
pub use scene::*;
pub use table::*;
pub use upload_ring::*;

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
