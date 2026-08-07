//! Monotonic scene mutation records for disposable derived consumers.

use std::any::TypeId;

use glam::Mat4;
use saffron_core::Uuid;

use crate::Entity;

/// Monotonic revision assigned to every tracked scene mutation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneRevision(u64);

impl SceneRevision {
    /// Initial revision of an empty scene.
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
                .expect("scene mutation revision exhausted u64"),
        )
    }
}

/// Consumer position in the bounded scene mutation journal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneJournalCursor {
    revision: SceneRevision,
}

impl SceneJournalCursor {
    /// Cursor at the initial empty-scene revision.
    pub const START: Self = Self {
        revision: SceneRevision::ZERO,
    };

    /// Revision consumed by this cursor.
    #[must_use]
    pub const fn revision(self) -> SceneRevision {
        self.revision
    }

    pub(crate) const fn at(revision: SceneRevision) -> Self {
        Self { revision }
    }
}

/// One canonical scene change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneMutationKind {
    /// A complete entity snapshot became available.
    EntityCreated,
    /// An entity ceased to exist.
    EntityDestroyed,
    /// A component type became present.
    ComponentAdded(TypeId),
    /// A present component may have changed.
    ComponentUpdated(TypeId),
    /// A component type ceased to be present.
    ComponentRemoved(TypeId),
}

/// Revisioned mutation addressed by both transient handle and stable entity identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneMutation {
    /// Globally monotonic revision of this change.
    pub revision: SceneRevision,
    /// Handle valid for live mutations and useful for direct scene lookup.
    pub entity: Entity,
    /// Stable identity retained after destruction.
    pub entity_id: Uuid,
    /// Structural or component change.
    pub kind: SceneMutationKind,
}

/// Per-entity revisions used by incremental derived consumers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneEntityRevisions {
    /// Revision at which the entity became live.
    pub created: SceneRevision,
    /// Latest structural or component mutation.
    pub content: SceneRevision,
    /// Latest local-transform mutation.
    pub local_transform: SceneRevision,
    /// Latest parent/child relationship mutation.
    pub hierarchy: SceneRevision,
    /// Latest world-transform value change.
    pub world_transform: SceneRevision,
    /// Revision of the prior world-transform value.
    pub previous_world_transform: SceneRevision,
}

/// Current and prior world transforms with their exact publication revisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneWorldTransformState {
    /// Current composed world transform.
    pub current: Mat4,
    /// Value immediately preceding `current`.
    pub previous: Mat4,
    /// Revision at which `current` was published.
    pub current_revision: SceneRevision,
    /// Revision associated with `previous`.
    pub previous_revision: SceneRevision,
}

/// Result of reading the bounded journal from a consumer cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneJournalRead {
    /// Every mutation after the cursor is available in revision order.
    Delta {
        /// Ordered mutations newer than the input cursor.
        mutations: Vec<SceneMutation>,
        /// Cursor to retain after applying the delta.
        next: SceneJournalCursor,
    },
    /// The cursor fell outside retained history; rebuild from a scene snapshot.
    SnapshotRequired {
        /// Cursor to retain immediately after taking the replacement snapshot.
        next: SceneJournalCursor,
    },
}
