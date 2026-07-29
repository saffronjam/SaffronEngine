//! Device-free reference evaluation for triangle-to-aggregate-voxel transitions.

use std::collections::BTreeSet;

use glam::Vec3;

use crate::{
    AppearanceError, Error, HierarchyRepresentation, PortableBounds, PortableMaterialMoments,
    PortableTriangleCluster, PortableVirtualHierarchy, PortableVoxelBrick, Result,
    validate_portable_virtual_hierarchy,
};

/// One deterministic orthographic view and incident-light direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HierarchyReferenceFixture {
    /// Direction from the evaluated object toward the camera.
    pub view_direction: Vec3,
    /// Direction from the evaluated object toward the light.
    pub light_direction: Vec3,
}

/// The per-part wind modes an aggregate voxel cannot express, at their wind-independent
/// saturation, in the hierarchy's local units.
///
/// The triangle representation swings each assembly use about its pivot (the branch mode) and
/// shimmers leaf parts across the wind (flutter). An aggregate brick has no parts and applies
/// neither — it keeps only the whole-plant sway, which both representations share. That
/// collapse is the point of aggregating, and it is also an appearance difference the declared
/// transition error has to cover, or a plant visibly stiffens as the cut coarsens.
///
/// Both amplitudes SATURATE: the runtime clamps the wind term before scaling it by the authored
/// response, so the largest displacement either mode can ever reach is a property of the family
/// alone and not of any particular gust. That is what makes a cook-time bound possible.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModalAggregationBound {
    /// Saturated branch-mode amplitude. The node's own extent supplies the lever.
    pub branch: f32,
    /// Saturated leaf-flutter amplitude.
    pub flutter: f32,
}

impl ModalAggregationBound {
    /// The largest displacement the modes can apply inside a node spanning `extent` local
    /// units. The branch term scales by the lever from the use pivot, clamped exactly as the
    /// vertex path clamps it; the flutter term is height-weighted and so bounded by its
    /// amplitude alone.
    #[must_use]
    pub fn displacement(self, extent: f32) -> f32 {
        self.branch.max(0.0) * extent.clamp(0.0, 4.0) * 0.25 + self.flutter.max(0.0)
    }

    /// Whether the family applies no modal terms at all, so aggregating drops nothing.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.branch <= 0.0 && self.flutter <= 0.0
    }
}

/// Measured error for one voxel node against its finest triangle descendants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriangleVoxelReferenceComparison {
    /// Voxel hierarchy node under evaluation.
    pub voxel_node: u32,
    /// Finest triangle nodes rendered as the reference.
    pub triangle_nodes: Vec<u32>,
    /// Component-wise maximum measured across every supplied fixture.
    pub measured: AppearanceError,
    /// Error declared by the voxel hierarchy node.
    pub declared: AppearanceError,
    /// Number of view/light fixtures evaluated.
    pub fixture_count: u32,
}

impl TriangleVoxelReferenceComparison {
    /// Whether every measured appearance component is covered by the declared transition error.
    #[must_use]
    pub const fn is_within_declared_error(&self) -> bool {
        self.measured.silhouette <= self.declared.silhouette
            && self.measured.coverage <= self.declared.coverage
            && self.measured.transmission <= self.declared.transmission
            && self.measured.material <= self.declared.material
            && self.measured.normal_distribution <= self.declared.normal_distribution
    }
}

/// Canonical direction set used by device-free hierarchy conformance tests.
#[must_use]
pub fn canonical_hierarchy_reference_fixtures() -> Vec<HierarchyReferenceFixture> {
    let views = [
        Vec3::X,
        Vec3::Y,
        Vec3::Z,
        Vec3::new(1.0, 1.0, 1.0).normalize(),
        Vec3::new(-2.0, 1.0, 3.0).normalize(),
        Vec3::new(1.0, -3.0, 2.0).normalize(),
    ];
    let lights = [
        Vec3::new(1.0, 2.0, 3.0).normalize(),
        Vec3::new(-3.0, 1.0, 2.0).normalize(),
    ];
    views
        .into_iter()
        .zip(lights.into_iter().cycle())
        .map(
            |(view_direction, light_direction)| HierarchyReferenceFixture {
                view_direction,
                light_direction,
            },
        )
        .collect()
}

/// Renders every triangle-to-voxel transition in software and measures its appearance error.
pub fn compare_triangle_voxel_transitions(
    hierarchy: &PortableVirtualHierarchy,
    fixtures: &[HierarchyReferenceFixture],
    resolution: u32,
    modal: ModalAggregationBound,
) -> Result<Vec<TriangleVoxelReferenceComparison>> {
    validate_portable_virtual_hierarchy(hierarchy)?;
    if fixtures.is_empty() || !(8..=512).contains(&resolution) {
        return Err(reference_error("fixtures"));
    }
    if fixtures.iter().any(|fixture| {
        !fixture.view_direction.is_finite()
            || !fixture.light_direction.is_finite()
            || fixture.view_direction.length_squared() <= f32::EPSILON
            || fixture.light_direction.length_squared() <= f32::EPSILON
    }) {
        return Err(reference_error("directions"));
    }

    let mut comparisons = Vec::new();
    for node in &hierarchy.nodes {
        let HierarchyRepresentation::Voxel { brick } = node.representation else {
            continue;
        };
        let triangle_nodes = finest_triangle_descendants(hierarchy, node.id)?;
        if triangle_nodes.is_empty() {
            continue;
        }
        let voxel = hierarchy
            .voxel_bricks
            .get(brick as usize)
            .ok_or_else(|| reference_error("voxel"))?;
        let triangles = triangle_payload(hierarchy, &triangle_nodes)?;
        let voxel_triangles = voxel_payload(voxel)?;
        let mut measured = AppearanceError::default();
        // The modes swing the triangles and leave the aggregate still, so the transition is
        // measured at both ends of that swing: undisplaced (the shape difference alone) and
        // displaced by the saturated modal amplitude (the difference a distant plant shows
        // when the cut takes its motion away). The component-wise maximum is the bound.
        let extent = bounds_extent(node.bounds);
        let displacements = if modal.is_zero() {
            vec![0.0]
        } else {
            vec![0.0, modal.displacement(extent)]
        };
        for fixture in fixtures {
            let voxel_image = render_reference(
                &voxel_triangles,
                node.bounds,
                *fixture,
                resolution as usize,
                0.0,
            );
            for across in &displacements {
                let triangle_image = render_reference(
                    &triangles,
                    node.bounds,
                    *fixture,
                    resolution as usize,
                    *across,
                );
                measured = measured.max(measure_images(
                    &triangle_image,
                    &voxel_image,
                    node.bounds,
                    *fixture,
                    resolution as usize,
                ));
            }
        }
        comparisons.push(TriangleVoxelReferenceComparison {
            voxel_node: node.id,
            triangle_nodes,
            measured,
            declared: node.appearance_error,
            fixture_count: u32::try_from(fixtures.len()).map_err(|_| Error::NumericOverflow)?,
        });
    }
    Ok(comparisons)
}

#[derive(Clone, Copy)]
struct ReferenceTriangle {
    positions: [Vec3; 3],
    moments: PortableMaterialMoments,
}

#[derive(Clone, Copy)]
struct ReferenceSample {
    moments: PortableMaterialMoments,
}

fn finest_triangle_descendants(
    hierarchy: &PortableVirtualHierarchy,
    root: u32,
) -> Result<Vec<u32>> {
    let mut pending = hierarchy.nodes[root as usize].children.clone();
    let mut visited = BTreeSet::new();
    let mut triangles = Vec::new();
    while let Some(node_id) = pending.pop() {
        if !visited.insert(node_id) {
            return Err(reference_error("topology"));
        }
        let node = hierarchy
            .nodes
            .get(node_id as usize)
            .ok_or_else(|| reference_error("node"))?;
        if node.children.is_empty()
            && matches!(
                node.representation,
                HierarchyRepresentation::Triangles { .. }
            )
        {
            triangles.push(node_id);
        } else {
            pending.extend(node.children.iter().copied());
        }
    }
    triangles.sort_unstable();
    Ok(triangles)
}

fn triangle_payload(
    hierarchy: &PortableVirtualHierarchy,
    nodes: &[u32],
) -> Result<Vec<ReferenceTriangle>> {
    let mut output = Vec::new();
    for node_id in nodes {
        let node = &hierarchy.nodes[*node_id as usize];
        let HierarchyRepresentation::Triangles { first, count } = node.representation else {
            return Err(reference_error("triangleNode"));
        };
        let end = first.checked_add(count).ok_or(Error::NumericOverflow)?;
        for cluster in &hierarchy.triangle_clusters[first as usize..end as usize] {
            append_cluster_triangles(cluster, &mut output)?;
        }
    }
    Ok(output)
}

fn append_cluster_triangles(
    cluster: &PortableTriangleCluster,
    output: &mut Vec<ReferenceTriangle>,
) -> Result<()> {
    let vertices = cluster
        .vertices
        .iter()
        .map(|vertex| {
            Vec3::from_array(std::array::from_fn(|axis| {
                let minimum = cluster.bounds.min_bits[axis] as f32;
                let extent = cluster.bounds.max_bits[axis] as f32 - minimum;
                (minimum + extent * f32::from(vertex.position_unorm[axis]) / 65_535.0) / 65_536.0
            }))
        })
        .collect::<Vec<_>>();
    for indices in cluster.local_indices.chunks_exact(3) {
        output.push(ReferenceTriangle {
            positions: [
                *vertices
                    .get(indices[0] as usize)
                    .ok_or_else(|| reference_error("triangleIndex"))?,
                *vertices
                    .get(indices[1] as usize)
                    .ok_or_else(|| reference_error("triangleIndex"))?,
                *vertices
                    .get(indices[2] as usize)
                    .ok_or_else(|| reference_error("triangleIndex"))?,
            ],
            moments: cluster.material_moments,
        });
    }
    Ok(())
}

fn voxel_payload(voxel: &PortableVoxelBrick) -> Result<Vec<ReferenceTriangle>> {
    let vertices = voxel
        .vertices
        .iter()
        .map(|vertex| Vec3::from_array(vertex.position_bits.map(|value| value as f32 / 65_536.0)))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(voxel.indices.len() / 3);
    for indices in voxel.indices.chunks_exact(3) {
        output.push(ReferenceTriangle {
            positions: [
                *vertices
                    .get(indices[0] as usize)
                    .ok_or_else(|| reference_error("voxelIndex"))?,
                *vertices
                    .get(indices[1] as usize)
                    .ok_or_else(|| reference_error("voxelIndex"))?,
                *vertices
                    .get(indices[2] as usize)
                    .ok_or_else(|| reference_error("voxelIndex"))?,
            ],
            moments: voxel.moments,
        });
    }
    Ok(output)
}

/// Renders `triangles` under `fixture`, optionally displaced `across` local units along the
/// view's horizontal screen axis — the direction that moves a silhouette most, and so the
/// worst case for a mode the aggregate does not apply. The projection is parallel, so
/// displacing the geometry one way is the same as displacing every ray origin the other, and
/// the framing stays on `bounds`: geometry the displacement pushes out of frame reads as lost
/// silhouette, which is what it is.
fn render_reference(
    triangles: &[ReferenceTriangle],
    bounds: PortableBounds,
    fixture: HierarchyReferenceFixture,
    resolution: usize,
    across: f32,
) -> Vec<Option<ReferenceSample>> {
    let view = fixture.view_direction.normalize();
    let helper = if view.z.abs() < 0.9 { Vec3::Z } else { Vec3::Y };
    let right = view.cross(helper).normalize();
    let up = right.cross(view).normalize();
    let corners = bounds_corners(bounds);
    let projected_x = projected_extent(&corners, right);
    let projected_y = projected_extent(&corners, up);
    let projected_depth = projected_extent(&corners, view);
    let width = (projected_x.1 - projected_x.0).max(1.0 / 65_536.0);
    let height = (projected_y.1 - projected_y.0).max(1.0 / 65_536.0);
    let origin_depth = projected_depth.1 + (projected_depth.1 - projected_depth.0).max(1.0);
    let mut image = vec![None; resolution * resolution];
    for y in 0..resolution {
        for x in 0..resolution {
            let px = projected_x.0 + (x as f32 + 0.5) * width / resolution as f32;
            let py = projected_y.0 + (y as f32 + 0.5) * height / resolution as f32;
            let origin = right * (px - across) + up * py + view * origin_depth;
            let mut nearest = f32::INFINITY;
            let mut sample = None;
            for triangle in triangles {
                if let Some(distance) = ray_triangle(origin, -view, triangle.positions)
                    && distance < nearest
                {
                    nearest = distance;
                    sample = Some(ReferenceSample {
                        moments: triangle.moments,
                    });
                }
            }
            image[y * resolution + x] = sample;
        }
    }
    image
}

fn measure_images(
    triangle: &[Option<ReferenceSample>],
    voxel: &[Option<ReferenceSample>],
    bounds: PortableBounds,
    fixture: HierarchyReferenceFixture,
    resolution: usize,
) -> AppearanceError {
    let silhouette = silhouette_error(triangle, voxel, bounds, fixture, resolution);
    let mut coverage = 0_u128;
    let mut transmission = 0_u128;
    let mut material = 0_u128;
    let mut normal_distribution = 0_u128;
    let mut shared = 0_u128;
    let light = fixture.light_direction.normalize();
    for (triangle, voxel) in triangle.iter().zip(voxel) {
        let triangle_coverage = triangle.map_or(0, |sample| u32::from(sample.moments.occupancy));
        let voxel_coverage = voxel.map_or(0, |sample| u32::from(sample.moments.occupancy));
        coverage += u128::from(triangle_coverage.abs_diff(voxel_coverage));
        let (Some(triangle), Some(voxel)) = (triangle, voxel) else {
            continue;
        };
        shared += 1;
        transmission += u128::from(
            directional_transmission(triangle.moments, light)
                .abs_diff(directional_transmission(voxel.moments, light)),
        );
        material += u128::from(material_difference(triangle.moments, voxel.moments));
        normal_distribution += u128::from(normal_difference(triangle.moments, voxel.moments));
    }
    let pixels = triangle.len() as u128;
    AppearanceError::new(
        silhouette,
        average_error(coverage, pixels),
        average_error(transmission, shared.max(1)),
        average_error(material, shared.max(1)),
        average_error(normal_distribution, shared.max(1)),
    )
}

fn silhouette_error(
    first: &[Option<ReferenceSample>],
    second: &[Option<ReferenceSample>],
    bounds: PortableBounds,
    fixture: HierarchyReferenceFixture,
    resolution: usize,
) -> u32 {
    let first_hits = hit_coordinates(first, resolution);
    let second_hits = hit_coordinates(second, resolution);
    if first_hits == second_hits {
        return 0;
    }
    let view = fixture.view_direction.normalize();
    let helper = if view.z.abs() < 0.9 { Vec3::Z } else { Vec3::Y };
    let right = view.cross(helper).normalize();
    let up = right.cross(view).normalize();
    let corners = bounds_corners(bounds);
    let width = projected_extent(&corners, right);
    let height = projected_extent(&corners, up);
    let pixel_scale =
        ((width.1 - width.0).max(height.1 - height.0) / resolution as f32).max(1.0 / 65_536.0);
    let distance = directed_mask_distance(&first_hits, &second_hits)
        .max(directed_mask_distance(&second_hits, &first_hits));
    (distance * pixel_scale * 65_536.0)
        .ceil()
        .clamp(0.0, u32::MAX as f32) as u32
}

fn hit_coordinates(image: &[Option<ReferenceSample>], resolution: usize) -> Vec<[i32; 2]> {
    image
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| {
            sample.map(|_| [(index % resolution) as i32, (index / resolution) as i32])
        })
        .collect()
}

fn directed_mask_distance(from: &[[i32; 2]], to: &[[i32; 2]]) -> f32 {
    if from.is_empty() {
        return 0.0;
    }
    if to.is_empty() {
        return f32::MAX.sqrt();
    }
    from.iter()
        .map(|point| {
            to.iter()
                .map(|other| {
                    let x = (point[0] - other[0]) as f32;
                    let y = (point[1] - other[1]) as f32;
                    x.mul_add(x, y * y)
                })
                .fold(f32::INFINITY, f32::min)
                .sqrt()
        })
        .fold(0.0, f32::max)
}

fn directional_transmission(moments: PortableMaterialMoments, light: Vec3) -> u32 {
    let [x, y, z] = light.to_array();
    let m = moments
        .normal_second_moments
        .map(|value| value as f32 / 65_536.0);
    let directional = (m[0] * x * x
        + m[1] * y * y
        + m[2] * z * z
        + 2.0 * (m[3] * x * y + m[4] * x * z + m[5] * y * z))
        .max(0.0)
        .sqrt();
    moments
        .transmission_mean
        .iter()
        .map(|value| ((*value as f32).abs() * directional).round() as u32)
        .max()
        .unwrap_or_default()
}

fn material_difference(first: PortableMaterialMoments, second: PortableMaterialMoments) -> u32 {
    first
        .albedo_mean
        .into_iter()
        .zip(second.albedo_mean)
        .map(|(first, second)| first.abs_diff(second))
        .chain([u32::from(
            first.roughness_mean.abs_diff(second.roughness_mean),
        )])
        .max()
        .unwrap_or_default()
}

fn normal_difference(first: PortableMaterialMoments, second: PortableMaterialMoments) -> u32 {
    first
        .normal_second_moments
        .into_iter()
        .zip(second.normal_second_moments)
        .map(|(first, second)| first.abs_diff(second))
        .max()
        .unwrap_or_default()
}

fn average_error(total: u128, count: u128) -> u32 {
    u32::try_from((total + count / 2) / count).unwrap_or(u32::MAX)
}

/// The node's largest local-space side, the lever the branch mode swings over inside it.
fn bounds_extent(bounds: PortableBounds) -> f32 {
    (0..3)
        .map(|axis| (bounds.max_bits[axis] - bounds.min_bits[axis]) as f32 / 65_536.0)
        .fold(0.0_f32, f32::max)
}

fn bounds_corners(bounds: PortableBounds) -> [Vec3; 8] {
    std::array::from_fn(|corner| {
        Vec3::from_array(std::array::from_fn(|axis| {
            let value = if corner & (1 << axis) == 0 {
                bounds.min_bits[axis]
            } else {
                bounds.max_bits[axis]
            };
            value as f32 / 65_536.0
        }))
    })
}

fn projected_extent(points: &[Vec3], axis: Vec3) -> (f32, f32) {
    points.iter().fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), point| {
            let projection = point.dot(axis);
            (minimum.min(projection), maximum.max(projection))
        },
    )
}

fn ray_triangle(origin: Vec3, direction: Vec3, positions: [Vec3; 3]) -> Option<f32> {
    let edge_a = positions[1] - positions[0];
    let edge_b = positions[2] - positions[0];
    let cross = direction.cross(edge_b);
    let determinant = edge_a.dot(cross);
    if determinant.abs() <= 1.0e-8 {
        return None;
    }
    let inverse = determinant.recip();
    let offset = origin - positions[0];
    let u = offset.dot(cross) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = offset.cross(edge_a);
    let v = direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = edge_b.dot(q) * inverse;
    (distance >= 0.0).then_some(distance)
}

/// What one calibration pass changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoxelErrorCalibration {
    /// Voxel nodes whose declared error was widened to cover what a render actually showed.
    pub widened: Vec<u32>,
    /// Voxel nodes measured, widened or not.
    pub measured: u32,
}

/// Derives each aggregate-voxel node's declared appearance error from a measured render instead of
/// from the analytic estimate the cooker starts with.
///
/// The cooker's estimate is a function of the brick's bounds and material moments — cheap, and
/// necessarily a guess about how wrong the aggregate will *look*. The cut selector then trusts that
/// number to decide when a voxel brick may stand in for triangles. When the estimate is low, the
/// swap happens too early and pops on screen; the fix is not a fudge factor but a measurement.
///
/// This renders every transition device-free, takes the component-wise maximum across the fixtures,
/// and widens any declared error the measurement exceeds. It only ever widens: a measured error
/// below the estimate means the estimate was conservative, and narrowing to the measurement would
/// trust a finite fixture set to have found the worst view.
///
/// # Errors
///
/// Propagates the reference comparison's errors: an invalid hierarchy, an empty fixture set, or a
/// resolution outside `8..=512`.
pub fn calibrate_voxel_appearance_error(
    hierarchy: &mut PortableVirtualHierarchy,
    fixtures: &[HierarchyReferenceFixture],
    resolution: u32,
    modal: ModalAggregationBound,
) -> Result<VoxelErrorCalibration> {
    let comparisons = compare_triangle_voxel_transitions(hierarchy, fixtures, resolution, modal)?;
    let mut calibration = VoxelErrorCalibration {
        widened: Vec::new(),
        measured: u32::try_from(comparisons.len()).unwrap_or(u32::MAX),
    };
    for comparison in &comparisons {
        if comparison.is_within_declared_error() {
            continue;
        }
        let node = hierarchy
            .nodes
            .get_mut(comparison.voxel_node as usize)
            .ok_or_else(|| reference_error("node"))?;
        node.appearance_error = node.appearance_error.max(comparison.measured);
        calibration.widened.push(comparison.voxel_node);
    }
    Ok(calibration)
}

fn reference_error(field: &str) -> Error {
    Error::HierarchyFormat {
        format: "portable hierarchy reference",
        field: field.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mesh, PortableHierarchyInput, Submesh, Vertex, cook_portable_virtual_hierarchy};

    fn tetrahedron() -> Mesh {
        let positions = [
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(0.0, 1.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        Mesh {
            vertices: positions
                .into_iter()
                .map(|position| Vertex {
                    position,
                    normal: position.normalize(),
                    ..Vertex::default()
                })
                .collect(),
            indices: vec![0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 12,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    /// A comb of thin separated blades.
    ///
    /// The calibration fixture has to be geometry the analytic estimate is blind to, and thin
    /// features are exactly that: a voxel brick at its own resolution fills the gaps between the
    /// blades, so the aggregate reads as a slab while the triangles read as a comb. The estimate
    /// derives from bounds and occupancy and cannot see the difference.
    fn comb() -> Mesh {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for blade in 0..6_u32 {
            let x = -1.0 + blade as f32 * 0.34;
            let base = vertices.len() as u32;
            for corner in [
                Vec3::new(x, -1.0, -0.02),
                Vec3::new(x + 0.06, -1.0, -0.02),
                Vec3::new(x + 0.06, 1.0, 0.02),
                Vec3::new(x, 1.0, 0.02),
            ] {
                vertices.push(Vertex {
                    position: corner,
                    normal: Vec3::Z,
                    ..Vertex::default()
                });
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let index_count = indices.len() as u32;
        Mesh {
            vertices,
            indices,
            submeshes: vec![Submesh {
                first_index: 0,
                index_count,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    /// The cooked hierarchy the calibration tests measure and rewrite.
    fn fixture_hierarchy() -> PortableVirtualHierarchy {
        let input = PortableHierarchyInput::from_mesh(&comb(), &[]).unwrap();
        cook_portable_virtual_hierarchy(&input).unwrap()
    }

    #[test]
    fn the_modes_an_aggregate_drops_widen_its_declared_error() {
        // A distant plant keeps the whole-plant sway and loses the per-part branch mode and
        // leaf flutter, because an aggregate brick has no parts to swing. That loss is an
        // appearance difference the declared transition error has to cover, and the only way
        // to know it is covered is to measure the transition with the modes applied to the
        // triangle side and not to the aggregate — which is what the bound parameter does.
        let fixtures = canonical_hierarchy_reference_fixtures();
        let input = PortableHierarchyInput::from_mesh(&tetrahedron(), &[]).unwrap();
        let mut still = cook_portable_virtual_hierarchy(&input).unwrap();
        let mut moving = still.clone();
        calibrate_voxel_appearance_error(
            &mut still,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("still calibration");
        let modes = ModalAggregationBound {
            branch: 0.5,
            flutter: 0.2,
        };
        assert!(!modes.is_zero());
        calibrate_voxel_appearance_error(&mut moving, &fixtures, 32, modes)
            .expect("moving calibration");

        // Only ever wider: the modal pass adds measurements, it never removes one, so no node
        // may come out narrower than the still calibration left it.
        for (moving, still) in moving.nodes.iter().zip(&still.nodes) {
            assert!(moving.appearance_error.silhouette >= still.appearance_error.silhouette);
            assert!(moving.appearance_error.coverage >= still.appearance_error.coverage);
            assert!(moving.appearance_error.transmission >= still.appearance_error.transmission);
            assert!(moving.appearance_error.material >= still.appearance_error.material);
        }
        // And strictly wider somewhere, or the bound is inert and this proves nothing.
        assert!(
            moving
                .nodes
                .iter()
                .zip(&still.nodes)
                .any(|(moving, still)| moving.appearance_error != still.appearance_error),
            "the dropped modes changed no declared error"
        );
    }

    #[test]
    fn reference_renderer_measures_all_components_over_direction_fixtures() {
        let input = PortableHierarchyInput::from_mesh(&tetrahedron(), &[]).unwrap();
        let hierarchy = cook_portable_virtual_hierarchy(&input).unwrap();
        let fixtures = canonical_hierarchy_reference_fixtures();
        let comparisons = compare_triangle_voxel_transitions(
            &hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .unwrap();
        assert!(!comparisons.is_empty());
        assert!(comparisons.iter().all(|comparison| {
            comparison.fixture_count == fixtures.len() as u32
                && !comparison.triangle_nodes.is_empty()
                && comparison.is_within_declared_error()
        }));
        assert!(
            comparisons
                .iter()
                .any(|comparison| comparison.measured.silhouette > 0)
        );
    }

    #[test]
    fn calibration_makes_every_transition_fit_its_declared_error() {
        // The property the cut selector needs: after calibration, no voxel node claims to be a
        // better stand-in than a render says it is. Before it, the declared value is an analytic
        // guess, and a low guess swaps to the aggregate too early and pops.
        let mut hierarchy = fixture_hierarchy();
        let fixtures = canonical_hierarchy_reference_fixtures();
        let before = compare_triangle_voxel_transitions(
            &hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("the fixture measures");
        assert!(!before.is_empty(), "the fixture has transitions to measure");

        let calibration = calibrate_voxel_appearance_error(
            &mut hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("calibration runs");
        assert_eq!(calibration.measured as usize, before.len());
        // Without this the assertion below would pass on a hierarchy whose analytic estimate
        // already covered every measurement, proving nothing about the calibration.
        assert!(
            !calibration.widened.is_empty(),
            "the fixture must actually need widening"
        );

        let after = compare_triangle_voxel_transitions(
            &hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("the calibrated fixture measures");
        for comparison in &after {
            assert!(
                comparison.is_within_declared_error(),
                "node {} still exceeds its declared error",
                comparison.voxel_node
            );
        }
    }

    #[test]
    fn calibration_only_widens() {
        // A measured error below the estimate means the estimate was conservative. Narrowing to
        // the measurement would trust a finite fixture set to have found the worst view, which is
        // exactly the assumption a measured calibration exists to avoid.
        let mut hierarchy = fixture_hierarchy();
        let declared: Vec<AppearanceError> = hierarchy
            .nodes
            .iter()
            .map(|node| node.appearance_error)
            .collect();
        calibrate_voxel_appearance_error(
            &mut hierarchy,
            &canonical_hierarchy_reference_fixtures(),
            32,
            ModalAggregationBound::default(),
        )
        .expect("calibration runs");
        for (node, before) in hierarchy.nodes.iter().zip(&declared) {
            assert!(node.appearance_error.silhouette >= before.silhouette);
            assert!(node.appearance_error.coverage >= before.coverage);
            assert!(node.appearance_error.transmission >= before.transmission);
            assert!(node.appearance_error.material >= before.material);
            assert!(node.appearance_error.normal_distribution >= before.normal_distribution);
        }
    }

    #[test]
    fn calibration_is_idempotent() {
        // A second pass has nothing left to widen, which is what "the declared value now covers
        // the measurement" means operationally — and what a cooker re-running it must rely on.
        let mut hierarchy = fixture_hierarchy();
        let fixtures = canonical_hierarchy_reference_fixtures();
        calibrate_voxel_appearance_error(
            &mut hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("first pass");
        let second = calibrate_voxel_appearance_error(
            &mut hierarchy,
            &fixtures,
            32,
            ModalAggregationBound::default(),
        )
        .expect("second pass");
        assert!(second.widened.is_empty());
    }
}
