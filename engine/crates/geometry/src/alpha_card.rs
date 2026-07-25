//! Deterministic geometry extraction for texture-masked planar cards.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use glam::{Vec2, Vec3};

use crate::{Error, Mesh, Result, Submesh, Vertex};

/// Maximum raster extent retained by the macro-silhouette contour.
pub const ALPHA_CARD_CONTOUR_MAX_EXTENT: u32 = 128;

/// One generated card vertex and its interpolation back to the source triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContouredCardVertex {
    /// Render vertex interpolated on the source card.
    pub vertex: Vertex,
    /// Source-mesh vertex indices of the containing triangle.
    pub source_triangle: [u32; 3],
    /// Barycentric weights within `source_triangle`.
    pub barycentric: [f32; 3],
}

/// A material-homogeneous, geometry-first replacement for one alpha card.
#[derive(Clone, Debug, PartialEq)]
pub struct ContouredAlphaCard {
    /// Generated vertices with source interpolation data.
    pub vertices: Vec<ContouredCardVertex>,
    /// Counter-clockwise triangle indices into `vertices`.
    pub indices: Vec<u32>,
    /// Source material slot retained by the replacement geometry.
    pub material_slot: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GridPoint {
    x: u32,
    y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GridEdge {
    from: GridPoint,
    to: GridPoint,
}

/// Converts the occupied macro silhouette of one planar alpha-card submesh to triangles.
///
/// The contour uses at most a 128×128 deterministic max-reduction of the source alpha. The mesh
/// therefore owns the visible outer silhouette while the original alpha remains available for
/// holes and detail below the contour grid. `None` means the source is not a safe planar card.
pub fn contour_alpha_card(
    mesh: &Mesh,
    submesh_index: usize,
    alpha: &[u8],
    width: u32,
    height: u32,
    cutoff: u8,
) -> Result<Option<ContouredAlphaCard>> {
    let Some(submesh) = mesh.submeshes.get(submesh_index).copied() else {
        return Err(Error::Import("alpha-card submesh is missing".to_owned()));
    };
    if width == 0
        || height == 0
        || alpha.len() != width as usize * height as usize
        || submesh.index_count == 0
        || !submesh.index_count.is_multiple_of(3)
    {
        return Err(Error::Import(
            "alpha-card coverage or topology is invalid".to_owned(),
        ));
    }
    let triangles = submesh_triangles(mesh, submesh)?;
    if triangles.is_empty() || !is_planar(mesh, &triangles) {
        return Ok(None);
    }

    let grid_width = width.min(ALPHA_CARD_CONTOUR_MAX_EXTENT);
    let grid_height = height.min(ALPHA_CARD_CONTOUR_MAX_EXTENT);
    let mut occupied = vec![false; grid_width as usize * grid_height as usize];
    for y in 0..grid_height {
        for x in 0..grid_width {
            let uv = Vec2::new(
                (x as f32 + 0.5) / grid_width as f32,
                (y as f32 + 0.5) / grid_height as f32,
            );
            if source_triangle_at_uv(mesh, &triangles, uv).is_none() {
                continue;
            }
            let source_x_begin = (u64::from(x) * u64::from(width) / u64::from(grid_width)) as u32;
            let source_x_end =
                (u64::from(x + 1) * u64::from(width)).div_ceil(u64::from(grid_width)) as u32;
            let source_y_begin = (u64::from(y) * u64::from(height) / u64::from(grid_height)) as u32;
            let source_y_end =
                (u64::from(y + 1) * u64::from(height)).div_ceil(u64::from(grid_height)) as u32;
            occupied[y as usize * grid_width as usize + x as usize] =
                (source_y_begin..source_y_end.min(height)).any(|source_y| {
                    (source_x_begin..source_x_end.min(width)).any(|source_x| {
                        alpha[source_y as usize * width as usize + source_x as usize] >= cutoff
                    })
                });
        }
    }
    let components = occupied_components(&occupied, grid_width, grid_height);
    if components.is_empty() {
        return Err(Error::Import(
            "alpha-card coverage contains no occupied texels".to_owned(),
        ));
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for component in components {
        let Some(polygon) = outer_boundary(&component, grid_width, grid_height) else {
            continue;
        };
        let polygon_uv = polygon
            .iter()
            .map(|point| {
                Vec2::new(
                    point.x as f32 / grid_width as f32,
                    point.y as f32 / grid_height as f32,
                )
            })
            .collect::<Vec<_>>();
        let polygon_indices = triangulate_polygon(&polygon_uv)?;
        let base = u32::try_from(vertices.len()).map_err(|_| Error::NumericOverflow)?;
        let mut mapped = Vec::with_capacity(polygon_uv.len());
        for uv in polygon_uv {
            let Some((source_triangle, barycentric)) = source_triangle_at_uv(mesh, &triangles, uv)
            else {
                mapped.clear();
                break;
            };
            mapped.push(ContouredCardVertex {
                vertex: interpolate_vertex(mesh, source_triangle, barycentric, uv),
                source_triangle,
                barycentric,
            });
        }
        if mapped.len() != polygon.len() {
            continue;
        }
        vertices.extend(mapped);
        indices.extend(
            polygon_indices
                .into_iter()
                .map(|index| base.checked_add(index).ok_or(Error::NumericOverflow))
                .collect::<Result<Vec<_>>>()?,
        );
    }
    if vertices.len() < 3 || indices.is_empty() {
        return Ok(None);
    }
    Ok(Some(ContouredAlphaCard {
        vertices,
        indices,
        material_slot: submesh.material_slot,
    }))
}

fn submesh_triangles(mesh: &Mesh, submesh: Submesh) -> Result<Vec<[u32; 3]>> {
    let begin = submesh.first_index as usize;
    let end = begin
        .checked_add(submesh.index_count as usize)
        .ok_or(Error::NumericOverflow)?;
    let source = mesh.indices.get(begin..end).ok_or_else(|| {
        Error::Import("alpha-card submesh range exceeds the index stream".to_owned())
    })?;
    source
        .chunks_exact(3)
        .map(|triangle| {
            let mut addressed = [0_u32; 3];
            for (output, index) in addressed.iter_mut().zip(triangle) {
                let value = i64::from(*index) + i64::from(submesh.vertex_offset);
                *output = u32::try_from(value).map_err(|_| {
                    Error::Import("alpha-card vertex offset is out of range".to_owned())
                })?;
                if *output as usize >= mesh.vertices.len() {
                    return Err(Error::Import(
                        "alpha-card index references a missing vertex".to_owned(),
                    ));
                }
            }
            Ok(addressed)
        })
        .collect()
}

fn is_planar(mesh: &Mesh, triangles: &[[u32; 3]]) -> bool {
    let first = triangles[0];
    let origin = mesh.vertices[first[0] as usize].position;
    let normal = (mesh.vertices[first[1] as usize].position - origin)
        .cross(mesh.vertices[first[2] as usize].position - origin)
        .normalize_or_zero();
    if normal.length_squared() < 0.5 {
        return false;
    }
    let mut extent = 0.0_f32;
    let mut distance = 0.0_f32;
    for triangle in triangles {
        for index in triangle {
            let delta = mesh.vertices[*index as usize].position - origin;
            extent = extent.max(delta.length());
            distance = distance.max(delta.dot(normal).abs());
        }
    }
    distance <= extent.max(1.0) * 1.0e-5
}

fn source_triangle_at_uv(
    mesh: &Mesh,
    triangles: &[[u32; 3]],
    uv: Vec2,
) -> Option<([u32; 3], [f32; 3])> {
    for &triangle in triangles {
        let points = triangle.map(|index| mesh.vertices[index as usize].uv0);
        let first = points[1] - points[0];
        let second = points[2] - points[0];
        let relative = uv - points[0];
        let determinant =
            f64::from(first.x) * f64::from(second.y) - f64::from(first.y) * f64::from(second.x);
        if determinant.abs() <= 1.0e-12 {
            continue;
        }
        let b = (f64::from(relative.x) * f64::from(second.y)
            - f64::from(relative.y) * f64::from(second.x))
            / determinant;
        let c = (f64::from(first.x) * f64::from(relative.y)
            - f64::from(first.y) * f64::from(relative.x))
            / determinant;
        let a = 1.0 - b - c;
        if a >= -1.0e-5 && b >= -1.0e-5 && c >= -1.0e-5 {
            let mut weights = [a.max(0.0) as f32, b.max(0.0) as f32, c.max(0.0) as f32];
            let total = weights.iter().sum::<f32>();
            if total > 0.0 {
                weights.iter_mut().for_each(|weight| *weight /= total);
                return Some((triangle, weights));
            }
        }
    }
    None
}

fn interpolate_vertex(mesh: &Mesh, triangle: [u32; 3], barycentric: [f32; 3], uv: Vec2) -> Vertex {
    let source = triangle.map(|index| mesh.vertices[index as usize]);
    let weighted_vec3 = |field: fn(Vertex) -> Vec3| {
        field(source[0]) * barycentric[0]
            + field(source[1]) * barycentric[1]
            + field(source[2]) * barycentric[2]
    };
    let normal = weighted_vec3(|vertex| vertex.normal).normalize_or_zero();
    let tangent = weighted_vec3(|vertex| {
        Vec3::from_array([vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]])
    });
    let tangent = (tangent - normal * normal.dot(tangent)).normalize_or_zero();
    let handedness = source
        .iter()
        .zip(barycentric)
        .map(|(vertex, weight)| vertex.tangent[3] * weight)
        .sum::<f32>()
        .signum();
    Vertex {
        position: weighted_vec3(|vertex| vertex.position),
        normal,
        uv0: uv,
        tangent: [tangent.x, tangent.y, tangent.z, handedness],
    }
}

fn occupied_components(mask: &[bool], width: u32, height: u32) -> Vec<BTreeSet<(u32, u32)>> {
    let mut seen = vec![false; mask.len()];
    let mut components = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let index = y as usize * width as usize + x as usize;
            if !mask[index] || seen[index] {
                continue;
            }
            seen[index] = true;
            let mut queue = VecDeque::from([(x, y)]);
            let mut component = BTreeSet::new();
            while let Some((cell_x, cell_y)) = queue.pop_front() {
                component.insert((cell_x, cell_y));
                for (next_x, next_y) in neighbours(cell_x, cell_y, width, height) {
                    let next = next_y as usize * width as usize + next_x as usize;
                    if mask[next] && !seen[next] {
                        seen[next] = true;
                        queue.push_back((next_x, next_y));
                    }
                }
            }
            components.push(component);
        }
    }
    components
}

fn neighbours(x: u32, y: u32, width: u32, height: u32) -> impl Iterator<Item = (u32, u32)> {
    [
        x.checked_sub(1).map(|next| (next, y)),
        (x + 1 < width).then_some((x + 1, y)),
        y.checked_sub(1).map(|next| (x, next)),
        (y + 1 < height).then_some((x, y + 1)),
    ]
    .into_iter()
    .flatten()
}

fn outer_boundary(
    component: &BTreeSet<(u32, u32)>,
    width: u32,
    height: u32,
) -> Option<Vec<GridPoint>> {
    let mut outgoing = BTreeMap::<GridPoint, Vec<GridPoint>>::new();
    let contains = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && x < i64::from(width)
            && y < i64::from(height)
            && component.contains(&(x as u32, y as u32))
    };
    for &(x, y) in component {
        let x = i64::from(x);
        let y = i64::from(y);
        let point = |x: i64, y: i64| GridPoint {
            x: x as u32,
            y: y as u32,
        };
        let edges = [
            (!contains(x, y - 1)).then_some(GridEdge {
                from: point(x, y),
                to: point(x + 1, y),
            }),
            (!contains(x + 1, y)).then_some(GridEdge {
                from: point(x + 1, y),
                to: point(x + 1, y + 1),
            }),
            (!contains(x, y + 1)).then_some(GridEdge {
                from: point(x + 1, y + 1),
                to: point(x, y + 1),
            }),
            (!contains(x - 1, y)).then_some(GridEdge {
                from: point(x, y + 1),
                to: point(x, y),
            }),
        ];
        for edge in edges.into_iter().flatten() {
            outgoing.entry(edge.from).or_default().push(edge.to);
        }
    }
    outgoing.values_mut().for_each(|targets| targets.sort());
    let mut unused = outgoing
        .iter()
        .flat_map(|(from, targets)| {
            targets
                .iter()
                .map(|to| GridEdge {
                    from: *from,
                    to: *to,
                })
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    let mut loops = Vec::new();
    while let Some(first) = unused.first().copied() {
        let mut polygon = vec![first.from];
        let mut edge = first;
        unused.remove(&edge);
        while edge.to != first.from {
            polygon.push(edge.to);
            let candidates = outgoing.get(&edge.to)?;
            let next = candidates
                .iter()
                .copied()
                .filter_map(|target| {
                    let candidate = GridEdge {
                        from: edge.to,
                        to: target,
                    };
                    unused
                        .contains(&candidate)
                        .then_some((turn_priority(edge, candidate), candidate))
                })
                .min_by_key(|candidate| candidate.0)
                .map(|candidate| candidate.1)?;
            unused.remove(&next);
            edge = next;
            if polygon.len() > component.len().saturating_mul(4).saturating_add(4) {
                return None;
            }
        }
        remove_collinear(&mut polygon);
        if polygon.len() >= 3 {
            loops.push(polygon);
        }
    }
    loops
        .into_iter()
        .max_by_key(|polygon| polygon_area2(polygon).unsigned_abs())
}

fn direction(edge: GridEdge) -> u8 {
    match (
        i64::from(edge.to.x) - i64::from(edge.from.x),
        i64::from(edge.to.y) - i64::from(edge.from.y),
    ) {
        (1, 0) => 0,
        (0, 1) => 1,
        (-1, 0) => 2,
        (0, -1) => 3,
        _ => 4,
    }
}

fn turn_priority(previous: GridEdge, next: GridEdge) -> u8 {
    match (direction(next) + 4 - direction(previous)) % 4 {
        1 => 0,
        0 => 1,
        3 => 2,
        _ => 3,
    }
}

fn remove_collinear(polygon: &mut Vec<GridPoint>) {
    loop {
        let mut removed = false;
        for index in 0..polygon.len() {
            let previous = polygon[(index + polygon.len() - 1) % polygon.len()];
            let current = polygon[index];
            let next = polygon[(index + 1) % polygon.len()];
            if cross(previous, current, next) == 0 {
                polygon.remove(index);
                removed = true;
                break;
            }
        }
        if !removed || polygon.len() < 3 {
            break;
        }
    }
}

fn polygon_area2(polygon: &[GridPoint]) -> i64 {
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .map(|(first, second)| {
            i64::from(first.x) * i64::from(second.y) - i64::from(second.x) * i64::from(first.y)
        })
        .sum()
}

fn cross(first: GridPoint, middle: GridPoint, last: GridPoint) -> i64 {
    (i64::from(middle.x) - i64::from(first.x)) * (i64::from(last.y) - i64::from(middle.y))
        - (i64::from(middle.y) - i64::from(first.y)) * (i64::from(last.x) - i64::from(middle.x))
}

fn triangulate_polygon(polygon: &[Vec2]) -> Result<Vec<u32>> {
    if polygon.len() < 3 {
        return Ok(Vec::new());
    }
    let area = polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .map(|(first, second)| first.x * second.y - second.x * first.y)
        .sum::<f32>();
    if area.abs() <= 1.0e-12 {
        return Ok(Vec::new());
    }
    let orientation = area.signum();
    let mut remaining = (0..polygon.len()).collect::<Vec<_>>();
    let mut triangles = Vec::with_capacity((polygon.len() - 2) * 3);
    while remaining.len() > 3 {
        let mut ear = None;
        for index in 0..remaining.len() {
            let previous = remaining[(index + remaining.len() - 1) % remaining.len()];
            let current = remaining[index];
            let next = remaining[(index + 1) % remaining.len()];
            if triangle_cross(polygon[previous], polygon[current], polygon[next]) * orientation
                <= 1.0e-9
            {
                continue;
            }
            if remaining.iter().copied().any(|candidate| {
                candidate != previous
                    && candidate != current
                    && candidate != next
                    && point_in_triangle(
                        polygon[candidate],
                        polygon[previous],
                        polygon[current],
                        polygon[next],
                        orientation,
                    )
            }) {
                continue;
            }
            ear = Some((index, [previous, current, next]));
            break;
        }
        let Some((index, triangle)) = ear else {
            return Err(Error::Import(
                "alpha-card contour is not a simple polygon".to_owned(),
            ));
        };
        triangles.extend(triangle.map(|vertex| vertex as u32));
        remaining.remove(index);
    }
    triangles.extend(remaining.into_iter().map(|vertex| vertex as u32));
    if orientation < 0.0 {
        for triangle in triangles.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    Ok(triangles)
}

fn triangle_cross(first: Vec2, middle: Vec2, last: Vec2) -> f32 {
    (middle - first).perp_dot(last - middle)
}

fn point_in_triangle(
    point: Vec2,
    first: Vec2,
    second: Vec2,
    third: Vec2,
    orientation: f32,
) -> bool {
    [
        triangle_cross(first, second, point),
        triangle_cross(second, third, point),
        triangle_cross(third, first, point),
    ]
    .into_iter()
    .all(|value| value * orientation >= -1.0e-9)
}

#[cfg(test)]
mod tests {
    use glam::{Vec2, Vec3};

    use super::*;

    fn card() -> Mesh {
        Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 0.0),
                    tangent: [1.0, 0.0, 0.0, 1.0],
                },
                Vertex {
                    position: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    tangent: [1.0, 0.0, 0.0, 1.0],
                },
                Vertex {
                    position: Vec3::new(1.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 1.0),
                    tangent: [1.0, 0.0, 0.0, 1.0],
                },
                Vertex {
                    position: Vec3::new(-1.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
                    tangent: [1.0, 0.0, 0.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 7,
            }],
        }
    }

    #[test]
    fn macro_contour_replaces_the_card_silhouette_and_keeps_source_mapping() {
        let mut alpha = vec![0_u8; 8 * 8];
        for y in 1..7 {
            let half_width = if y == 1 || y == 6 { 1 } else { 2 };
            for x in 4 - half_width..=4 + half_width {
                alpha[y * 8 + x] = 255;
            }
        }
        let output = contour_alpha_card(&card(), 0, &alpha, 8, 8, 128)
            .expect("contour")
            .expect("planar card");
        assert_eq!(output.material_slot, 7);
        assert!(output.indices.len() >= 6);
        assert!(output.vertices.iter().all(|vertex| {
            (vertex.barycentric.iter().sum::<f32>() - 1.0).abs() < 1.0e-5
                && vertex.vertex.position.z == 0.0
        }));
        let bounds = output.vertices.iter().fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(minimum, maximum), vertex| {
                (
                    minimum.min(vertex.vertex.uv0),
                    maximum.max(vertex.vertex.uv0),
                )
            },
        );
        assert!(bounds.0.x > 0.0 && bounds.1.x < 1.0);
        assert!(bounds.0.y > 0.0 && bounds.1.y < 1.0);
    }

    #[test]
    fn interior_holes_remain_residual_alpha_instead_of_extra_geometry_boundaries() {
        let mut alpha = vec![255_u8; 8 * 8];
        for y in 3..5 {
            for x in 3..5 {
                alpha[y * 8 + x] = 0;
            }
        }
        let output = contour_alpha_card(&card(), 0, &alpha, 8, 8, 128)
            .expect("contour")
            .expect("planar card");
        assert_eq!(output.vertices.len(), 4);
        assert_eq!(output.indices.len(), 6);
    }

    #[test]
    fn nonplanar_mesh_is_not_rewritten_as_a_card() {
        let mut mesh = card();
        mesh.vertices[2].position.z = 0.25;
        assert!(
            contour_alpha_card(&mesh, 0, &[255; 64], 8, 8, 128)
                .expect("classification")
                .is_none()
        );
    }
}
