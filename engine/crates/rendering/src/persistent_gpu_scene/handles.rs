use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use crate::GpuHandle;

/// Typed stable slot and generation used by the persistent GPU Scene.
#[repr(transparent)]
pub struct GpuSceneHandle<K> {
    pub(super) raw: GpuHandle,
    marker: PhantomData<fn() -> K>,
}

impl<K> GpuSceneHandle<K> {
    pub(super) fn from_raw(raw: GpuHandle) -> Self {
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
