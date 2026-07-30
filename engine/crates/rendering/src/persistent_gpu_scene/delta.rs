use super::table::SceneTable;
use super::*;

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
pub(super) struct GpuSceneSharedRecords {
    pub(super) prototypes: SceneTable<GpuScenePrototypeRecord, GpuScenePrototypeKind>,
    pub(super) materials: SceneTable<GpuSceneMaterialRecord, GpuSceneMaterialKind>,
    pub(super) deformations: SceneTable<GpuSceneDeformationRecord, GpuSceneDeformationKind>,
    pub(super) sdfs: SceneTable<GpuSceneSdfRecord, GpuSceneSdfKind>,
    pub(super) pages: SceneTable<GpuScenePageRecord, GpuScenePageKind>,
}

#[derive(Default)]
pub(super) struct GpuSceneWorld {
    pub(super) instances: SceneTable<GpuSceneInstanceRecord, GpuSceneInstanceKind>,
    pub(super) lights: SceneTable<GpuSceneLightRecord, GpuSceneLightKind>,
    pub(super) revision: u64,
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
    /// The wind field jumped: speed, gust, or a source edit the reprojection cannot follow.
    ///
    /// Distinct from `CameraCut` because nothing about the view moved — the GEOMETRY did, and only
    /// the deformed geometry. Keeping the two apart is what lets a reader tell a camera teleport
    /// from an artist dragging a wind slider when both blank the same history.
    WindDiscontinuity,
}

impl GpuSceneHistoryInvalidation {
    /// A stable lowercase name for logs and the control plane.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NewView => "new-view",
            Self::CameraCut => "camera-cut",
            Self::Resize => "resize",
            Self::OriginShift => "origin-shift",
            Self::RepresentationChange => "representation-change",
            Self::SceneRebuild => "scene-rebuild",
            Self::WindDiscontinuity => "wind-discontinuity",
        }
    }
}
