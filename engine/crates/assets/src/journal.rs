//! Monotonic asset mutation records for disposable derived consumers.

use saffron_core::Uuid;
use saffron_scene::{AssetCatalog, AssetType};

/// Monotonic revision assigned to every tracked asset mutation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetRevision(u64);

impl AssetRevision {
    /// Initial revision of an empty asset server.
    pub const ZERO: Self = Self(0);

    /// Underlying monotonic sequence value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("asset mutation revision exhausted u64"),
        )
    }
}

/// Consumer position in the bounded asset mutation journal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetJournalCursor {
    revision: AssetRevision,
}

impl AssetJournalCursor {
    /// Cursor at the initial empty-server revision.
    pub const START: Self = Self {
        revision: AssetRevision::ZERO,
    };

    /// Revision consumed by this cursor.
    #[must_use]
    pub const fn revision(self) -> AssetRevision {
        self.revision
    }

    pub(crate) const fn at(revision: AssetRevision) -> Self {
        Self { revision }
    }
}

/// Render-derived data invalidated by an asset mutation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetInvalidations(u8);

impl AssetInvalidations {
    /// No render-derived data changes.
    pub const NONE: Self = Self(0);
    /// Geometry and model prototype records.
    pub const PROTOTYPE: Self = Self(1 << 0);
    /// Resolved material records and material dependency state.
    pub const MATERIAL: Self = Self(1 << 1);
    /// Texture descriptors and image content.
    pub const TEXTURE: Self = Self(1 << 2);
    /// Streamed geometry or vegetation pages.
    pub const PAGE: Self = Self(1 << 3);
    /// Every render-derived asset class.
    pub const ALL: Self =
        Self(Self::PROTOTYPE.0 | Self::MATERIAL.0 | Self::TEXTURE.0 | Self::PAGE.0);

    /// Whether this set contains every bit in `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Scope addressed by an asset mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetMutationTarget {
    /// One stable catalog asset.
    Asset {
        /// Stable asset identity.
        id: Uuid,
        /// Asset kind at the mutation boundary.
        asset_type: AssetType,
    },
    /// The complete live catalog and all loaded representations.
    All,
}

/// One canonical asset change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetMutationKind {
    /// A new asset became available.
    Imported,
    /// Stable asset identity received new imported content.
    Reimported,
    /// Authored content or catalog metadata changed.
    Edited,
    /// Loaded derived representations were discarded.
    Unloaded,
    /// An asset ceased to exist.
    Deleted,
    /// The complete catalog was atomically replaced.
    CatalogReplaced,
}

/// Revisioned asset mutation and its precise render invalidation scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetMutation {
    /// Globally monotonic revision of this change.
    pub revision: AssetRevision,
    /// Stable asset or complete-catalog target.
    pub target: AssetMutationTarget,
    /// Mutation operation.
    pub kind: AssetMutationKind,
    /// Render-derived data that must be refreshed.
    pub invalidations: AssetInvalidations,
}

/// Atomically captured catalog and the cursor representing it.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetCatalogSnapshot {
    /// Complete catalog state at `cursor`.
    pub catalog: AssetCatalog,
    /// Cursor to retain after rebuilding a derived mirror from `catalog`.
    pub cursor: AssetJournalCursor,
}

/// Result of reading the bounded journal from a consumer cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetJournalRead {
    /// Every mutation after the cursor is available in revision order.
    Delta {
        /// Ordered mutations newer than the input cursor.
        mutations: Vec<AssetMutation>,
        /// Cursor to retain after applying the delta.
        next: AssetJournalCursor,
    },
    /// The cursor fell outside retained history; rebuild from a catalog snapshot.
    SnapshotRequired {
        /// Cursor to retain immediately after taking the replacement snapshot.
        next: AssetJournalCursor,
    },
}

pub(crate) const fn invalidations_for(asset_type: AssetType) -> AssetInvalidations {
    match asset_type {
        AssetType::Mesh => AssetInvalidations::PROTOTYPE.union(AssetInvalidations::PAGE),
        AssetType::Texture => AssetInvalidations::TEXTURE.union(AssetInvalidations::MATERIAL),
        AssetType::Animation => AssetInvalidations::PROTOTYPE,
        AssetType::Material => AssetInvalidations::MATERIAL,
        AssetType::Model => AssetInvalidations::PROTOTYPE.union(AssetInvalidations::PAGE),
        AssetType::Plant => AssetInvalidations::PROTOTYPE
            .union(AssetInvalidations::MATERIAL)
            .union(AssetInvalidations::PAGE),
        AssetType::Biome | AssetType::VegetationMap => {
            AssetInvalidations::PROTOTYPE.union(AssetInvalidations::PAGE)
        }
        AssetType::Other | AssetType::Lut | AssetType::Environment => AssetInvalidations::NONE,
    }
}
