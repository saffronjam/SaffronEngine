use std::sync::Arc;

use saffron_geometry::glam::Mat4;
use saffron_spatial::{DecisionScalar, QuantizedOrientation, WorldPosition};

use super::*;
use crate::{GpuHandle, GpuLight};

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

    /// The world matrix these quantized values compose to, in absolute world metres — the space
    /// instances, the TLAS, and the shadow spaces are all built in.
    ///
    /// Orientation is a unit quaternion in 1/32767ths and scale is Q15.16; the translation composes
    /// in ticks at f64 before the single conversion to metres, so a placement far from the origin
    /// keeps its cell-local precision.
    #[must_use]
    pub fn to_matrix(&self) -> Mat4 {
        use saffron_geometry::glam::{DVec3, Quat, Vec3};
        let rotation = Quat::from_xyzw(
            f32::from(self.orientation[0]) / 32_767.0,
            f32::from(self.orientation[1]) / 32_767.0,
            f32::from(self.orientation[2]) / 32_767.0,
            f32::from(self.orientation[3]) / 32_767.0,
        )
        .normalize();
        let scale = Vec3::new(
            self.scale[0] as f32 / 65_536.0,
            self.scale[1] as f32 / 65_536.0,
            self.scale[2] as f32 / 65_536.0,
        );
        let cell_ticks = f64::from(saffron_spatial::BASE_CELL_TICKS);
        let translation = DVec3::new(
            self.cell[0] as f64 * cell_ticks + f64::from(self.local_ticks[0]),
            self.cell[1] as f64 * cell_ticks + f64::from(self.local_ticks[1]),
            self.cell[2] as f64 * cell_ticks + f64::from(self.local_ticks[2]),
        ) / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
        Mat4::from_scale_rotation_translation(scale, rotation, translation.as_vec3())
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

pub(super) fn matrix_is_finite(matrix: Mat4) -> bool {
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

/// One hierarchy page's swept local bounds, in metres.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuScenePageBounds {
    /// Swept minimum.
    pub min: [f32; 3],
    /// Swept maximum.
    pub max: [f32; 3],
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
    /// Ordered SDF-reference list — one per baked field of the prototype's mesh, empty
    /// when it baked none. Occluders are a prototype property; instances carry none.
    pub sdfs: Arc<[GpuSceneSdfHandle]>,
    /// Guaranteed-resident hierarchy root.
    pub root_page: GpuScenePageHandle,
    /// Conservative object-space bounding sphere.
    pub bounds: [f32; 4],
    /// Each cooked hierarchy page's swept local bounds, in page order — the cluster-group extent
    /// including every deformation the cook proved the payload can reach. Empty for a prototype
    /// whose mesh cooked no hierarchy.
    ///
    /// Consumers that dirty something per moving instance work from these rather than from
    /// [`GpuScenePrototypeRecord::bounds`]: one sphere over a tall sparse canopy covers many times
    /// the footprint its clusters actually occupy.
    pub page_bounds: Arc<[GpuScenePageBounds]>,
    /// Source content generation.
    pub source_generation: u32,
    /// Prototype flags.
    pub flags: u32,
    /// The authored mechanical response in its cooked integer form, or all zero when the
    /// prototype is not a plant family. See [`crate::GpuScenePrototypeGpuRecord::mechanics`]
    /// for the packing.
    pub mechanics: [u32; 4],
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
