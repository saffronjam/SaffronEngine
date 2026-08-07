//! Aggregate voxel brick construction and its portable indexed surface.

use crate::Result;

use super::support::{format_error, multiply_unit, oct_encode, u32_len};
use super::types::{
    AppearanceError, PORTABLE_VOXEL_BRICK_EDGE, PortableBounds, PortableMaterialMoments,
    PortableSourceMesh, PortableVoxelBrick, PortableVoxelVertex, VirtualHierarchyMaterial,
};

pub(crate) fn build_voxel_brick(
    id: u32,
    bounds: PortableBounds,
    mesh: &PortableSourceMesh,
    indices: &[u32],
    material: VirtualHierarchyMaterial,
    padding: i32,
) -> Result<PortableVoxelBrick> {
    let edge = usize::from(PORTABLE_VOXEL_BRICK_EDGE);
    let voxel_count = edge * edge * edge;
    let mut occupancy = vec![0_u8; voxel_count.div_ceil(8)];
    for triangle in indices.chunks_exact(3) {
        let coordinates = triangle
            .iter()
            .map(|index| {
                mesh.vertices
                    .get(*index as usize)
                    .map(|vertex| voxel_coordinate(vertex.position_bits, bounds, edge))
                    .ok_or_else(|| format_error("portable hierarchy", "voxel.vertex"))
            })
            .collect::<Result<Vec<_>>>()?;
        let minimum: [usize; 3] = std::array::from_fn(|axis| {
            coordinates
                .iter()
                .map(|coordinate| coordinate[axis])
                .min()
                .unwrap_or(0)
        });
        let maximum: [usize; 3] = std::array::from_fn(|axis| {
            coordinates
                .iter()
                .map(|coordinate| coordinate[axis])
                .max()
                .unwrap_or(0)
        });
        for z in minimum[2]..=maximum[2] {
            for y in minimum[1]..=maximum[1] {
                for x in minimum[0]..=maximum[0] {
                    mark_voxel(&mut occupancy, [x, y, z], edge);
                }
            }
        }
    }
    dilate_occupancy(&mut occupancy, edge);
    let (vertices, surface_indices) = voxel_surface(bounds, &occupancy, edge)?;
    let occupied = occupancy.iter().map(|byte| byte.count_ones()).sum::<u32>();
    let density = ((u64::from(occupied) * u64::from(u16::MAX) + voxel_count as u64 / 2)
        / voxel_count as u64) as u16;
    let mut aggregate = material.moments;
    aggregate.occupancy = multiply_unit(aggregate.occupancy, density);
    let error = voxel_appearance_error(bounds, aggregate, PORTABLE_VOXEL_BRICK_EDGE);
    Ok(PortableVoxelBrick {
        id,
        dimensions: [PORTABLE_VOXEL_BRICK_EDGE; 3],
        bounds,
        deformed_bounds: bounds.expanded(padding),
        occupancy,
        moments: aggregate,
        material_class: material.class,
        opacity_micromap: material.opacity_micromap,
        vertices,
        indices: surface_indices,
        page: u32::MAX,
        appearance_error: error,
    })
}

pub(crate) fn build_coarse_root_brick(
    id: u32,
    bounds: PortableBounds,
    material: VirtualHierarchyMaterial,
    padding: i32,
) -> Result<PortableVoxelBrick> {
    let edge = usize::from(PORTABLE_VOXEL_BRICK_EDGE);
    let mut occupancy = vec![0_u8; (edge * edge * edge).div_ceil(8)];
    occupancy.fill(u8::MAX);
    let (vertices, indices) = voxel_surface(bounds, &occupancy, edge)?;
    let aggregate = material.moments;
    let maximum_extent = (0..3)
        .map(|axis| {
            i64::from(bounds.max_bits[axis])
                .saturating_sub(i64::from(bounds.min_bits[axis]))
                .unsigned_abs()
        })
        .max()
        .unwrap_or_default();
    let transmission = material
        .moments
        .transmission_mean
        .iter()
        .map(|value| value.unsigned_abs())
        .max()
        .unwrap_or_default();
    let material_error = material
        .moments
        .albedo_mean
        .iter()
        .map(|value| value.unsigned_abs())
        .chain([u32::from(material.moments.roughness_mean)])
        .max()
        .unwrap_or_default();
    Ok(PortableVoxelBrick {
        id,
        dimensions: [PORTABLE_VOXEL_BRICK_EDGE; 3],
        bounds,
        deformed_bounds: bounds.expanded(padding),
        occupancy,
        moments: aggregate,
        material_class: material.class,
        opacity_micromap: material.opacity_micromap,
        vertices,
        indices,
        page: u32::MAX,
        appearance_error: AppearanceError::new(
            u32::try_from(maximum_extent).unwrap_or(u32::MAX),
            u32::from(u16::MAX),
            transmission,
            material_error,
            65_536,
        ),
    })
}

fn voxel_coordinate(position: [i32; 3], bounds: PortableBounds, edge: usize) -> [usize; 3] {
    std::array::from_fn(|axis| {
        let extent = i64::from(bounds.max_bits[axis]) - i64::from(bounds.min_bits[axis]);
        if extent <= 0 {
            return 0;
        }
        let offset = i64::from(position[axis]) - i64::from(bounds.min_bits[axis]);
        usize::try_from((offset.clamp(0, extent) * (edge as i64 - 1)) / extent).unwrap_or(0)
    })
}

fn mark_voxel(occupancy: &mut [u8], coordinate: [usize; 3], edge: usize) {
    let index = coordinate[0] + edge * (coordinate[1] + edge * coordinate[2]);
    occupancy[index / 8] |= 1 << (index % 8);
}

fn voxel_occupied(occupancy: &[u8], coordinate: [usize; 3], edge: usize) -> bool {
    let index = coordinate[0] + edge * (coordinate[1] + edge * coordinate[2]);
    occupancy[index / 8] & (1 << (index % 8)) != 0
}

fn dilate_occupancy(occupancy: &mut [u8], edge: usize) {
    let source = occupancy.to_vec();
    for z in 0..edge {
        for y in 0..edge {
            for x in 0..edge {
                if !voxel_occupied(&source, [x, y, z], edge) {
                    continue;
                }
                for [dx, dy, dz] in [
                    [-1_i32, 0, 0],
                    [1, 0, 0],
                    [0, -1, 0],
                    [0, 1, 0],
                    [0, 0, -1],
                    [0, 0, 1],
                ] {
                    let next = [x as i32 + dx, y as i32 + dy, z as i32 + dz];
                    if next.iter().all(|value| (0..edge as i32).contains(value)) {
                        mark_voxel(
                            occupancy,
                            [next[0] as usize, next[1] as usize, next[2] as usize],
                            edge,
                        );
                    }
                }
            }
        }
    }
}

fn voxel_surface(
    bounds: PortableBounds,
    occupancy: &[u8],
    edge: usize,
) -> Result<(Vec<PortableVoxelVertex>, Vec<u32>)> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for z in 0..edge {
        for y in 0..edge {
            for x in 0..edge {
                if !voxel_occupied(occupancy, [x, y, z], edge) {
                    continue;
                }
                for (axis, direction) in
                    [(0_usize, -1_i32), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)]
                {
                    let mut neighbor = [x as i32, y as i32, z as i32];
                    neighbor[axis] += direction;
                    let exposed = neighbor
                        .iter()
                        .any(|coordinate| !(0..edge as i32).contains(coordinate))
                        || !voxel_occupied(
                            occupancy,
                            [
                                neighbor[0].clamp(0, edge as i32 - 1) as usize,
                                neighbor[1].clamp(0, edge as i32 - 1) as usize,
                                neighbor[2].clamp(0, edge as i32 - 1) as usize,
                            ],
                            edge,
                        );
                    if exposed {
                        append_voxel_face(
                            &mut vertices,
                            &mut indices,
                            bounds,
                            [x, y, z],
                            edge,
                            axis,
                            direction,
                        )?;
                    }
                }
            }
        }
    }
    if indices.is_empty() {
        return Ok(box_surface(bounds));
    }
    Ok((vertices, indices))
}

fn append_voxel_face(
    vertices: &mut Vec<PortableVoxelVertex>,
    indices: &mut Vec<u32>,
    bounds: PortableBounds,
    voxel: [usize; 3],
    edge: usize,
    axis: usize,
    direction: i32,
) -> Result<()> {
    let tangent_axes = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let plane = voxel[axis] + usize::from(direction > 0);
    let corners = [[0_usize, 0_usize], [1, 0], [1, 1], [0, 1]];
    let first = u32_len(vertices.len())?;
    let mut normal = [0.0_f32; 3];
    normal[axis] = direction as f32;
    for corner in corners {
        let mut grid = voxel;
        grid[axis] = plane;
        grid[tangent_axes[0]] += corner[0];
        grid[tangent_axes[1]] += corner[1];
        let position_bits = std::array::from_fn(|component| {
            let min = i64::from(bounds.min_bits[component]);
            let extent = i64::from(bounds.max_bits[component]) - min;
            let numerator = i64::try_from(grid[component]).unwrap_or(0);
            (min + (extent * numerator + edge as i64 / 2) / edge as i64) as i32
        });
        vertices.push(PortableVoxelVertex {
            position_bits,
            normal_oct: oct_encode(normal),
        });
    }
    if direction > 0 {
        indices.extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
    } else {
        indices.extend_from_slice(&[first, first + 2, first + 1, first, first + 3, first + 2]);
    }
    Ok(())
}

fn box_surface(bounds: PortableBounds) -> (Vec<PortableVoxelVertex>, Vec<u32>) {
    let positions = [
        [0, 0, 0],
        [1, 0, 0],
        [1, 1, 0],
        [0, 1, 0],
        [0, 0, 1],
        [1, 0, 1],
        [1, 1, 1],
        [0, 1, 1],
    ];
    let vertices = positions
        .into_iter()
        .map(|corner| PortableVoxelVertex {
            position_bits: std::array::from_fn(|axis| {
                if corner[axis] == 0 {
                    bounds.min_bits[axis]
                } else {
                    bounds.max_bits[axis]
                }
            }),
            normal_oct: [0, 0],
        })
        .collect();
    let indices = vec![
        0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3,
        1, 2, 6, 1, 6, 5,
    ];
    (vertices, indices)
}

fn voxel_appearance_error(
    bounds: PortableBounds,
    moments: PortableMaterialMoments,
    edge: u8,
) -> AppearanceError {
    let maximum_extent = (0..3)
        .map(|axis| {
            i64::from(bounds.max_bits[axis])
                .saturating_sub(i64::from(bounds.min_bits[axis]))
                .unsigned_abs()
        })
        .max()
        .unwrap_or_default();
    let silhouette = u32::try_from(maximum_extent / u64::from(edge)).unwrap_or(u32::MAX);
    let coverage = u32::from(u16::MAX.saturating_sub(moments.occupancy));
    let transmission = moments
        .transmission_mean
        .iter()
        .map(|value| value.unsigned_abs())
        .max()
        .unwrap_or_default();
    let material = u32::from(moments.roughness_mean) / u32::from(edge);
    let normal_distribution = moments
        .normal_second_moments
        .iter()
        .map(|value| value.unsigned_abs())
        .max()
        .unwrap_or_default()
        / u32::from(edge);
    AppearanceError::new(
        silhouette,
        coverage,
        transmission,
        material,
        normal_distribution,
    )
}
