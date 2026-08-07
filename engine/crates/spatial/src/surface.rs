//! Surface-provider identity, queries, fields, attachments, and invalidation.

use glam::{DVec3, Vec2, Vec3};

use crate::{
    DecisionHessian3, DecisionScalar, DecisionVec3, Error, Result, UnitInterval, WorldBounds,
    WorldPosition,
};

/// Stable identity of a surface provider, independent of its runtime container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceProviderId(pub u64);

/// Stable content revision of one provider snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceRevision(pub u64);

/// Stable primitive identity inside a provider revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfacePrimitiveId(pub u64);

/// Stable material, biome, or user-defined surface classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceTagId(pub u64);

/// One weighted classification on a hit or field sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeightedSurfaceTag {
    pub tag: SurfaceTagId,
    /// Canonical normalized contribution.
    pub weight: UnitInterval,
}

/// A stable attachment to one provider primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceAttachment {
    pub provider: SurfaceProviderId,
    /// Primitive identity within the provider.
    pub primitive: SurfacePrimitiveId,
    /// Canonical triangle barycentrics in vertex order. The three values sum to `u16::MAX`.
    pub barycentric: [UnitInterval; 3],
    /// Provider revision against which this attachment was resolved.
    pub revision: SurfaceRevision,
}

impl SurfaceAttachment {
    /// Constructs an attachment with a checked canonical unit sum.
    pub fn new(
        provider: SurfaceProviderId,
        primitive: SurfacePrimitiveId,
        barycentric: [UnitInterval; 3],
        revision: SurfaceRevision,
    ) -> Result<Self> {
        let sum: u32 = barycentric
            .iter()
            .map(|value| u32::from(value.bits()))
            .sum();
        if sum != u32::from(u16::MAX) {
            return Err(Error::InvalidBarycentrics);
        }
        Ok(Self {
            provider,
            primitive,
            barycentric,
            revision,
        })
    }

    /// Quantizes finite non-negative floating barycentrics by largest remainder.
    pub fn from_f32(
        provider: SurfaceProviderId,
        primitive: SurfacePrimitiveId,
        barycentric: [f32; 3],
        revision: SurfaceRevision,
    ) -> Result<Self> {
        if barycentric
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(Error::InvalidBarycentrics);
        }
        let sum: f64 = barycentric.iter().map(|value| f64::from(*value)).sum();
        if !sum.is_finite() || sum <= 0.0 {
            return Err(Error::InvalidBarycentrics);
        }
        let scaled = barycentric.map(|value| f64::from(value) / sum * f64::from(u16::MAX));
        let mut bits = scaled.map(|value| value.floor() as u16);
        let assigned: u32 = bits.iter().map(|value| u32::from(*value)).sum();
        let mut remaining = u32::from(u16::MAX) - assigned;
        let mut order = [0_usize, 1, 2];
        order.sort_by(|left, right| {
            let left_fraction = scaled[*left] - scaled[*left].floor();
            let right_fraction = scaled[*right] - scaled[*right].floor();
            right_fraction
                .total_cmp(&left_fraction)
                .then_with(|| left.cmp(right))
        });
        for index in order {
            if remaining == 0 {
                break;
            }
            bits[index] = bits[index]
                .checked_add(1)
                .ok_or(Error::InvalidBarycentrics)?;
            remaining -= 1;
        }
        Self::new(
            provider,
            primitive,
            bits.map(UnitInterval::from_bits),
            revision,
        )
    }
}

/// An orthonormal right-handed frame at a surface hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceFrame {
    /// Geometric normal.
    pub normal: Vec3,
    /// UV-aligned tangent where the provider has one, otherwise a deterministic basis tangent.
    pub tangent: Vec3,
    /// `normal × tangent`, including source handedness.
    pub bitangent: Vec3,
}

impl SurfaceFrame {
    /// Orthonormalizes a finite normal and tangent, preserving the requested handedness.
    pub fn new(normal: Vec3, tangent: Vec3, handedness: f32) -> Result<Self> {
        if !normal.is_finite() || !tangent.is_finite() || !handedness.is_finite() {
            return Err(Error::DegenerateDirection);
        }
        let normal = normal.normalize_or_zero();
        let tangent = (tangent - normal * normal.dot(tangent)).normalize_or_zero();
        if normal == Vec3::ZERO || tangent == Vec3::ZERO || handedness == 0.0 {
            return Err(Error::DegenerateDirection);
        }
        let bitangent = normal.cross(tangent) * handedness.signum();
        Ok(Self {
            normal,
            tangent,
            bitangent,
        })
    }

    /// Builds a deterministic tangent frame from only a finite non-zero normal.
    pub fn from_normal(normal: Vec3) -> Result<Self> {
        if !normal.is_finite() {
            return Err(Error::DegenerateDirection);
        }
        let normal = normal.normalize_or_zero();
        if normal == Vec3::ZERO {
            return Err(Error::DegenerateDirection);
        }
        let sign = if normal.z >= 0.0 { 1.0 } else { -1.0 };
        let a = -1.0 / (sign + normal.z);
        let tangent = Vec3::new(
            1.0 + sign * normal.x * normal.x * a,
            sign * normal.x * normal.y * a,
            -sign * normal.x,
        );
        Self::new(normal, tangent, 1.0)
    }
}

/// Provider coordinates associated with a hit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfaceCoordinates {
    /// Primary UV set, when the provider defines one.
    pub uv: Option<Vec2>,
    /// Provider-local projection coordinate for field lookup.
    pub projection: DVec3,
}

/// A complete nearest surface hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceHit {
    pub provider: SurfaceProviderId,
    /// Exact quantized world position.
    pub position: WorldPosition,
    /// Metric distance from the query origin.
    pub distance_m: f64,
    /// Geometric tangent frame.
    pub frame: SurfaceFrame,
    /// UV and provider projection coordinates.
    pub coordinates: SurfaceCoordinates,
    /// Stable attachment when the provider supports authoritative attachments.
    pub attachment: Option<SurfaceAttachment>,
    /// Weighted material and surface classifications, sorted by tag id.
    pub tags: Vec<WeightedSurfaceTag>,
    /// Provider revision used for the result.
    pub revision: SurfaceRevision,
}

/// Capabilities declared before a query is dispatched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceCapabilities {
    /// Arbitrary-direction ray queries.
    pub ray: bool,
    /// Directional projection queries.
    pub project: bool,
    /// Nearest-point queries.
    pub nearest: bool,
    /// UV coordinates are available.
    pub uv: bool,
    /// Attachments remain meaningful for authoritative placement at this revision.
    pub authoritative_attachments: bool,
    /// Canonical quantized tiles are available for cross-machine runtime decisions.
    pub authoritative_fields: bool,
}

/// Immutable provider metadata used for planning and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceProviderDescriptor {
    pub id: SurfaceProviderId,
    /// Monotonic content revision.
    pub revision: SurfaceRevision,
    /// Exact world bounds.
    pub bounds: WorldBounds,
    /// Stable primitive count at this revision.
    pub primitive_count: u64,
    /// Maximum weighted tags returned by any query hit.
    pub max_tags_per_hit: u32,
    /// Query and authority capabilities.
    pub capabilities: SurfaceCapabilities,
}

/// A normalized arbitrary-direction surface ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceRay {
    /// Exact world origin.
    pub origin: WorldPosition,
    /// Unit world direction.
    pub direction: DVec3,
    /// Maximum metric distance.
    pub max_distance_m: f64,
}

impl SurfaceRay {
    /// Constructs a checked normalized ray.
    pub fn new(origin: WorldPosition, direction: DVec3, max_distance_m: f64) -> Result<Self> {
        if !direction.is_finite()
            || direction.length_squared() == 0.0
            || !max_distance_m.is_finite()
            || max_distance_m < 0.0
        {
            return Err(Error::InvalidDistance);
        }
        Ok(Self {
            origin,
            direction: direction.normalize(),
            max_distance_m,
        })
    }
}

/// A directional surface projection request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceProjection {
    /// Exact point to project.
    pub origin: WorldPosition,
    /// Unit projection direction.
    pub direction: DVec3,
    /// Maximum metric distance.
    pub max_distance_m: f64,
}

impl SurfaceProjection {
    /// Constructs a checked projection.
    pub fn new(origin: WorldPosition, direction: DVec3, max_distance_m: f64) -> Result<Self> {
        let ray = SurfaceRay::new(origin, direction, max_distance_m)?;
        Ok(Self {
            origin: ray.origin,
            direction: ray.direction,
            max_distance_m: ray.max_distance_m,
        })
    }
}

/// A nearest-point request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceNearestQuery {
    /// Exact query point.
    pub position: WorldPosition,
    /// Maximum metric distance.
    pub max_distance_m: f64,
}

impl SurfaceNearestQuery {
    /// Constructs a checked nearest query.
    pub fn new(position: WorldPosition, max_distance_m: f64) -> Result<Self> {
        if !max_distance_m.is_finite() || max_distance_m < 0.0 {
            return Err(Error::InvalidDistance);
        }
        Ok(Self {
            position,
            max_distance_m,
        })
    }
}

/// Canonical scalar/vector channels exposed by surface providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldChannel {
    Altitude,
    Slope,
    /// Mean curvature.
    Curvature,
    Concavity,
    /// Drainage accumulation.
    Drainage,
    Moisture,
    Temperature,
    Precipitation,
    /// Direct and indirect sunlight availability.
    Sunlight,
    /// Exposure to open sky and wind.
    Exposure,
    WaterDistance,
    WaterDepth,
    /// Signed blocker distance.
    SignedBlocker,
    /// Distance to a spline network.
    SplineDistance,
    /// A stable user-defined channel.
    User(u64),
}

impl FieldChannel {
    /// The stable canonical tag and user payload used by persisted vegetation formats.
    #[must_use]
    pub const fn canonical_code(self) -> (u8, u64) {
        match self {
            Self::Altitude => (0, 0),
            Self::Slope => (1, 0),
            Self::Curvature => (2, 0),
            Self::Concavity => (3, 0),
            Self::Drainage => (4, 0),
            Self::Moisture => (5, 0),
            Self::Temperature => (6, 0),
            Self::Precipitation => (7, 0),
            Self::Sunlight => (8, 0),
            Self::Exposure => (9, 0),
            Self::WaterDistance => (10, 0),
            Self::WaterDepth => (11, 0),
            Self::SignedBlocker => (12, 0),
            Self::SplineDistance => (13, 0),
            Self::User(value) => (14, value),
        }
    }
}

/// Which derivative is requested for a field channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldDerivative {
    #[default]
    Value,
    /// The first spatial derivative.
    Gradient,
    /// The second spatial derivative.
    Hessian,
}

/// Planning-time availability of one channel over bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldAvailability {
    /// Every requested point has a canonical value.
    Complete,
    /// Some requested points are unavailable.
    Partial,
    /// The provider does not expose this channel.
    Unavailable,
}

/// A canonical quantized tile descriptor for authoritative field evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceTileDescriptor {
    pub provider: SurfaceProviderId,
    pub revision: SurfaceRevision,
    /// Covered exact world bounds.
    pub bounds: WorldBounds,
    /// Samples along each axis.
    pub dimensions: [u32; 3],
    /// Canonical value quantum in decision-scalar bits.
    pub value_quantum_bits: i32,
}

/// One canonical scalar field result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSample {
    pub channel: FieldChannel,
    pub derivative: FieldDerivative,
    /// Canonical scalar value.
    pub value: DecisionScalar,
    pub revision: SurfaceRevision,
}

/// One canonical vector field result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VectorFieldSample {
    pub channel: FieldChannel,
    pub derivative: FieldDerivative,
    /// Canonical vector value.
    pub value: DecisionVec3,
    pub revision: SurfaceRevision,
}

/// One canonical symmetric Hessian field result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HessianFieldSample {
    pub channel: FieldChannel,
    /// Always [`FieldDerivative::Hessian`].
    pub derivative: FieldDerivative,
    /// Canonical symmetric Hessian.
    pub value: DecisionHessian3,
    pub revision: SurfaceRevision,
}

/// A revision-tagged dirty region emitted after provider edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceDirtyRegion {
    /// Changed exact world bounds.
    pub bounds: WorldBounds,
    pub revision: SurfaceRevision,
}

/// A surface and environmental-field provider.
pub trait SurfaceField: Send + Sync {
    /// Immutable provider metadata.
    fn descriptor(&self) -> SurfaceProviderDescriptor;
    /// Canonically sorted field channels this immutable provider exposes.
    fn field_channels(&self) -> Vec<FieldChannel>;
    /// Nearest hit along an arbitrary world ray.
    fn raycast(&self, query: &SurfaceRay) -> Result<Option<SurfaceHit>>;
    /// First hit along a projection direction.
    fn project(&self, query: &SurfaceProjection) -> Result<Option<SurfaceHit>>;
    /// Nearest surface point within the query radius.
    fn nearest(&self, query: &SurfaceNearestQuery) -> Result<Option<SurfaceHit>>;
    /// Planning-time field availability over exact bounds.
    fn availability(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        bounds: WorldBounds,
    ) -> FieldAvailability;
    /// Cardinality estimate used to budget a query before dispatch.
    fn estimated_samples(&self, channel: FieldChannel, bounds: WorldBounds) -> u64;
    /// Canonical scalar field sample.
    fn sample_scalar(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        position: WorldPosition,
    ) -> Result<FieldSample>;
    /// Canonical vector field sample.
    fn sample_vector(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        position: WorldPosition,
    ) -> Result<VectorFieldSample>;
    /// Canonical symmetric Hessian field sample.
    fn sample_hessian(
        &self,
        channel: FieldChannel,
        position: WorldPosition,
    ) -> Result<HessianFieldSample>;
    /// Canonical tile metadata covering bounds, when authoritative field data exists.
    fn authoritative_tiles(
        &self,
        channel: FieldChannel,
        bounds: WorldBounds,
    ) -> Vec<SurfaceTileDescriptor>;
    /// Dirty-region notifications for content different from `revision`.
    fn changes_since(&self, revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion>;
    /// Reprojects an attachment after an edit, or returns `None` when it is orphaned.
    fn reproject_attachment(&self, attachment: SurfaceAttachment) -> Result<Option<SurfaceHit>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_requires_exact_unit_sum() {
        let good = [
            UnitInterval::from_bits(10_000),
            UnitInterval::from_bits(20_000),
            UnitInterval::from_bits(35_535),
        ];
        assert!(
            SurfaceAttachment::new(
                SurfaceProviderId(1),
                SurfacePrimitiveId(2),
                good,
                SurfaceRevision(3)
            )
            .is_ok()
        );
        assert!(
            SurfaceAttachment::new(
                SurfaceProviderId(1),
                SurfacePrimitiveId(2),
                [UnitInterval::ZERO; 3],
                SurfaceRevision(3)
            )
            .is_err()
        );
    }

    #[test]
    fn normal_only_frame_is_orthonormal() {
        let frame = SurfaceFrame::from_normal(Vec3::new(0.3, 0.8, -0.2)).unwrap();
        assert!(frame.normal.dot(frame.tangent).abs() < 1e-6);
        assert!(frame.normal.dot(frame.bitangent).abs() < 1e-6);
        assert!((frame.normal.length() - 1.0).abs() < 1e-6);
    }
}
