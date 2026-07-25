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
        for fixture in fixtures {
            let triangle_image =
                render_reference(&triangles, node.bounds, *fixture, resolution as usize);
            let voxel_image =
                render_reference(&voxel_triangles, node.bounds, *fixture, resolution as usize);
            measured = measured.max(measure_images(
                &triangle_image,
                &voxel_image,
                node.bounds,
                *fixture,
                resolution as usize,
            ));
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

fn render_reference(
    triangles: &[ReferenceTriangle],
    bounds: PortableBounds,
    fixture: HierarchyReferenceFixture,
    resolution: usize,
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
            let origin = right * px + up * py + view * origin_depth;
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

    #[test]
    fn reference_renderer_measures_all_components_over_direction_fixtures() {
        let input = PortableHierarchyInput::from_mesh(&tetrahedron(), &[]).unwrap();
        let hierarchy = cook_portable_virtual_hierarchy(&input).unwrap();
        let fixtures = canonical_hierarchy_reference_fixtures();
        let comparisons = compare_triangle_voxel_transitions(&hierarchy, &fixtures, 32).unwrap();
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
}
