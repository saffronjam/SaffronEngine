//! Normalization of an imported `.splant` recipe.

use std::collections::{BTreeMap, BTreeSet};

use crate::{PlantFamilyAsset, PlantSourceRole, PlantSourceSelector, Result};

use super::*;

pub(super) fn compile_imported_plant_family(
    asset: &PlantFamilyAsset,
    recipe: &crate::ImportedPlantFamilyRecipe,
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
) -> Result<PlantCompileOutput> {
    let mut diagnostics = Vec::new();
    let mut conflicts = PlantReimportConflictReport::default();
    let mut source_updates = Vec::new();
    let mut statistics = PlantCompileStatistics::default();
    if recipe.sources.len() > limits.sources as usize || snapshots.len() > limits.sources as usize {
        push_limit(
            &mut diagnostics,
            limits,
            "source.imported.sources",
            "plant source count exceeds the compile limit",
        )?;
        return Ok(PlantCompileOutput {
            family: None,
            family_hash: None,
            diagnostics,
            conflicts,
            source_updates,
            statistics,
        });
    }
    let mut snapshot_by_source = BTreeMap::new();
    let mut duplicate_sources = BTreeSet::new();
    for snapshot in snapshots {
        if duplicate_sources.contains(&snapshot.source) {
            continue;
        }
        if snapshot_by_source.remove(&snapshot.source).is_some() {
            duplicate_sources.insert(snapshot.source);
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::DuplicateSource,
                    Some(snapshot.source),
                    None,
                    "source",
                    "more than one canonical snapshot carries this source identity",
                ),
            )?;
        } else {
            snapshot_by_source.insert(snapshot.source, snapshot);
        }
    }

    for target in &recipe.semantic_targets {
        if duplicate_sources.contains(&target.source) {
            continue;
        }
        match snapshot_by_source.get(&target.source) {
            None => conflicts
                .conflicts
                .push(conflict(target, PlantReimportConflictReason::MissingSource)),
            Some(snapshot) if !snapshot.contains_selector(&target.selector) => {
                conflicts.conflicts.push(conflict(
                    target,
                    PlantReimportConflictReason::MissingElement,
                ))
            }
            Some(_) => {}
        }
    }
    conflicts.conflicts.sort_by(|first, second| {
        (first.source, first.target, &first.selector).cmp(&(
            second.source,
            second.target,
            &second.selector,
        ))
    });

    let (material_snapshots, conflicting_materials) =
        collect_material_snapshots(snapshot_by_source.values().copied());
    let normalized_materials = resolve_materials(
        asset,
        &material_snapshots,
        &conflicting_materials,
        &mut diagnostics,
        limits,
    )?;
    statistics.materials = normalized_materials.len() as u64;

    let mut meshes = Vec::new();
    let mut joints = Vec::new();
    let targets_by_source = semantic_targets_by_source(&recipe.semantic_targets);
    let mut source_dependencies = Vec::new();
    let mut sorted_sources = recipe.sources.iter().collect::<Vec<_>>();
    sorted_sources.sort_by_key(|source| source.id);
    for source in sorted_sources {
        if duplicate_sources.contains(&source.id) {
            statistics.rejected = statistics.rejected.saturating_add(1);
            continue;
        }
        let Some(snapshot) = snapshot_by_source.get(&source.id).copied() else {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingSource,
                    Some(source.id),
                    Some(source.selector.clone()),
                    "source.imported.sources",
                    "the recipe source has no canonical snapshot",
                ),
            )?;
            statistics.rejected = statistics.rejected.saturating_add(1);
            continue;
        };
        statistics.sources = statistics.sources.saturating_add(1);
        source_dependencies.push((source.id, snapshot.content_hash));
        if source.content_hash != snapshot.content_hash {
            source_updates.push(PlantSourceHashUpdate {
                source: source.id,
                previous: source.content_hash,
                current: snapshot.content_hash,
            });
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Info,
                    PlantCompileDiagnosticCode::SourceChanged,
                    Some(source.id),
                    Some(source.selector.clone()),
                    "source.imported.sources.contentHash",
                    "source content changed and will be accepted by a successful recook",
                ),
            )?;
        }
        match source.role {
            PlantSourceRole::Geometry
            | PlantSourceRole::Collision
            | PlantSourceRole::Navigation => {
                let selected = selected_meshes(snapshot, &source.selector);
                if selected.is_empty() {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.imported.sources.selector",
                            "the selected source contains no matching mesh payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                    continue;
                }
                let pivot = source_pivot(
                    source.id,
                    &source.settings,
                    &selected,
                    targets_by_source.get(&source.id),
                );
                let pivot = match pivot {
                    Ok(pivot) => pivot,
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::BoundsMismatch,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.imported.sources.settings.pivot",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        continue;
                    }
                };
                let selected = if source.role == PlantSourceRole::Geometry {
                    match partition_selected_by_parts(
                        asset,
                        source.id,
                        selected,
                        targets_by_source.get(&source.id),
                    ) {
                        Ok(selected) => selected,
                        Err(partition) => {
                            push_diagnostic(
                                &mut diagnostics,
                                limits,
                                diagnostic(
                                    PlantCompileDiagnosticSeverity::Error,
                                    PlantCompileDiagnosticCode::InvalidGeometry,
                                    Some(source.id),
                                    Some(source.selector.clone()),
                                    partition.path,
                                    &partition.message,
                                ),
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                            continue;
                        }
                    }
                } else {
                    selected
                };
                for selected_mesh in selected {
                    if meshes.len() >= limits.meshes as usize {
                        push_limit(
                            &mut diagnostics,
                            limits,
                            "source.meshes",
                            "selected mesh count exceeds the compile limit",
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        break;
                    }
                    match normalize_mesh(
                        asset,
                        source.id,
                        source.role,
                        &source.settings,
                        selected_mesh,
                        pivot,
                    ) {
                        Ok(mesh) => {
                            let requested_vertices = statistics
                                .vertices
                                .saturating_add(mesh.vertices.len() as u64);
                            let requested_indices =
                                statistics.indices.saturating_add(mesh.indices.len() as u64);
                            if requested_vertices > limits.vertices
                                || requested_indices > limits.indices
                            {
                                push_limit(
                                    &mut diagnostics,
                                    limits,
                                    "source.geometry",
                                    "normalized geometry exceeds the compile limit",
                                )?;
                                statistics.rejected = statistics.rejected.saturating_add(1);
                                continue;
                            }
                            statistics.vertices = requested_vertices;
                            statistics.indices = requested_indices;
                            statistics.meshes = statistics.meshes.saturating_add(1);
                            meshes.push(mesh);
                        }
                        Err(message) => {
                            push_diagnostic(
                                &mut diagnostics,
                                limits,
                                diagnostic(
                                    PlantCompileDiagnosticSeverity::Error,
                                    PlantCompileDiagnosticCode::InvalidGeometry,
                                    Some(source.id),
                                    Some(selected_mesh.output_selector()),
                                    "source.geometry",
                                    &message,
                                ),
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                        }
                    }
                }
            }
            PlantSourceRole::Material => {
                if !snapshot_selector_has_material(snapshot, &source.selector) {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.material",
                            "the selected source contains no matching material payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                }
            }
            PlantSourceRole::Skeleton => {
                let selected = selected_joints(snapshot, &source.selector);
                if selected.is_empty() {
                    push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::EmptySelection,
                            Some(source.id),
                            Some(source.selector.clone()),
                            "source.skeleton",
                            "the selected source contains no matching joint payload",
                        ),
                    )?;
                    statistics.rejected = statistics.rejected.saturating_add(1);
                    continue;
                }
                let pivot_meshes = selected_meshes(snapshot, &PlantSourceSelector::Whole);
                let pivot = match source_pivot(
                    source.id,
                    &source.settings,
                    &pivot_meshes,
                    targets_by_source.get(&source.id),
                ) {
                    Ok(pivot) => pivot,
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::BoundsMismatch,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.skeleton.settings.pivot",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                        continue;
                    }
                };
                match normalize_joints(source.id, &source.settings, pivot, &selected) {
                    Ok(mut normalized) => {
                        if joints.len().saturating_add(normalized.len()) > limits.joints as usize {
                            push_limit(
                                &mut diagnostics,
                                limits,
                                "source.skeleton.joints",
                                "structural joint count exceeds the compile limit",
                            )?;
                            statistics.rejected = statistics.rejected.saturating_add(1);
                        } else {
                            statistics.joints =
                                statistics.joints.saturating_add(normalized.len() as u64);
                            joints.append(&mut normalized);
                        }
                    }
                    Err(message) => {
                        push_diagnostic(
                            &mut diagnostics,
                            limits,
                            diagnostic(
                                PlantCompileDiagnosticSeverity::Error,
                                PlantCompileDiagnosticCode::InvalidSkeleton,
                                Some(source.id),
                                Some(source.selector.clone()),
                                "source.skeleton",
                                &message,
                            ),
                        )?;
                        statistics.rejected = statistics.rejected.saturating_add(1);
                    }
                }
            }
        }
    }

    source_updates.sort();
    source_dependencies.sort_by_key(|(source, _)| *source);
    meshes.sort_by(|first, second| {
        (first.source, role_tag(first.role), &first.selector).cmp(&(
            second.source,
            role_tag(second.role),
            &second.selector,
        ))
    });
    joints.sort_by(|first, second| {
        (first.source, &first.selector).cmp(&(second.source, &second.selector))
    });
    validate_geometry_contract(
        asset,
        &meshes,
        &material_snapshots,
        &recipe.semantic_targets,
        &mut diagnostics,
        limits,
    )?;

    sort_diagnostics(&mut diagnostics);
    let blocked = !conflicts.conflicts.is_empty()
        || diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == PlantCompileDiagnosticSeverity::Error);
    let family = (!blocked).then_some(NormalizedPlantFamily {
        family: asset.id,
        tags: asset.tags.clone(),
        sources: source_dependencies,
        meshes,
        joints,
        materials: normalized_materials,
        dimensions: asset.dimensions,
    });
    let family_hash = family
        .as_ref()
        .map(NormalizedPlantFamily::canonical_hash)
        .transpose()?;
    Ok(PlantCompileOutput {
        family,
        family_hash,
        diagnostics,
        conflicts,
        source_updates,
        statistics,
    })
}
