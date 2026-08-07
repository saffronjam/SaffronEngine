//! Material resolution and the authored geometry/coverage contract.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AlphaClassification, CoverageSource, PlantFamilyAsset, PlantManualSemanticTarget,
    PlantPartSemantic, PlantSemanticDestination, PlantSourceRole, Result,
};

use super::*;

pub(super) fn collect_material_snapshots<'a>(
    snapshots: impl IntoIterator<Item = &'a PlantSourceSnapshot>,
) -> (
    BTreeMap<u64, &'a PlantSourceMaterialSnapshot>,
    BTreeSet<u64>,
) {
    let mut materials = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for snapshot in snapshots {
        for material in &snapshot.materials {
            let identity = material.material.value();
            if let Some(previous) = materials.get(&identity) {
                if *previous != material {
                    conflicts.insert(identity);
                }
            } else {
                materials.insert(identity, material);
            }
        }
    }
    (materials, conflicts)
}

pub(super) fn resolve_materials(
    asset: &PlantFamilyAsset,
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    conflicts: &BTreeSet<u64>,
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<Vec<NormalizedPlantMaterial>> {
    let mut normalized = Vec::with_capacity(asset.material_slots.len());
    for material in &asset.material_slots {
        let Some(snapshot) = materials.get(&material.value()).copied() else {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingMaterial,
                    None,
                    None,
                    "materialSlots",
                    "family material slot has no resolved material snapshot",
                ),
            )?;
            continue;
        };
        if conflicts.contains(&material.value())
            || snapshot.content_hash == [0; 32]
            || snapshot.surface.validate().is_err()
        {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidMaterial,
                    None,
                    Some(snapshot.selector.clone()),
                    "materialSlots",
                    "resolved material or coverage contract is invalid",
                ),
            )?;
            continue;
        }
        normalized.push(NormalizedPlantMaterial {
            material: *material,
            content_hash: snapshot.content_hash,
        });
    }
    Ok(normalized)
}

pub(super) fn validate_geometry_contract(
    asset: &PlantFamilyAsset,
    meshes: &[NormalizedPlantMesh],
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    targets: &[PlantManualSemanticTarget],
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<()> {
    let bounds = asset.dimensions;
    for mesh in meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
    {
        for vertex in &mesh.vertices {
            if (0..3).any(|axis| {
                vertex.position_bits[axis] < bounds.local_bounds_min[axis].bits()
                    || vertex.position_bits[axis] > bounds.local_bounds_max[axis].bits()
            }) {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.localBounds",
                        "normalized geometry lies outside the authored conservative bounds",
                    ),
                )?;
                break;
            }
        }
    }
    let part_by_id = asset
        .parts
        .iter()
        .map(|part| (part.id, part))
        .collect::<BTreeMap<_, _>>();
    for target in targets {
        let PlantSemanticDestination::Part(part_id) = target.destination else {
            continue;
        };
        let Some(part) = part_by_id.get(&part_id).copied() else {
            continue;
        };
        let selected = meshes.iter().filter(|mesh| {
            mesh.source == target.source
                && mesh.role == PlantSourceRole::Geometry
                && selector_matches_output(&target.selector, &mesh.selector)
        });
        for mesh in selected {
            if matches!(
                part.semantic,
                PlantPartSemantic::Branch
                    | PlantPartSemantic::Frond
                    | PlantPartSemantic::Leaf
                    | PlantPartSemantic::Flower
                    | PlantPartSemantic::Fruit
            ) && mesh.vertices.iter().any(|vertex| {
                vertex.position_bits[0].unsigned_abs()
                    > bounds.crown_radius[0].bits().unsigned_abs()
                    || vertex.position_bits[2].unsigned_abs()
                        > bounds.crown_radius[1].bits().unsigned_abs()
            }) {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.crownRadius",
                        "crown semantic geometry exceeds the authored crown footprint",
                    ),
                )?;
            }
            if part.semantic == PlantPartSemantic::Root
                && mesh.vertices.iter().any(|vertex| {
                    vertex.position_bits[0].unsigned_abs()
                        > bounds.root_radius[0].bits().unsigned_abs()
                        || vertex.position_bits[2].unsigned_abs()
                            > bounds.root_radius[1].bits().unsigned_abs()
                })
            {
                push_diagnostic(
                    diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::BoundsMismatch,
                        Some(mesh.source),
                        Some(mesh.selector.clone()),
                        "dimensions.rootRadius",
                        "root semantic geometry exceeds the authored root footprint",
                    ),
                )?;
            }
            if matches!(
                part.semantic,
                PlantPartSemantic::Frond | PlantPartSemantic::Leaf | PlantPartSemantic::Blade
            ) {
                validate_leaf_mesh(asset, mesh, materials, diagnostics, limits)?;
            }
        }
    }
    Ok(())
}

fn validate_leaf_mesh(
    asset: &PlantFamilyAsset,
    mesh: &NormalizedPlantMesh,
    materials: &BTreeMap<u64, &PlantSourceMaterialSnapshot>,
    diagnostics: &mut Vec<PlantCompileDiagnostic>,
    limits: PlantCompileLimits,
) -> Result<()> {
    for submesh in &mesh.submeshes {
        let Some(material_id) = asset.material_slots.get(submesh.material_slot as usize) else {
            continue;
        };
        let Some(material) = materials.get(&material_id.value()).copied() else {
            continue;
        };
        let requires_uv = material.alpha_classification != AlphaClassification::Opaque
            && material.coverage_source != CoverageSource::ModeledGeometry;
        if !requires_uv {
            continue;
        }
        let start = submesh.first_index as usize;
        let end = start.saturating_add(submesh.index_count as usize);
        let has_uv_area = mesh.indices.get(start..end).is_some_and(|indices| {
            indices.chunks_exact(3).any(|triangle| {
                let first = mesh.vertices[triangle[0] as usize].uv_bits;
                let second = mesh.vertices[triangle[1] as usize].uv_bits;
                let third = mesh.vertices[triangle[2] as usize].uv_bits;
                let ab = [second[0] - first[0], second[1] - first[1]];
                let ac = [third[0] - first[0], third[1] - first[1]];
                i64::from(ab[0]) * i64::from(ac[1]) - i64::from(ab[1]) * i64::from(ac[0]) != 0
            })
        });
        if !has_uv_area {
            push_diagnostic(
                diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingCoverageUv,
                    Some(mesh.source),
                    Some(mesh.selector.clone()),
                    "parts.coverage",
                    "leaf-like covered geometry has no usable UV area",
                ),
            )?;
        }
    }
    Ok(())
}

pub(super) fn family_material_map(asset: &PlantFamilyAsset) -> BTreeMap<u64, u32> {
    asset
        .material_slots
        .iter()
        .enumerate()
        .map(|(index, material)| (material.value(), index as u32))
        .collect()
}
