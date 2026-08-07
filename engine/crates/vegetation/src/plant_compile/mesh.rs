//! Coordinate, attribute, and skeleton normalization of one selected source mesh.

use std::collections::{BTreeMap, BTreeSet};

use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, VertexSkin, compute_tangents};
use saffron_spatial::DecisionScalar;

use crate::{
    PlantFamilyAsset, PlantImportSettings, PlantManualSemanticTarget, PlantPivot,
    PlantSemanticDestination, PlantSourceRole, PlantTangentPolicy, SourceAxis, SourceHandedness,
    SourceUnits, SourceUvOrigin, SourceWinding,
};

use super::*;

pub(super) fn source_pivot(
    source: u128,
    settings: &PlantImportSettings,
    meshes: &[SelectedMesh<'_>],
    targets: Option<&Vec<&PlantManualSemanticTarget>>,
) -> std::result::Result<Vec3, String> {
    match settings.pivot {
        PlantPivot::SourceOrigin => Ok(Vec3::ZERO),
        PlantPivot::Explicit(position) => Ok(Vec3::new(
            position[0].to_f64() as f32,
            position[1].to_f64() as f32,
            position[2].to_f64() as f32,
        )),
        PlantPivot::BoundsBaseCenter => bounds_base_center(settings, meshes),
        PlantPivot::SemanticPart(part) => {
            let selectors = targets
                .into_iter()
                .flatten()
                .filter(|target| {
                    target.destination == PlantSemanticDestination::Part(part)
                        && target.source == source
                })
                .map(|target| &target.selector)
                .collect::<Vec<_>>();
            let selected = meshes
                .iter()
                .copied()
                .filter(|mesh| {
                    selectors
                        .iter()
                        .any(|selector| selector_matches_output(selector, &mesh.output_selector()))
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err("semantic-part pivot has no surviving geometry target".to_owned());
            }
            bounds_base_center(settings, &selected)
        }
    }
}

fn bounds_base_center(
    settings: &PlantImportSettings,
    meshes: &[SelectedMesh<'_>],
) -> std::result::Result<Vec3, String> {
    let basis = source_to_canonical(settings)?;
    let mut minimum = Vec3::splat(f32::INFINITY);
    let mut maximum = Vec3::splat(f32::NEG_INFINITY);
    for selected in meshes {
        for position in selected_positions(*selected)? {
            let transformed =
                basis.transform_point3(selected.snapshot.transform.transform_point3(position));
            if !transformed.is_finite() {
                return Err("source geometry has a non-finite transformed position".to_owned());
            }
            minimum = minimum.min(transformed);
            maximum = maximum.max(transformed);
        }
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return Err("source selection has no vertices".to_owned());
    }
    Ok(Vec3::new(
        (minimum.x + maximum.x) * 0.5,
        minimum.y,
        (minimum.z + maximum.z) * 0.5,
    ))
}

fn selected_positions(selected: SelectedMesh<'_>) -> std::result::Result<Vec<Vec3>, String> {
    let mesh = &selected.snapshot.mesh;
    let submeshes = selected.submesh.map_or(mesh.submeshes.as_slice(), |index| {
        std::slice::from_ref(&mesh.submeshes[index])
    });
    let mut positions = Vec::new();
    for submesh in submeshes {
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "source submesh range overflows".to_owned())?;
        let indices = mesh
            .indices
            .get(start..end)
            .ok_or_else(|| "source submesh range exceeds the index stream".to_owned())?;
        for index in indices {
            let addressed = i64::from(*index) + i64::from(submesh.vertex_offset);
            let addressed = usize::try_from(addressed)
                .map_err(|_| "source submesh vertex offset is out of range".to_owned())?;
            positions.push(
                mesh.vertices
                    .get(addressed)
                    .ok_or_else(|| "source index references a missing vertex".to_owned())?
                    .position,
            );
        }
    }
    Ok(positions)
}

pub(super) fn normalize_mesh(
    asset: &PlantFamilyAsset,
    source: u128,
    role: PlantSourceRole,
    settings: &PlantImportSettings,
    selected: SelectedMesh<'_>,
    pivot: Vec3,
) -> std::result::Result<NormalizedPlantMesh, String> {
    let selected_copy = selected_mesh_copy(selected)?;
    let mut mesh = selected_copy.mesh;
    validate_mesh_topology(&mesh)?;
    let basis = source_to_canonical(settings)?;
    let transform = basis * selected.snapshot.transform;
    let determinant = Mat3::from_mat4(transform).determinant();
    if !determinant.is_finite() || determinant.abs() <= 1.0e-12 {
        return Err("source transform is singular or non-finite".to_owned());
    }
    let normal_matrix = Mat3::from_mat4(transform).inverse().transpose();
    for vertex in &mut mesh.vertices {
        if !vertex.position.is_finite()
            || !vertex.normal.is_finite()
            || !vertex.uv0.is_finite()
            || !vertex.tangent.iter().all(|component| component.is_finite())
        {
            return Err("source vertex contains a non-finite attribute".to_owned());
        }
        vertex.position = transform.transform_point3(vertex.position) - pivot;
        vertex.normal = (normal_matrix * vertex.normal).normalize_or_zero();
        let tangent = transform.transform_vector3(Vec3::new(
            vertex.tangent[0],
            vertex.tangent[1],
            vertex.tangent[2],
        ));
        let tangent = (tangent - vertex.normal * vertex.normal.dot(tangent)).normalize_or_zero();
        vertex.tangent = [
            tangent.x,
            tangent.y,
            tangent.z,
            vertex.tangent[3] * determinant.signum(),
        ];
        if settings.uv_origin == SourceUvOrigin::BottomLeft {
            vertex.uv0.y = 1.0 - vertex.uv0.y;
        }
        vertex.uv0 = Vec2::new(
            vertex.uv0.x * settings.uv_scale[0].to_f64() as f32
                + settings.uv_offset[0].to_f64() as f32,
            vertex.uv0.y * settings.uv_scale[1].to_f64() as f32
                + settings.uv_offset[1].to_f64() as f32,
        );
    }
    let source_clockwise = settings.winding == SourceWinding::Clockwise;
    let reflection = determinant.is_sign_negative();
    if source_clockwise ^ reflection {
        for triangle in mesh.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    let invalid_tangent = mesh.vertices.iter().any(|vertex| {
        let tangent = Vec3::from_array([vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]]);
        !tangent.is_finite()
            || tangent.length_squared() < 0.5
            || vertex.tangent[3].abs() < 0.5
            || vertex.normal.dot(tangent).abs() > 1.0e-3
    });
    match settings.tangent_policy {
        PlantTangentPolicy::Require if invalid_tangent => {
            return Err("source tangent policy requires complete orthonormal frames".to_owned());
        }
        PlantTangentPolicy::GenerateMissing if invalid_tangent => compute_tangents(&mut mesh),
        PlantTangentPolicy::Regenerate => compute_tangents(&mut mesh),
        PlantTangentPolicy::Require | PlantTangentPolicy::GenerateMissing => {}
    }
    validate_mesh_geometry(&mesh)?;
    let material_map = family_material_map(asset);
    let mut submeshes = Vec::with_capacity(mesh.submeshes.len());
    for submesh in &mesh.submeshes {
        let source_material = selected
            .snapshot
            .material_slots
            .get(submesh.material_slot as usize)
            .ok_or_else(|| "source submesh material slot has no resolved material".to_owned())?;
        let material_slot = material_map
            .get(&source_material.value())
            .copied()
            .ok_or_else(|| "source material is absent from the family material table".to_owned())?;
        submeshes.push(NormalizedPlantSubmesh {
            first_index: submesh.first_index,
            index_count: submesh.index_count,
            material_slot,
        });
    }
    let vertices = mesh
        .vertices
        .iter()
        .map(normalize_vertex)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let selected_skin = if selected.snapshot.skin.is_empty() {
        Vec::new()
    } else {
        if selected.snapshot.skin.len() != selected.snapshot.mesh.vertices.len() {
            return Err("skin stream length does not match the source vertex count".to_owned());
        }
        selected_copy
            .source_vertices
            .iter()
            .map(|index| selected.snapshot.skin[*index])
            .collect()
    };
    let skin = normalize_skin(&selected_skin, mesh.vertices.len())?;
    Ok(NormalizedPlantMesh {
        source,
        role,
        selector: selected.output_selector(),
        vertices,
        indices: mesh.indices,
        submeshes,
        skin,
    })
}

struct SelectedMeshCopy {
    mesh: Mesh,
    source_vertices: Vec<usize>,
}

fn selected_mesh_copy(selected: SelectedMesh<'_>) -> std::result::Result<SelectedMeshCopy, String> {
    let mesh = &selected.snapshot.mesh;
    let selected_submeshes = match selected.submesh {
        Some(index) => vec![
            *mesh
                .submeshes
                .get(index)
                .ok_or_else(|| "selected submesh is absent".to_owned())?,
        ],
        None => mesh.submeshes.clone(),
    };
    if selected_submeshes.is_empty() {
        return Err("source mesh has no selected submeshes".to_owned());
    }
    let mut covered_source_indices = selected
        .submesh
        .is_none()
        .then(|| vec![false; mesh.indices.len()]);
    let mut addressed_submeshes = Vec::with_capacity(selected_submeshes.len());
    let mut referenced_vertices = BTreeSet::new();
    for submesh in selected_submeshes {
        if submesh.index_count == 0 || submesh.index_count % 3 != 0 {
            return Err("source submesh has an invalid index count".to_owned());
        }
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "selected submesh range overflows".to_owned())?;
        let source_indices = mesh
            .indices
            .get(start..end)
            .ok_or_else(|| "selected submesh range exceeds the index stream".to_owned())?;
        if let Some(covered) = &mut covered_source_indices {
            let covered = covered
                .get_mut(start..end)
                .ok_or_else(|| "selected submesh range exceeds the index stream".to_owned())?;
            if covered.iter().any(|covered| *covered) {
                return Err("source submesh ranges overlap".to_owned());
            }
            covered.fill(true);
        }
        let mut addressed_indices = Vec::with_capacity(submesh.index_count as usize);
        for index in source_indices {
            let addressed = i64::from(*index) + i64::from(submesh.vertex_offset);
            let addressed = usize::try_from(addressed)
                .map_err(|_| "source submesh vertex offset is out of range".to_owned())?;
            if addressed >= mesh.vertices.len() {
                return Err("source index references a missing vertex".to_owned());
            }
            referenced_vertices.insert(addressed);
            addressed_indices.push(addressed);
        }
        addressed_submeshes.push((submesh, addressed_indices));
    }
    if covered_source_indices
        .as_ref()
        .is_some_and(|covered| covered.iter().any(|covered| !covered))
    {
        return Err("source submeshes do not cover the complete index stream".to_owned());
    }
    let source_vertices = referenced_vertices.into_iter().collect::<Vec<_>>();
    let remap = source_vertices
        .iter()
        .enumerate()
        .map(|(output, source)| (*source, output as u32))
        .collect::<BTreeMap<_, _>>();
    let vertices = source_vertices
        .iter()
        .map(|index| mesh.vertices[*index])
        .collect();
    let mut indices = Vec::new();
    let mut submeshes = Vec::with_capacity(addressed_submeshes.len());
    for (source_submesh, addressed_indices) in addressed_submeshes {
        let first_index = indices.len() as u32;
        indices.extend(addressed_indices.iter().map(|index| remap[index]));
        submeshes.push(Submesh {
            first_index,
            index_count: source_submesh.index_count,
            vertex_offset: 0,
            material_slot: source_submesh.material_slot,
        });
    }
    Ok(SelectedMeshCopy {
        mesh: Mesh {
            vertices,
            indices,
            submeshes,
        },
        source_vertices,
    })
}

fn validate_mesh_topology(mesh: &Mesh) -> std::result::Result<(), String> {
    if mesh.vertices.is_empty() || mesh.indices.is_empty() || mesh.submeshes.is_empty() {
        return Err("source mesh must contain vertices, triangles, and submeshes".to_owned());
    }
    if !mesh.indices.len().is_multiple_of(3) {
        return Err("source index count is not a multiple of three".to_owned());
    }
    if mesh
        .indices
        .iter()
        .any(|index| *index as usize >= mesh.vertices.len())
    {
        return Err("source index references a missing vertex".to_owned());
    }
    let mut covered = vec![false; mesh.indices.len()];
    for submesh in &mesh.submeshes {
        if submesh.index_count == 0 || submesh.index_count % 3 != 0 {
            return Err("source submesh has an invalid index count".to_owned());
        }
        let start = submesh.first_index as usize;
        let end = start
            .checked_add(submesh.index_count as usize)
            .ok_or_else(|| "source submesh range overflows".to_owned())?;
        let range = covered
            .get_mut(start..end)
            .ok_or_else(|| "source submesh range exceeds the index stream".to_owned())?;
        if range.iter().any(|covered| *covered) {
            return Err("source submesh ranges overlap".to_owned());
        }
        range.fill(true);
    }
    if covered.iter().any(|covered| !covered) {
        return Err("source submeshes do not cover the complete index stream".to_owned());
    }
    Ok(())
}

fn validate_mesh_geometry(mesh: &Mesh) -> std::result::Result<(), String> {
    for triangle in mesh.indices.chunks_exact(3) {
        let first = &mesh.vertices[triangle[0] as usize];
        let second = &mesh.vertices[triangle[1] as usize];
        let third = &mesh.vertices[triangle[2] as usize];
        let cross = (second.position - first.position).cross(third.position - first.position);
        if !cross.is_finite() || cross.length_squared() <= 1.0e-16 {
            return Err("source mesh contains a degenerate triangle".to_owned());
        }
        let geometric_normal = cross.normalize();
        for vertex in [first, second, third] {
            let tangent = Vec3::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]);
            if !vertex.position.is_finite()
                || !vertex.normal.is_finite()
                || vertex.normal.length_squared() < 0.5
                || !vertex.uv0.is_finite()
                || !tangent.is_finite()
                || tangent.length_squared() < 0.5
                || vertex.normal.dot(tangent).abs() > 1.0e-3
                || vertex.tangent[3].abs() < 0.5
            {
                return Err("normalized vertex frame is incomplete or invalid".to_owned());
            }
            if geometric_normal.dot(vertex.normal) < -0.25 {
                return Err("source vertex normal opposes its geometric front face".to_owned());
            }
        }
    }
    Ok(())
}

fn normalize_vertex(
    vertex: &saffron_geometry::Vertex,
) -> std::result::Result<NormalizedPlantVertex, String> {
    Ok(NormalizedPlantVertex {
        position_bits: [
            fixed_bits(vertex.position.x)?,
            fixed_bits(vertex.position.y)?,
            fixed_bits(vertex.position.z)?,
        ],
        normal_snorm: [
            snorm16(vertex.normal.x)?,
            snorm16(vertex.normal.y)?,
            snorm16(vertex.normal.z)?,
        ],
        uv_bits: [fixed_bits(vertex.uv0.x)?, fixed_bits(vertex.uv0.y)?],
        tangent_snorm: [
            snorm16(vertex.tangent[0])?,
            snorm16(vertex.tangent[1])?,
            snorm16(vertex.tangent[2])?,
            if vertex.tangent[3].is_sign_negative() {
                -32_767
            } else {
                32_767
            },
        ],
    })
}

fn fixed_bits(value: f32) -> std::result::Result<i32, String> {
    DecisionScalar::from_f64(f64::from(value))
        .map(DecisionScalar::bits)
        .map_err(|error| error.to_string())
}

fn snorm16(value: f32) -> std::result::Result<i16, String> {
    if !value.is_finite() {
        return Err("cannot quantize a non-finite normalized component".to_owned());
    }
    let clamped = value.clamp(-1.0, 1.0);
    Ok((f64::from(clamped) * 32_767.0).round_ties_even() as i16)
}

pub(super) fn normalize_skin(
    source: &[VertexSkin],
    vertex_count: usize,
) -> std::result::Result<Vec<NormalizedPlantSkin>, String> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    if source.len() != vertex_count {
        return Err("skin stream length does not match the source vertex count".to_owned());
    }
    source
        .iter()
        .map(|skin| {
            if skin
                .weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight < 0.0)
            {
                return Err("skin stream contains an invalid weight".to_owned());
            }
            let mut combined = BTreeMap::<u16, f64>::new();
            for (joint, weight) in skin.joints.into_iter().zip(skin.weights) {
                *combined.entry(joint).or_default() += f64::from(weight);
            }
            let mut influences = combined.into_iter().collect::<Vec<_>>();
            influences.sort_by(|first, second| {
                second
                    .1
                    .total_cmp(&first.1)
                    .then_with(|| first.0.cmp(&second.0))
            });
            influences.truncate(4);
            let sum = influences.iter().map(|(_, weight)| *weight).sum::<f64>();
            if !sum.is_finite() || sum <= 0.0 {
                return Err("skin vertex has no positive structural weight".to_owned());
            }
            let mut joints = [0_u16; 4];
            let mut weights = [0_u16; 4];
            let mut accumulated = 0_u32;
            for (index, (joint, weight)) in influences.iter().enumerate() {
                joints[index] = *joint;
                let quantized = ((*weight / sum) * f64::from(u16::MAX)).round_ties_even() as u32;
                weights[index] = quantized.min(u32::from(u16::MAX)) as u16;
                accumulated += u32::from(weights[index]);
            }
            let target = u32::from(u16::MAX);
            if accumulated != target {
                let difference = target as i64 - accumulated as i64;
                let adjusted = i64::from(weights[0]) + difference;
                if !(0..=i64::from(u16::MAX)).contains(&adjusted) {
                    return Err(
                        "skin weight quantization could not preserve normalization".to_owned()
                    );
                }
                weights[0] = adjusted as u16;
            }
            Ok(NormalizedPlantSkin { joints, weights })
        })
        .collect()
}

pub(super) fn normalize_joints(
    source: u128,
    settings: &PlantImportSettings,
    pivot: Vec3,
    joints: &[&PlantSourceJointSnapshot],
) -> std::result::Result<Vec<NormalizedPlantJoint>, String> {
    let selectors = joints
        .iter()
        .map(|joint| joint.selector.clone())
        .collect::<BTreeSet<_>>();
    if selectors.len() != joints.len() {
        return Err("structural joint selectors are not unique".to_owned());
    }
    if joints.iter().any(|joint| {
        joint
            .parent
            .as_ref()
            .is_some_and(|parent| !selectors.contains(parent))
    }) {
        return Err("structural joint parent is outside the selected hierarchy".to_owned());
    }
    validate_joint_forest(joints)?;
    let basis = source_to_canonical(settings)?;
    let inverse_basis = basis.inverse();
    if !inverse_basis.is_finite() {
        return Err("source coordinate basis is singular".to_owned());
    }
    joints
        .iter()
        .map(|joint| {
            let transform =
                Mat4::from_translation(-pivot) * basis * joint.rest_transform * inverse_basis;
            if !transform.is_finite() {
                return Err("structural joint rest transform is non-finite".to_owned());
            }
            let columns = transform.to_cols_array();
            let mut transform_bits = [0_i32; 16];
            for (output, component) in transform_bits.iter_mut().zip(columns) {
                *output = fixed_bits(component)?;
            }
            Ok(NormalizedPlantJoint {
                source,
                selector: joint.selector.clone(),
                parent: joint.parent.clone(),
                transform_bits,
            })
        })
        .collect()
}

fn validate_joint_forest(joints: &[&PlantSourceJointSnapshot]) -> std::result::Result<(), String> {
    let parents = joints
        .iter()
        .map(|joint| (joint.selector.clone(), joint.parent.clone()))
        .collect::<BTreeMap<_, _>>();
    for start in parents.keys() {
        let mut active = BTreeSet::new();
        let mut current = Some(start.clone());
        while let Some(selector) = current {
            if !active.insert(selector.clone()) {
                return Err("structural joint hierarchy contains a cycle".to_owned());
            }
            current = parents.get(&selector).cloned().flatten();
        }
    }
    Ok(())
}

fn source_to_canonical(settings: &PlantImportSettings) -> std::result::Result<Mat4, String> {
    let up = axis_vector(settings.up_axis);
    let forward = axis_vector(settings.forward_axis);
    if up.dot(forward).abs() > f32::EPSILON {
        return Err("source up and forward axes are not orthogonal".to_owned());
    }
    let right = match settings.handedness {
        SourceHandedness::Right => forward.cross(up),
        SourceHandedness::Left => up.cross(forward),
    };
    let unit_scale = match settings.units {
        SourceUnits::Meters => 1.0,
        SourceUnits::Centimeters => 0.01,
        SourceUnits::Millimeters => 0.001,
        SourceUnits::Feet => 0.3048,
    };
    let scale = unit_scale * settings.scale.to_f64() as f32;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("source unit scale is invalid".to_owned());
    }
    let source_basis = Mat3::from_cols(right, up, -forward);
    let canonical_from_source = source_basis.transpose() * scale;
    Ok(Mat4::from_mat3(canonical_from_source))
}

fn axis_vector(axis: SourceAxis) -> Vec3 {
    match axis {
        SourceAxis::PositiveX => Vec3::X,
        SourceAxis::NegativeX => Vec3::NEG_X,
        SourceAxis::PositiveY => Vec3::Y,
        SourceAxis::NegativeY => Vec3::NEG_Y,
        SourceAxis::PositiveZ => Vec3::Z,
        SourceAxis::NegativeZ => Vec3::NEG_Z,
    }
}
