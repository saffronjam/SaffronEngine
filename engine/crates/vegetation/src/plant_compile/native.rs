//! Growth and normalization of a native botanical `.splant`.

use std::collections::BTreeMap;

use saffron_geometry::glam::Vec3;

use crate::{PlantFamilyAsset, PlantSourceRole, Result};

use super::*;

pub(super) fn compile_native_plant_family(
    asset: &PlantFamilyAsset,
    graph: &crate::BotanicalGraphDocument,
    grafts: &[crate::PlantSourceReference],
    snapshots: &[PlantSourceSnapshot],
    limits: PlantCompileLimits,
    modules: &dyn crate::BotanicalModuleResolver,
) -> Result<PlantCompileOutput> {
    let source = native_plant_source_id(asset.id);
    let content_hash = native_botanical_graph_content_hash(graph);
    let mut diagnostics = Vec::new();
    let mut statistics = PlantCompileStatistics::default();
    let mut source_updates = Vec::new();
    let snapshot = snapshots.iter().find(|snapshot| snapshot.source == source);
    let graft_by_id: BTreeMap<u128, &crate::PlantSourceReference> =
        grafts.iter().map(|graft| (graft.id, graft)).collect();
    let mut graft_snapshots: BTreeMap<u128, &PlantSourceSnapshot> = BTreeMap::new();
    let mut unexpected = false;
    for candidate in snapshots {
        if candidate.source == source {
            continue;
        }
        if graft_by_id.contains_key(&candidate.source) {
            if graft_snapshots
                .insert(candidate.source, candidate)
                .is_some()
            {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::DuplicateSource,
                        Some(candidate.source),
                        None,
                        "source.native.graft",
                        "more than one snapshot carries this graft source identity",
                    ),
                )?;
            }
        } else {
            unexpected = true;
        }
    }
    if unexpected {
        push_diagnostic(
            &mut diagnostics,
            limits,
            diagnostic(
                PlantCompileDiagnosticSeverity::Error,
                PlantCompileDiagnosticCode::DuplicateSource,
                Some(source),
                None,
                "source.native",
                "a native botanical family accepts only its own snapshot and its declared grafts",
            ),
        )?;
    }
    for graft in grafts {
        let Some(resolved) = graft_snapshots.get(&graft.id) else {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::MissingSource,
                    Some(graft.id),
                    Some(graft.selector.clone()),
                    "source.native.graft",
                    "graft source has no resolved snapshot",
                ),
            )?;
            continue;
        };
        statistics.sources += 1;
        if resolved.content_hash != graft.content_hash {
            source_updates.push(PlantSourceHashUpdate {
                source: graft.id,
                previous: graft.content_hash,
                current: resolved.content_hash,
            });
        }
    }
    if let Some(snapshot) = snapshot {
        statistics.sources = 1;
        if snapshot.content_hash != content_hash {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::SourceChanged,
                    Some(source),
                    None,
                    "source.native.graph",
                    "native source snapshot does not match the embedded botanical graph",
                ),
            )?;
        }
        if !snapshot.meshes.is_empty() || !snapshot.joints.is_empty() {
            push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidGeometry,
                    Some(source),
                    None,
                    "source.native.generatedPayload",
                    "a native family grows its geometry; an imported payload cannot stand in for it",
                ),
            )?;
        }
    }

    let mut generated = None;
    let mut meshes = Vec::new();
    let mut joints = Vec::new();
    let mut assemblies = Vec::new();
    let mut failed = false;
    for index in 0..graph.variations.len() {
        let growth = match crate::grow(graph, index, modules, &crate::BotanicalBudget::COOK) {
            Ok(growth) => growth,
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::InvalidGeometry,
                        Some(source),
                        None,
                        "source.native.graph",
                        &error.to_string(),
                    ),
                )?;
                failed = true;
                break;
            }
        };
        // The edit layer is shared, so every variation orphans the same set; report it once.
        if index == 0 {
            for orphan in &growth.diagnostics.orphans {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Warning,
                        PlantCompileDiagnosticCode::OrphanedEdit,
                        Some(source),
                        None,
                        "source.native.edits",
                        &format!(
                            "manual {} edit on element {:032x} has no target: {}",
                            orphan.action.name(),
                            orphan.target.value(),
                            orphan.reason.name()
                        ),
                    ),
                )?;
            }
        }
        let mut grafted: BTreeMap<crate::BotanicalElementId, Vec<NormalizedPlantMesh>> =
            BTreeMap::new();
        for graft in &growth.assembly.grafts {
            let (Some(reference), Some(resolved)) = (
                graft_by_id.get(&graft.source),
                graft_snapshots.get(&graft.source),
            ) else {
                continue;
            };
            let selected = selected_meshes(resolved, &graft.selector);
            if selected.is_empty() {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::EmptySelection,
                        Some(graft.source),
                        Some(graft.selector.clone()),
                        "source.native.graft.selection",
                        "graft selection contains no source geometry",
                    ),
                )?;
                continue;
            }
            for mesh in selected {
                match normalize_mesh(
                    asset,
                    graft.source,
                    PlantSourceRole::Geometry,
                    &reference.settings,
                    mesh,
                    Vec3::ZERO,
                ) {
                    Ok(normalized) => grafted.entry(graft.id).or_default().push(normalized),
                    Err(message) => push_diagnostic(
                        &mut diagnostics,
                        limits,
                        diagnostic(
                            PlantCompileDiagnosticSeverity::Error,
                            PlantCompileDiagnosticCode::InvalidGeometry,
                            Some(graft.source),
                            Some(graft.selector.clone()),
                            "source.native.graft.geometry",
                            &message,
                        ),
                    )?,
                }
            }
        }
        statistics.grafts += growth.assembly.grafts.len() as u64;
        let variation_source = native_variation_source_id(index);
        match crate::normalize_botanical_geometry(variation_source, &growth.assembly, &grafted) {
            Ok(geometry) => {
                // Skin joint indices are family-global, so this variation's shift past the prior ones.
                let base = u16::try_from(joints.len()).unwrap_or(u16::MAX);
                for mut mesh in geometry.meshes {
                    for skin in &mut mesh.skin {
                        for joint in &mut skin.joints {
                            *joint = joint.saturating_add(base);
                        }
                    }
                    meshes.push(mesh);
                }
                joints.extend(geometry.joints);
                assemblies.push(growth.assembly);
            }
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    limits,
                    diagnostic(
                        PlantCompileDiagnosticSeverity::Error,
                        PlantCompileDiagnosticCode::InvalidGeometry,
                        Some(variation_source),
                        None,
                        "source.native.generation",
                        &error.to_string(),
                    ),
                )?;
                failed = true;
                break;
            }
        }
    }
    if !failed {
        match crate::widest_family_structure(&assemblies) {
            Ok(structure) => {
                statistics.meshes = meshes.len() as u64;
                statistics.joints = joints.len() as u64;
                statistics.vertices = meshes.iter().map(|mesh| mesh.vertices.len() as u64).sum();
                statistics.indices = meshes.iter().map(|mesh| mesh.indices.len() as u64).sum();
                generated = Some((meshes, joints, structure));
            }
            Err(error) => push_diagnostic(
                &mut diagnostics,
                limits,
                diagnostic(
                    PlantCompileDiagnosticSeverity::Error,
                    PlantCompileDiagnosticCode::InvalidGeometry,
                    Some(source),
                    None,
                    "source.native.generation",
                    &error.to_string(),
                ),
            )?,
        }
    }

    let (material_snapshots, conflicting_materials) = collect_material_snapshots(snapshot);
    let materials = resolve_materials(
        asset,
        &material_snapshots,
        &conflicting_materials,
        &mut diagnostics,
        limits,
    )?;
    statistics.materials = materials.len() as u64;
    sort_diagnostics(&mut diagnostics);
    let blocked = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == PlantCompileDiagnosticSeverity::Error);
    let family = if blocked { None } else { generated }.map(|(meshes, joints, structure)| {
        NormalizedPlantFamily {
            family: asset.id,
            tags: asset.tags.clone(),
            sources: std::iter::once((source, content_hash))
                .chain(
                    graft_snapshots
                        .values()
                        .map(|snapshot| (snapshot.source, snapshot.content_hash)),
                )
                .collect(),
            meshes,
            joints,
            materials,
            // A native family's dimensions are grown, never authored.
            dimensions: structure.dimensions,
        }
    });
    let family_hash = family
        .as_ref()
        .map(NormalizedPlantFamily::canonical_hash)
        .transpose()?;
    Ok(PlantCompileOutput {
        family,
        family_hash,
        diagnostics,
        conflicts: PlantReimportConflictReport::default(),
        source_updates,
        statistics,
    })
}
