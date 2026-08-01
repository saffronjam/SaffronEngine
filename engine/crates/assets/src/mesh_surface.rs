//! The static-mesh adapter for the shared surface-field contract.

use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};
use saffron_geometry::{MeshBvh, MeshNearestHit, MeshRayHit, Ray, Submesh, Vertex};
use saffron_rendering::{CoverageSourceKind, classify_canonical_coverage};
use saffron_spatial::{
    DecisionScalar, FieldAvailability, FieldChannel, FieldDerivative, FieldSample,
    HessianFieldSample, SurfaceAttachment, SurfaceCapabilities, SurfaceCoordinates,
    SurfaceDirtyRegion, SurfaceField, SurfaceFrame, SurfaceHit, SurfaceNearestQuery,
    SurfacePrimitiveId, SurfaceProjection, SurfaceProviderDescriptor, SurfaceProviderId,
    SurfaceRay, SurfaceRevision, SurfaceTagId, SurfaceTileDescriptor, UnitInterval,
    VectorFieldSample, WeightedSurfaceTag, WorldBounds, WorldPosition,
};
use saffron_vegetation::AlphaClassification;

/// The one phase the CPU coverage decision evaluates at. A surface-field answer reaches cooked
/// vegetation bytes, and [`SurfaceProviderDescriptor::revision`] does not cover the coverage
/// record, so a phase that moved per frame would change published bytes while every cook key
/// stayed identical. The raster path advances a separate phase for TAA.
const CANONICAL_COVERAGE_PHASE: u32 = 0;

/// CPU-resident projection of one material's canonical coverage contract.
#[derive(Clone)]
pub struct CanonicalCpuCoverage {
    /// Canonical source interpretation.
    pub source_kind: CoverageSourceKind,
    /// Alpha classification applied after filtering.
    pub classification: AlphaClassification,
    /// Material alpha multiplied into albedo-alpha sources.
    pub base_color_alpha: f32,
    /// Decoded level-zero dimensions used for filtering.
    pub texture_extent: [u32; 2],
    /// Authored source dimensions used by object-anchored stochastic coverage.
    pub hash_extent: [u32; 2],
    /// Stable object-space stochastic-coverage salt.
    pub salt: u64,
    /// Authored alpha cutoff.
    pub reference_cutoff: f32,
    /// Whether the decoded alpha already stores coverage probability.
    pub canonical_probability: bool,
    /// Material UV scale.
    pub uv_tiling: Vec2,
    /// Material UV translation.
    pub uv_offset: Vec2,
    /// Optional decoded level-zero alpha plane.
    pub alpha: Option<Arc<[u8]>>,
}

impl CanonicalCpuCoverage {
    pub(crate) fn covered(&self, uv: Vec2, anchor: Vec3) -> bool {
        let uv = uv * self.uv_tiling + self.uv_offset;
        let sampled = self.alpha.as_ref().map_or(1.0, |alpha| {
            sample_bilinear_alpha(alpha, self.texture_extent, uv)
        });
        classify_canonical_coverage(
            sampled,
            uv.to_array(),
            anchor.to_array(),
            self.source_kind,
            self.classification,
            self.base_color_alpha,
            self.hash_extent,
            self.salt,
            CANONICAL_COVERAGE_PHASE,
            self.reference_cutoff,
            0.0,
            self.canonical_probability,
            false,
        )
        .covered
    }
}

/// The index of the submesh whose index range covers `triangle_index`.
fn submesh_of_triangle(submeshes: &[Submesh], triangle_index: u32) -> Option<usize> {
    let index_offset = triangle_index.saturating_mul(3);
    submeshes.iter().position(|submesh| {
        let end = submesh.first_index.saturating_add(submesh.index_count);
        index_offset >= submesh.first_index && index_offset < end
    })
}

pub(crate) fn coverage_for_triangle<'a>(
    submeshes: &[Submesh],
    coverage: &'a [CanonicalCpuCoverage],
    triangle_index: u32,
) -> Option<&'a CanonicalCpuCoverage> {
    coverage.get(submesh_of_triangle(submeshes, triangle_index)?)
}

/// Complete immutable inputs for one static-mesh surface-provider snapshot.
pub struct StaticMeshSurfaceInput {
    /// Stable provider identity.
    pub id: SurfaceProviderId,
    /// Content revision represented by this snapshot.
    pub revision: SurfaceRevision,
    /// Complete mesh vertex stream.
    pub vertices: Arc<[Vertex]>,
    /// Flat triangle index stream.
    pub indices: Arc<[u32]>,
    /// Material-slot ranges over the index stream.
    pub submeshes: Vec<Submesh>,
    /// Mesh-local bounds minimum.
    pub bounds_min: Vec3,
    /// Mesh-local bounds maximum.
    pub bounds_max: Vec3,
    /// Cached mesh-local acceleration structure.
    pub bvh: Arc<MeshBvh>,
    /// Mesh-local to render-relative transform.
    pub model: Mat4,
    /// Exact world position represented by render-relative zero.
    pub render_origin: WorldPosition,
    /// Stable material tags indexed by material slot.
    pub material_tags: Vec<SurfaceTagId>,
    /// Canonical coverage records parallel to `submeshes`.
    pub coverage: Vec<CanonicalCpuCoverage>,
}

/// A provider over one static mesh instance and its cached mesh-local BVH.
pub struct StaticMeshSurfaceProvider {
    descriptor: SurfaceProviderDescriptor,
    vertices: Arc<[Vertex]>,
    indices: Arc<[u32]>,
    submeshes: Vec<Submesh>,
    bvh: Arc<MeshBvh>,
    model: Mat4,
    inverse: Mat4,
    render_origin: WorldPosition,
    material_tags: Vec<SurfaceTagId>,
    coverage: Vec<CanonicalCpuCoverage>,
}

impl StaticMeshSurfaceProvider {
    /// Constructs an immutable provider snapshot for one scene entity revision.
    pub fn new(input: StaticMeshSurfaceInput) -> saffron_spatial::Result<Self> {
        let StaticMeshSurfaceInput {
            id,
            revision,
            vertices,
            indices,
            submeshes,
            bounds_min,
            bounds_max,
            bvh,
            model,
            render_origin,
            material_tags,
            coverage,
        } = input;
        let determinant = model.determinant();
        let inverse = model.inverse();
        if !model.is_finite()
            || !determinant.is_finite()
            || determinant == 0.0
            || !inverse.is_finite()
        {
            return Err(saffron_spatial::Error::InvalidSurfaceTransform);
        }
        let mut minimum = Vec3::splat(f32::MAX);
        let mut maximum = Vec3::splat(f32::MIN);
        saffron_geometry::world_aabb_from_corners(
            &model,
            bounds_min,
            bounds_max,
            &mut minimum,
            &mut maximum,
        );
        let bounds = WorldBounds::from_render_relative(
            minimum.as_dvec3(),
            maximum.as_dvec3(),
            render_origin,
        )?;
        Ok(Self {
            descriptor: SurfaceProviderDescriptor {
                id,
                revision,
                bounds,
                primitive_count: indices.len() as u64 / 3,
                max_tags_per_hit: 1,
                capabilities: SurfaceCapabilities {
                    ray: true,
                    project: true,
                    nearest: true,
                    uv: true,
                    authoritative_attachments: true,
                    authoritative_fields: true,
                },
            },
            inverse,
            vertices,
            indices,
            submeshes,
            bvh,
            model,
            render_origin,
            material_tags,
            coverage,
        })
    }

    fn query_ray(&self, query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let origin = query.origin.to_render_relative(self.render_origin)?;
        let world_ray = Ray {
            origin,
            dir: query.direction.as_vec3(),
        };
        let local_ray = Ray {
            origin: self.inverse.transform_point3(world_ray.origin),
            dir: self.inverse.transform_vector3(world_ray.dir),
        };
        let Some(hit) = self
            .bvh
            .raycast_hit_filtered(&local_ray, |hit| self.hit_is_covered(hit))
        else {
            return Ok(None);
        };
        let local_point = local_ray.origin + local_ray.dir * hit.distance;
        let world_point = self.model.transform_point3(local_point);
        let distance = f64::from(world_point.distance(origin));
        if distance > query.max_distance_m {
            return Ok(None);
        }
        self.surface_hit(hit, local_point, world_point, distance)
            .map(Some)
    }

    fn hit_is_covered(&self, hit: MeshRayHit) -> bool {
        let Ok(vertices) = self.triangle_vertices(hit.triangle_index) else {
            return false;
        };
        let uv = barycentric_interpolate_vec2(
            vertices[0].uv0,
            vertices[1].uv0,
            vertices[2].uv0,
            hit.barycentric,
        );
        let anchor = barycentric_interpolate(
            vertices[0].position,
            vertices[1].position,
            vertices[2].position,
            hit.barycentric,
        );
        self.coverage_for_triangle(hit.triangle_index)
            .is_none_or(|coverage| coverage.covered(uv, anchor))
    }

    fn coverage_for_triangle(&self, triangle_index: u32) -> Option<&CanonicalCpuCoverage> {
        coverage_for_triangle(&self.submeshes, &self.coverage, triangle_index)
    }

    fn surface_hit(
        &self,
        hit: MeshRayHit,
        local_point: Vec3,
        world_point: Vec3,
        distance_m: f64,
    ) -> saffron_spatial::Result<SurfaceHit> {
        self.surface_from_parts(
            hit.triangle_index,
            hit.barycentric,
            local_point,
            world_point,
            distance_m,
            true,
        )
    }

    fn nearest_surface_hit(
        &self,
        hit: MeshNearestHit,
        query_point: Vec3,
    ) -> saffron_spatial::Result<SurfaceHit> {
        let local_point = self.inverse.transform_point3(hit.point);
        self.surface_from_parts(
            hit.triangle_index,
            hit.barycentric,
            local_point,
            hit.point,
            f64::from(hit.point.distance(query_point)),
            true,
        )
    }

    fn surface_from_parts(
        &self,
        triangle_index: u32,
        barycentric: [f32; 3],
        local_point: Vec3,
        world_point: Vec3,
        distance_m: f64,
        attach: bool,
    ) -> saffron_spatial::Result<SurfaceHit> {
        let vertices = self.triangle_vertices(triangle_index)?;
        let local_normal = (vertices[1].position - vertices[0].position)
            .cross(vertices[2].position - vertices[0].position)
            .normalize_or_zero();
        let world_normal = self
            .inverse
            .transpose()
            .transform_vector3(local_normal)
            .normalize_or_zero();
        let local_tangent = barycentric_interpolate(
            Vec3::new(
                vertices[0].tangent[0],
                vertices[0].tangent[1],
                vertices[0].tangent[2],
            ),
            Vec3::new(
                vertices[1].tangent[0],
                vertices[1].tangent[1],
                vertices[1].tangent[2],
            ),
            Vec3::new(
                vertices[2].tangent[0],
                vertices[2].tangent[1],
                vertices[2].tangent[2],
            ),
            barycentric,
        );
        let world_tangent = self.model.transform_vector3(local_tangent);
        let source_handedness = barycentric[0] * vertices[0].tangent[3]
            + barycentric[1] * vertices[1].tangent[3]
            + barycentric[2] * vertices[2].tangent[3];
        let handedness = source_handedness.signum() * self.model.determinant().signum();
        let frame = SurfaceFrame::new(world_normal, world_tangent, handedness)
            .or_else(|_| SurfaceFrame::from_normal(world_normal))?;
        let uv = barycentric_interpolate_vec2(
            vertices[0].uv0,
            vertices[1].uv0,
            vertices[2].uv0,
            barycentric,
        );
        let attachment = attach
            .then(|| {
                SurfaceAttachment::from_f32(
                    self.descriptor.id,
                    SurfacePrimitiveId(u64::from(triangle_index)),
                    barycentric,
                    self.descriptor.revision,
                )
            })
            .transpose()?;
        let tags = self.tags_for_triangle(triangle_index);
        Ok(SurfaceHit {
            provider: self.descriptor.id,
            position: WorldPosition::from_render_relative(world_point, self.render_origin)?,
            distance_m,
            frame,
            coordinates: SurfaceCoordinates {
                uv: Some(uv),
                projection: local_point.as_dvec3(),
            },
            attachment,
            tags,
            revision: self.descriptor.revision,
        })
    }

    fn triangle_vertices(&self, triangle_index: u32) -> saffron_spatial::Result<[Vertex; 3]> {
        let base = usize::try_from(triangle_index)
            .ok()
            .and_then(|value| value.checked_mul(3))
            .ok_or(saffron_spatial::Error::NumericOverflow)?;
        let indices = self
            .indices
            .get(base..base + 3)
            .ok_or(saffron_spatial::Error::FieldUnavailable)?;
        let vertex = |index: u32| {
            self.vertices
                .get(index as usize)
                .copied()
                .ok_or(saffron_spatial::Error::FieldUnavailable)
        };
        Ok([
            vertex(indices[0])?,
            vertex(indices[1])?,
            vertex(indices[2])?,
        ])
    }

    fn tags_for_triangle(&self, triangle_index: u32) -> Vec<WeightedSurfaceTag> {
        let tag = submesh_of_triangle(&self.submeshes, triangle_index)
            .and_then(|index| self.submeshes.get(index))
            .and_then(|submesh| {
                self.material_tags
                    .get(submesh.material_slot as usize)
                    .copied()
            });
        tag.map_or_else(Vec::new, |tag| {
            vec![WeightedSurfaceTag {
                tag,
                weight: UnitInterval::ONE,
            }]
        })
    }

    fn hit_from_attachment(
        &self,
        attachment: SurfaceAttachment,
    ) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if attachment.provider != self.descriptor.id
            || attachment.revision != self.descriptor.revision
        {
            return Ok(None);
        }
        let triangle_index = u32::try_from(attachment.primitive.0)
            .map_err(|_| saffron_spatial::Error::NumericOverflow)?;
        let barycentric = attachment.barycentric.map(|weight| weight.to_f64() as f32);
        let vertices = self.triangle_vertices(triangle_index)?;
        let local_point = barycentric_interpolate(
            vertices[0].position,
            vertices[1].position,
            vertices[2].position,
            barycentric,
        );
        let world_point = self.model.transform_point3(local_point);
        self.surface_from_parts(
            triangle_index,
            barycentric,
            local_point,
            world_point,
            0.0,
            true,
        )
        .map(Some)
    }
}

impl SurfaceField for StaticMeshSurfaceProvider {
    fn descriptor(&self) -> SurfaceProviderDescriptor {
        self.descriptor.clone()
    }

    fn field_channels(&self) -> Vec<FieldChannel> {
        vec![FieldChannel::Altitude, FieldChannel::Slope]
    }

    fn raycast(&self, query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
        self.query_ray(query)
    }

    fn project(&self, query: &SurfaceProjection) -> saffron_spatial::Result<Option<SurfaceHit>> {
        self.query_ray(&SurfaceRay::new(
            query.origin,
            query.direction,
            query.max_distance_m,
        )?)
    }

    fn nearest(&self, query: &SurfaceNearestQuery) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let point = query.position.to_render_relative(self.render_origin)?;
        let Some(hit) = self.bvh.nearest_hit_transformed(point, self.model) else {
            return Ok(None);
        };
        if f64::from(hit.distance) > query.max_distance_m {
            return Ok(None);
        }
        self.nearest_surface_hit(hit, point).map(Some)
    }

    fn availability(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _bounds: WorldBounds,
    ) -> FieldAvailability {
        if derivative == FieldDerivative::Value
            && matches!(channel, FieldChannel::Altitude | FieldChannel::Slope)
        {
            FieldAvailability::Complete
        } else {
            FieldAvailability::Unavailable
        }
    }

    fn estimated_samples(&self, channel: FieldChannel, _bounds: WorldBounds) -> u64 {
        if matches!(channel, FieldChannel::Altitude | FieldChannel::Slope) {
            self.descriptor.primitive_count
        } else {
            0
        }
    }

    fn sample_scalar(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        position: WorldPosition,
    ) -> saffron_spatial::Result<FieldSample> {
        if derivative != FieldDerivative::Value {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        let query = SurfaceNearestQuery::new(position, f64::MAX)?;
        let hit = self
            .nearest(&query)?
            .ok_or(saffron_spatial::Error::FieldUnavailable)?;
        let value = match channel {
            FieldChannel::Altitude => DecisionScalar::from_f64(hit.position.world_meters().y)?,
            FieldChannel::Slope => {
                DecisionScalar::from_f64(f64::from(1.0 - hit.frame.normal.y.abs()))?
            }
            _ => return Err(saffron_spatial::Error::FieldUnavailable),
        };
        Ok(FieldSample {
            channel,
            derivative,
            value,
            revision: self.descriptor.revision,
        })
    }

    fn sample_vector(
        &self,
        _channel: FieldChannel,
        _derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<VectorFieldSample> {
        Err(saffron_spatial::Error::FieldUnavailable)
    }

    fn sample_hessian(
        &self,
        _channel: FieldChannel,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<HessianFieldSample> {
        Err(saffron_spatial::Error::FieldUnavailable)
    }

    fn authoritative_tiles(
        &self,
        _channel: FieldChannel,
        _bounds: WorldBounds,
    ) -> Vec<SurfaceTileDescriptor> {
        Vec::new()
    }

    fn changes_since(&self, revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion> {
        (revision != self.descriptor.revision)
            .then_some(SurfaceDirtyRegion {
                bounds: self.descriptor.bounds,
                revision: self.descriptor.revision,
            })
            .into_iter()
            .collect()
    }

    fn reproject_attachment(
        &self,
        attachment: SurfaceAttachment,
    ) -> saffron_spatial::Result<Option<SurfaceHit>> {
        self.hit_from_attachment(attachment)
    }
}

fn barycentric_interpolate(a: Vec3, b: Vec3, c: Vec3, weights: [f32; 3]) -> Vec3 {
    a * weights[0] + b * weights[1] + c * weights[2]
}

fn barycentric_interpolate_vec2(a: Vec2, b: Vec2, c: Vec2, weights: [f32; 3]) -> Vec2 {
    a * weights[0] + b * weights[1] + c * weights[2]
}

fn sample_bilinear_alpha(alpha: &[u8], extent: [u32; 2], uv: Vec2) -> f32 {
    let [width, height] = extent;
    if width == 0
        || height == 0
        || alpha.len() != width as usize * height as usize
        || !uv.is_finite()
    {
        return 1.0;
    }
    let texel = uv * Vec2::new(width as f32, height as f32) - Vec2::splat(0.5);
    let base = texel.floor();
    let fraction = texel - base;
    let sample = |x: i32, y: i32| {
        let x = x.rem_euclid(width as i32) as usize;
        let y = y.rem_euclid(height as i32) as usize;
        f32::from(alpha[y * width as usize + x]) * (1.0 / 255.0)
    };
    let x = base.x as i32;
    let y = base.y as i32;
    let top = sample(x, y) + (sample(x + 1, y) - sample(x, y)) * fraction.x;
    let bottom = sample(x, y + 1) + (sample(x + 1, y + 1) - sample(x, y + 1)) * fraction.x;
    top + (bottom - top) * fraction.y
}

#[cfg(test)]
mod tests {
    use glam::{DVec3, Mat4, Vec2, Vec3};
    use saffron_geometry::{MeshBvh, Submesh, Vertex};
    use saffron_spatial::{
        SurfaceField, SurfaceProviderId, SurfaceRay, SurfaceRevision, SurfaceTagId, WorldPosition,
    };

    use saffron_rendering::CoverageSourceKind;
    use saffron_vegetation::AlphaClassification;

    use super::{
        CanonicalCpuCoverage, StaticMeshSurfaceInput, StaticMeshSurfaceProvider,
        sample_bilinear_alpha,
    };

    fn triangle_provider(
        render_origin: WorldPosition,
        render_translation_x: f32,
    ) -> StaticMeshSurfaceProvider {
        let vertices: std::sync::Arc<[Vertex]> = vec![
            Vertex {
                position: Vec3::new(-1.0, 0.0, -1.0),
                normal: Vec3::Y,
                uv0: Vec2::new(0.0, 0.0),
                tangent: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                position: Vec3::new(0.0, 0.0, 1.0),
                normal: Vec3::Y,
                uv0: Vec2::new(0.5, 1.0),
                tangent: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                position: Vec3::new(1.0, 0.0, -1.0),
                normal: Vec3::Y,
                uv0: Vec2::new(1.0, 0.0),
                tangent: [1.0, 0.0, 0.0, 1.0],
            },
        ]
        .into();
        let indices: std::sync::Arc<[u32]> = vec![0, 1, 2].into();
        let positions: Vec<Vec3> = vertices.iter().map(|vertex| vertex.position).collect();
        let bvh = MeshBvh::build(&positions, &indices).unwrap();
        StaticMeshSurfaceProvider::new(StaticMeshSurfaceInput {
            id: SurfaceProviderId(7),
            revision: SurfaceRevision(11),
            vertices,
            indices,
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
            bounds_min: Vec3::new(-1.0, 0.0, -1.0),
            bounds_max: Vec3::new(1.0, 0.0, 1.0),
            bvh: std::sync::Arc::new(bvh),
            model: Mat4::from_translation(Vec3::new(render_translation_x, 0.0, 0.0)),
            render_origin,
            material_tags: vec![SurfaceTagId(23)],
            coverage: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn surface_hit_is_identical_after_render_origin_rebase() {
        let first_origin =
            WorldPosition::from_world_meters(DVec3::new(1_000_000.0, 0.0, 0.0)).unwrap();
        let second_origin = first_origin
            .offset_meters(DVec3::new(8.0, 0.0, 0.0))
            .unwrap();
        let first = triangle_provider(first_origin, 10.0);
        let second = triangle_provider(second_origin, 2.0);
        let query_origin = first_origin
            .offset_meters(DVec3::new(10.0, 5.0, 0.0))
            .unwrap();
        let query = SurfaceRay::new(query_origin, DVec3::NEG_Y, 10.0).unwrap();
        let first_hit = first.raycast(&query).unwrap().unwrap();
        let second_hit = second.raycast(&query).unwrap().unwrap();
        assert_eq!(first_hit.position, second_hit.position);
        assert_eq!(first_hit.frame, second_hit.frame);
        assert_eq!(first_hit.coordinates.uv, second_hit.coordinates.uv);
        assert_eq!(first_hit.tags, second_hit.tags);
        assert_eq!(first_hit.attachment, second_hit.attachment);
        assert_eq!(first_hit.revision, second_hit.revision);
        assert_eq!(first.descriptor().bounds, second.descriptor().bounds);
    }

    #[test]
    fn singular_transform_is_rejected() {
        let vertices: std::sync::Arc<[Vertex]> = Vec::new().into();
        let indices: std::sync::Arc<[u32]> = Vec::new().into();
        let bvh = MeshBvh::build(&[Vec3::ZERO, Vec3::X, Vec3::Z], &[0, 1, 2]).unwrap();
        let result = StaticMeshSurfaceProvider::new(StaticMeshSurfaceInput {
            id: SurfaceProviderId(1),
            revision: SurfaceRevision(1),
            vertices,
            indices,
            submeshes: Vec::new(),
            bounds_min: Vec3::ZERO,
            bounds_max: Vec3::ONE,
            bvh: std::sync::Arc::new(bvh),
            model: Mat4::from_scale(Vec3::new(1.0, 0.0, 1.0)),
            render_origin: WorldPosition::origin(),
            material_tags: Vec::new(),
            coverage: Vec::new(),
        });
        assert!(matches!(
            result,
            Err(saffron_spatial::Error::InvalidSurfaceTransform)
        ));
    }

    #[test]
    fn raycast_rejects_a_transparent_near_triangle_and_returns_the_solid_triangle() {
        let vertices: std::sync::Arc<[Vertex]> = vec![
            Vertex {
                position: Vec3::new(-1.0, 0.0, -1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(0.0, 0.0, 1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(1.0, 0.0, -1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(-1.0, -1.0, -1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(0.0, -1.0, 1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(1.0, -1.0, -1.0),
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
        ]
        .into();
        let indices: std::sync::Arc<[u32]> = vec![0, 1, 2, 3, 4, 5].into();
        let positions = vertices
            .iter()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>();
        let provider = StaticMeshSurfaceProvider::new(StaticMeshSurfaceInput {
            id: SurfaceProviderId(31),
            revision: SurfaceRevision(1),
            vertices,
            indices: std::sync::Arc::clone(&indices),
            submeshes: vec![
                Submesh {
                    first_index: 0,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 0,
                },
                Submesh {
                    first_index: 3,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 1,
                },
            ],
            bounds_min: Vec3::new(-1.0, -1.0, -1.0),
            bounds_max: Vec3::new(1.0, 0.0, 1.0),
            bvh: std::sync::Arc::new(MeshBvh::build(&positions, &indices).unwrap()),
            model: Mat4::IDENTITY,
            render_origin: WorldPosition::origin(),
            material_tags: Vec::new(),
            coverage: vec![
                CanonicalCpuCoverage {
                    source_kind: CoverageSourceKind::Texture,
                    classification: AlphaClassification::Masked,
                    base_color_alpha: 1.0,
                    texture_extent: [1, 1],
                    hash_extent: [1, 1],
                    salt: 0,
                    reference_cutoff: 0.5,
                    canonical_probability: true,
                    uv_tiling: Vec2::ONE,
                    uv_offset: Vec2::ZERO,
                    alpha: Some(vec![0].into()),
                },
                CanonicalCpuCoverage {
                    source_kind: CoverageSourceKind::ModeledGeometry,
                    classification: AlphaClassification::Opaque,
                    base_color_alpha: 1.0,
                    texture_extent: [1, 1],
                    hash_extent: [1, 1],
                    salt: 0,
                    reference_cutoff: 0.5,
                    canonical_probability: true,
                    uv_tiling: Vec2::ONE,
                    uv_offset: Vec2::ZERO,
                    alpha: None,
                },
            ],
        })
        .unwrap();
        let ray = SurfaceRay::new(
            WorldPosition::from_world_meters(DVec3::new(0.0, 2.0, 0.0)).unwrap(),
            DVec3::NEG_Y,
            10.0,
        )
        .unwrap();
        let hit = provider.raycast(&ray).unwrap().unwrap();
        assert_eq!(hit.position.world_meters().y, -1.0);
    }

    #[test]
    fn bilinear_alpha_matches_repeat_sampler_texel_centres() {
        let alpha = [0, 255];
        assert_eq!(
            sample_bilinear_alpha(&alpha, [2, 1], Vec2::new(0.25, 0.5)),
            0.0
        );
        assert_eq!(
            sample_bilinear_alpha(&alpha, [2, 1], Vec2::new(0.75, 0.5)),
            1.0
        );
        assert_eq!(
            sample_bilinear_alpha(&alpha, [2, 1], Vec2::new(0.0, 0.5)),
            0.5
        );
    }
}
