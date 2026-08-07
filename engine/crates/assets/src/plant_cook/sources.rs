//! Resolution of a family's declared sources into the compiler's snapshot inputs.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_geometry::{
    ChunkKind, Mesh, Submesh, VertexSkin, contour_alpha_card, load_mesh_from_bytes,
    load_mesh_skin_from_bytes,
};
use saffron_scene::AssetType;
use saffron_vegetation::{
    PlantCompileDiagnostic, PlantCompileDiagnosticCode, PlantCompileDiagnosticSeverity,
    PlantFamilyAsset, PlantFamilySource, PlantPartSemantic, PlantSourceLocator,
    PlantSourceMeshSnapshot, PlantSourceReference, PlantSourceRole, PlantSourceSelector,
    PlantSourceSnapshot, native_botanical_graph_content_hash, native_plant_source_id,
    vegetation_content_hash,
};

use crate::cook_reader::CookAssetAccess;
use crate::model::imported_node_world_transforms;
use crate::spawn::{imported_nodes_from_json, node_mesh_ids_from_json};
use crate::{Error, Result};

use super::import_source::{attribution_notice, catalog_joints, node_paths, resolve_file_source};
use super::materials::{
    ResolvedCoverageImages, ResolvedMaterialDocuments, resolve_catalog_material_source,
    resolve_catalog_materials,
};
use super::sections::{
    append_domain, append_optional_selector, append_selector, append_skin, append_u32, append_u64,
};

pub(super) struct ResolvedPlantInputs {
    pub(super) snapshots: Vec<PlantSourceSnapshot>,
    pub(super) material_documents: ResolvedMaterialDocuments,
    pub(super) coverage_images: ResolvedCoverageImages,
    pub(super) issues: Vec<PlantCompileDiagnostic>,
}

pub(super) fn resolve_plant_inputs(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
) -> ResolvedPlantInputs {
    let PlantFamilySource::Imported(recipe) = &asset.source else {
        return resolve_native_plant_input(assets, asset);
    };
    let mut snapshots = Vec::with_capacity(recipe.sources.len());
    let mut material_documents = BTreeMap::new();
    let mut coverage_images = ResolvedCoverageImages::new();
    let mut issues = Vec::new();
    let mut resolved_by_locator =
        BTreeMap::<String, std::result::Result<ResolvedPlantSource, String>>::new();
    for source in &recipe.sources {
        let key = source_locator_key(&source.locator);
        let resolved = resolved_by_locator
            .entry(key)
            .or_insert_with(|| {
                resolve_plant_source(assets, asset, source).map_err(|error| error.to_string())
            })
            .clone();
        match resolved {
            Ok(mut resolved) => {
                if let Some(notice) = attribution_notice(source, &resolved.origin) {
                    issues.push(notice);
                }
                resolved.snapshot.source = source.id;
                // The hash identifies what the SOURCE holds, so it is taken before any
                // family-specific derivation runs: geometry-first contours rewrite a geometry
                // source's snapshot and not a material source's, so hashing afterwards makes two
                // sources over one file disagree about that file's identity.
                resolved.snapshot.content_hash = source_snapshot_hash(&resolved.snapshot);
                if source.role == PlantSourceRole::Geometry
                    && geometry_first_semantic(asset, source.id)
                    && let Err(error) = apply_geometry_first_contours(
                        &mut resolved.snapshot,
                        &resolved.coverage_images,
                    )
                {
                    issues.push(resolution_issue(source, error.to_string()));
                    continue;
                }
                let conflict =
                    resolved
                        .material_documents
                        .iter()
                        .find_map(|(material, document)| {
                            material_documents
                                .get(material)
                                .filter(|previous| *previous != document)
                                .map(|_| *material)
                        });
                if let Some(material) = conflict {
                    issues.push(resolution_issue(
                        source,
                        format!("material {material} resolves to conflicting source documents"),
                    ));
                } else {
                    material_documents.extend(resolved.material_documents);
                    coverage_images.extend(resolved.coverage_images);
                    snapshots.push(resolved.snapshot);
                }
            }
            Err(error) => issues.push(resolution_issue(source, error)),
        }
    }
    ResolvedPlantInputs {
        snapshots,
        material_documents,
        coverage_images,
        issues,
    }
}

fn resolve_native_plant_input(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
) -> ResolvedPlantInputs {
    let PlantFamilySource::Native { graph, grafts } = &asset.source else {
        unreachable!();
    };
    match resolve_catalog_materials(
        assets,
        asset
            .material_slots
            .iter()
            .copied()
            .map(|material| (material, format!("materials/{}", material.value()))),
    ) {
        Ok((materials, mut material_documents, mut coverage_images)) => {
            let mut snapshots = vec![PlantSourceSnapshot {
                source: native_plant_source_id(asset.id),
                content_hash: native_botanical_graph_content_hash(graph),
                meshes: Vec::new(),
                materials,
                joints: Vec::new(),
                semantic_elements: Vec::new(),
            }];
            // A graft's hero mesh resolves exactly as an imported family's geometry does — same
            // locator, same importer, same snapshot — so there is one way external geometry
            // reaches a plant.
            let mut issues = Vec::new();
            for graft in grafts {
                match resolve_plant_source(assets, asset, graft) {
                    Ok(mut resolved) => {
                        resolved.snapshot.source = graft.id;
                        resolved.snapshot.content_hash = source_snapshot_hash(&resolved.snapshot);
                        material_documents.extend(resolved.material_documents);
                        coverage_images.extend(resolved.coverage_images);
                        snapshots.push(resolved.snapshot);
                    }
                    Err(error) => issues.push(resolution_issue(graft, error.to_string())),
                }
            }
            ResolvedPlantInputs {
                snapshots,
                material_documents,
                coverage_images,
                issues,
            }
        }
        Err(error) => ResolvedPlantInputs {
            snapshots: Vec::new(),
            material_documents: BTreeMap::new(),
            coverage_images: ResolvedCoverageImages::new(),
            issues: vec![PlantCompileDiagnostic {
                severity: PlantCompileDiagnosticSeverity::Error,
                code: PlantCompileDiagnosticCode::MissingMaterial,
                source: Some(native_plant_source_id(asset.id)),
                selector: None,
                path: "source.native.materials".to_owned(),
                message: error.to_string(),
            }],
        },
    }
}

#[derive(Clone)]
pub(super) struct ResolvedPlantSource {
    /// What the source file states about what wrote it and how it may be reused. Empty for a
    /// source whose format has no asset block to state it in.
    pub(super) origin: saffron_geometry::ImportedOrigin,
    pub(super) snapshot: PlantSourceSnapshot,
    pub(super) material_documents: ResolvedMaterialDocuments,
    pub(super) coverage_images: ResolvedCoverageImages,
}

fn resolve_plant_source(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    source: &PlantSourceReference,
) -> Result<ResolvedPlantSource> {
    let mut resolved = match &source.locator {
        PlantSourceLocator::Asset(id) => resolve_catalog_source(assets, asset, *id)?,
        PlantSourceLocator::File(uri) => resolve_file_source(assets, asset, uri)?,
    };
    resolved.snapshot.content_hash = source_snapshot_hash(&resolved.snapshot);
    Ok(resolved)
}

fn source_locator_key(locator: &PlantSourceLocator) -> String {
    match locator {
        PlantSourceLocator::Asset(asset) => format!("asset:{}", asset.value()),
        PlantSourceLocator::File(uri) => format!("file:{uri}"),
    }
}

fn resolve_catalog_source(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    id: Uuid,
) -> Result<ResolvedPlantSource> {
    let entry = assets
        .catalog()
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    match entry.asset_type {
        AssetType::Model => resolve_catalog_model(assets, id),
        AssetType::Mesh => resolve_catalog_mesh(assets, asset, &entry),
        AssetType::Material => resolve_catalog_material_source(assets, &entry),
        _ => Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "model, mesh, or material",
        }),
    }
}

fn resolve_catalog_model(
    assets: &mut dyn CookAssetAccess,
    model_id: Uuid,
) -> Result<ResolvedPlantSource> {
    let model = assets
        .load_model(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let nodes = imported_nodes_from_json(&model.meta.nodes);
    let node_meshes = node_mesh_ids_from_json(&model.meta.nodes);
    let world = imported_node_world_transforms(&nodes)?;
    let paths = node_paths(&nodes)?;
    let material_ids = model
        .meta
        .sub_assets
        .iter()
        .filter(|sub| sub.asset_type == AssetType::Material)
        .map(|sub| sub.sub_id)
        .collect::<Vec<_>>();
    let mut meshes = Vec::new();
    let mesh_ids = model
        .meta
        .sub_assets
        .iter()
        .filter(|sub| sub.asset_type == AssetType::Mesh)
        .map(|sub| sub.sub_id)
        .collect::<Vec<_>>();
    for mesh_id in mesh_ids {
        let source = assets.chunk_source(&model, ChunkKind::Mesh, mesh_id);
        if source.is_empty() {
            return Err(Error::ContainerMissingSubAsset {
                container: model_id.value(),
                sub: mesh_id.value(),
            });
        }
        let bytes = assets.read_source(&source)?;
        let node = node_meshes
            .iter()
            .position(|candidate| *candidate == mesh_id);
        meshes.push(PlantSourceMeshSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(mesh_id.value()),
                path: node
                    .and_then(|index| paths.get(index).cloned())
                    .unwrap_or_else(|| format!("meshes/{}", mesh_id.value())),
            },
            transform: node
                .and_then(|index| world.get(index).copied())
                .unwrap_or(Mat4::IDENTITY),
            mesh: load_mesh_from_bytes(&bytes)?,
            skin: load_mesh_skin_from_bytes(&bytes)?,
            material_slots: material_ids.clone(),
        });
    }
    let (materials, material_documents, coverage_images) = resolve_catalog_materials(
        assets,
        model
            .meta
            .sub_assets
            .iter()
            .filter(|sub| sub.asset_type == AssetType::Material)
            .map(|sub| (sub.sub_id, format!("materials/{}", sub.name))),
    )?;
    let joints = catalog_joints(&model.meta.name, &nodes, &world, &model.meta.skin)?;
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes,
        materials,
        joints,
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    Ok(ResolvedPlantSource {
        origin: saffron_geometry::ImportedOrigin::default(),
        snapshot,
        material_documents,
        coverage_images,
    })
}

fn resolve_catalog_mesh(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    entry: &saffron_scene::AssetEntry,
) -> Result<ResolvedPlantSource> {
    let payload = assets.load_mesh_source(entry.id)?;
    let mut transform = Mat4::IDENTITY;
    let mut path = entry.name.clone();
    let mut material_ids = asset.material_slots.clone();
    let mut materials = Vec::new();
    let mut material_documents = BTreeMap::new();
    let mut coverage_images = BTreeMap::new();
    let mut joints = Vec::new();
    if entry.container.value() != 0 {
        let model = assets.load_model(entry.container).ok_or_else(|| {
            Error::Io(format!("model {} is not loadable", entry.container.value()))
        })?;
        let nodes = imported_nodes_from_json(&model.meta.nodes);
        let node_meshes = node_mesh_ids_from_json(&model.meta.nodes);
        let world = imported_node_world_transforms(&nodes)?;
        let paths = node_paths(&nodes)?;
        if let Some(node) = node_meshes.iter().position(|mesh| *mesh == entry.id) {
            transform = world[node];
            path = paths[node].clone();
        }
        material_ids = model
            .meta
            .sub_assets
            .iter()
            .filter(|sub| sub.asset_type == AssetType::Material)
            .map(|sub| sub.sub_id)
            .collect();
        (materials, material_documents, coverage_images) = resolve_catalog_materials(
            assets,
            model
                .meta
                .sub_assets
                .iter()
                .filter(|sub| sub.asset_type == AssetType::Material)
                .map(|sub| (sub.sub_id, format!("materials/{}", sub.name))),
        )?;
        joints = catalog_joints(&model.meta.name, &nodes, &world, &model.meta.skin)?;
    }
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes: vec![PlantSourceMeshSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(entry.id.value()),
                path,
            },
            transform,
            mesh: payload.mesh,
            skin: payload.skin,
            material_slots: material_ids,
        }],
        materials,
        joints,
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    Ok(ResolvedPlantSource {
        origin: saffron_geometry::ImportedOrigin::default(),
        snapshot,
        material_documents,
        coverage_images,
    })
}

fn geometry_first_semantic(asset: &PlantFamilyAsset, source: u128) -> bool {
    asset.parts.iter().any(|part| {
        part.sources.contains(&source)
            && matches!(
                part.semantic,
                PlantPartSemantic::Frond
                    | PlantPartSemantic::Leaf
                    | PlantPartSemantic::Flower
                    | PlantPartSemantic::Blade
            )
    })
}

fn apply_geometry_first_contours(
    snapshot: &mut PlantSourceSnapshot,
    coverage_images: &ResolvedCoverageImages,
) -> Result<()> {
    for source in &mut snapshot.meshes {
        let original = source.mesh.clone();
        let original_skin = source.skin.clone();
        if !original_skin.is_empty() && original_skin.len() != original.vertices.len() {
            return Err(Error::Io(
                "alpha-card skin stream does not match the vertex stream".to_owned(),
            ));
        }
        let mut mesh = Mesh::default();
        let mut skin = Vec::new();
        for submesh_index in 0..original.submeshes.len() {
            let submesh = original.submeshes[submesh_index];
            let material = source
                .material_slots
                .get(submesh.material_slot as usize)
                .copied()
                .ok_or_else(|| Error::Io("alpha-card material slot is unresolved".to_owned()))?;
            let contoured = if let Some(coverage) = coverage_images.get(&material.value()) {
                contour_alpha_card(
                    &original,
                    submesh_index,
                    &coverage.alpha,
                    coverage.width,
                    coverage.height,
                    coverage.cutoff,
                )?
            } else {
                None
            };
            let first_index = u32::try_from(mesh.indices.len())
                .map_err(|_| Error::Io("alpha-card index count overflows u32".to_owned()))?;
            match contoured {
                Some(contoured) => {
                    let base = u32::try_from(mesh.vertices.len()).map_err(|_| {
                        Error::Io("alpha-card vertex count overflows u32".to_owned())
                    })?;
                    mesh.vertices
                        .extend(contoured.vertices.iter().map(|vertex| vertex.vertex));
                    mesh.indices.extend(
                        contoured
                            .indices
                            .iter()
                            .map(|index| base.checked_add(*index))
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| {
                                Error::Io("alpha-card index offset overflows u32".to_owned())
                            })?,
                    );
                    if !original_skin.is_empty() {
                        skin.extend(contoured.vertices.iter().map(|vertex| {
                            interpolate_card_skin(
                                &original_skin,
                                vertex.source_triangle,
                                vertex.barycentric,
                            )
                        }));
                    }
                }
                None => {
                    append_source_submesh(&original, &original_skin, submesh, &mut mesh, &mut skin)?
                }
            }
            mesh.submeshes.push(Submesh {
                first_index,
                index_count: u32::try_from(mesh.indices.len())
                    .map_err(|_| Error::Io("alpha-card index count overflows u32".to_owned()))?
                    - first_index,
                vertex_offset: 0,
                material_slot: submesh.material_slot,
            });
        }
        source.mesh = mesh;
        source.skin = skin;
    }
    Ok(())
}

fn append_source_submesh(
    source: &Mesh,
    source_skin: &[VertexSkin],
    submesh: Submesh,
    output: &mut Mesh,
    output_skin: &mut Vec<VertexSkin>,
) -> Result<()> {
    let begin = submesh.first_index as usize;
    let end = begin
        .checked_add(submesh.index_count as usize)
        .ok_or_else(|| Error::Io("alpha-card submesh range overflows".to_owned()))?;
    let indices = source
        .indices
        .get(begin..end)
        .ok_or_else(|| Error::Io("alpha-card submesh range exceeds the index stream".to_owned()))?;
    let mut remap = BTreeMap::<usize, u32>::new();
    for index in indices {
        let addressed = usize::try_from(i64::from(*index) + i64::from(submesh.vertex_offset))
            .map_err(|_| Error::Io("alpha-card vertex offset is out of range".to_owned()))?;
        let output_index = if let Some(index) = remap.get(&addressed) {
            *index
        } else {
            let vertex = *source.vertices.get(addressed).ok_or_else(|| {
                Error::Io("alpha-card index references a missing vertex".to_owned())
            })?;
            let output_index = u32::try_from(output.vertices.len())
                .map_err(|_| Error::Io("alpha-card vertex count overflows u32".to_owned()))?;
            output.vertices.push(vertex);
            if !source_skin.is_empty() {
                output_skin.push(source_skin[addressed]);
            }
            remap.insert(addressed, output_index);
            output_index
        };
        output.indices.push(output_index);
    }
    Ok(())
}

fn interpolate_card_skin(
    skin: &[VertexSkin],
    triangle: [u32; 3],
    barycentric: [f32; 3],
) -> VertexSkin {
    let mut influences = BTreeMap::<u16, f32>::new();
    for (index, barycentric_weight) in triangle.into_iter().zip(barycentric) {
        let source = skin[index as usize];
        for (joint, weight) in source.joints.into_iter().zip(source.weights) {
            *influences.entry(joint).or_default() += weight * barycentric_weight;
        }
    }
    let mut influences = influences.into_iter().collect::<Vec<_>>();
    influences.sort_by(|first, second| {
        second
            .1
            .total_cmp(&first.1)
            .then_with(|| first.0.cmp(&second.0))
    });
    influences.truncate(4);
    let total = influences.iter().map(|influence| influence.1).sum::<f32>();
    let mut output = VertexSkin::default();
    for (slot, (joint, weight)) in influences.into_iter().enumerate() {
        output.joints[slot] = joint;
        output.weights[slot] = if total > 0.0 { weight / total } else { 0.0 };
    }
    output
}

fn resolution_issue(source: &PlantSourceReference, message: String) -> PlantCompileDiagnostic {
    PlantCompileDiagnostic {
        severity: PlantCompileDiagnosticSeverity::Error,
        code: PlantCompileDiagnosticCode::MissingSource,
        source: Some(source.id),
        selector: Some(source.selector.clone()),
        path: "source.imported.sources.locator".to_owned(),
        message,
    }
}

pub(super) fn source_snapshot_hash(snapshot: &PlantSourceSnapshot) -> [u8; 32] {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/plant-source-snapshot/v1");
    append_u64(&mut bytes, snapshot.meshes.len() as u64);
    for mesh in &snapshot.meshes {
        append_selector(&mut bytes, &mesh.selector);
        for value in mesh.transform.to_cols_array() {
            append_u32(&mut bytes, value.to_bits());
        }
        append_u64(&mut bytes, mesh.mesh.vertices.len() as u64);
        for vertex in &mesh.mesh.vertices {
            for value in vertex.position.to_array() {
                append_u32(&mut bytes, value.to_bits());
            }
            for value in vertex.normal.to_array() {
                append_u32(&mut bytes, value.to_bits());
            }
            for value in vertex.uv0.to_array() {
                append_u32(&mut bytes, value.to_bits());
            }
            for value in vertex.tangent {
                append_u32(&mut bytes, value.to_bits());
            }
        }
        append_u64(&mut bytes, mesh.mesh.indices.len() as u64);
        for index in &mesh.mesh.indices {
            append_u32(&mut bytes, *index);
        }
        append_u64(&mut bytes, mesh.mesh.submeshes.len() as u64);
        for submesh in &mesh.mesh.submeshes {
            append_u32(&mut bytes, submesh.first_index);
            append_u32(&mut bytes, submesh.index_count);
            bytes.extend_from_slice(&submesh.vertex_offset.to_be_bytes());
            append_u32(&mut bytes, submesh.material_slot);
        }
        append_skin(&mut bytes, &mesh.skin);
        append_u64(&mut bytes, mesh.material_slots.len() as u64);
        for material in &mesh.material_slots {
            append_u64(&mut bytes, material.value());
        }
    }
    let mut materials = snapshot.materials.iter().collect::<Vec<_>>();
    materials.sort_by_key(|material| material.material.value());
    append_u64(&mut bytes, materials.len() as u64);
    for material in materials {
        append_selector(&mut bytes, &material.selector);
        append_u64(&mut bytes, material.material.value());
        bytes.extend_from_slice(&material.content_hash);
    }
    append_u64(&mut bytes, snapshot.joints.len() as u64);
    for joint in &snapshot.joints {
        append_selector(&mut bytes, &joint.selector);
        append_optional_selector(&mut bytes, joint.parent.as_ref());
        for value in joint.rest_transform.to_cols_array() {
            append_u32(&mut bytes, value.to_bits());
        }
    }
    vegetation_content_hash(&bytes)
}

#[cfg(test)]
mod tests {
    use super::super::materials::ResolvedCoverageImage;
    use super::super::test_support::alpha_card;
    use super::*;
    use saffron_core::Uuid;
    use saffron_geometry::glam::Mat4;
    use saffron_vegetation::{PlantSourceMeshSnapshot, PlantSourceSelector, PlantSourceSnapshot};

    #[test]
    fn geometry_first_cook_replaces_alpha_card_empty_silhouette() {
        let material = Uuid(91);
        let mut snapshot = PlantSourceSnapshot {
            source: 10,
            content_hash: [0; 32],
            meshes: vec![PlantSourceMeshSnapshot {
                selector: PlantSourceSelector::Whole,
                transform: Mat4::IDENTITY,
                mesh: alpha_card(),
                skin: Vec::new(),
                material_slots: vec![material],
            }],
            materials: Vec::new(),
            joints: Vec::new(),
            semantic_elements: Vec::new(),
        };
        let coverage = BTreeMap::from([(
            material.value(),
            ResolvedCoverageImage {
                alpha: vec![255, 255, 0, 0, 255, 255, 0, 0],
                rgba: vec![255; 8 * 4],
                width: 4,
                height: 2,
                cutoff: 128,
                texture: None,
            },
        )]);
        apply_geometry_first_contours(&mut snapshot, &coverage).unwrap();
        let mesh = &snapshot.meshes[0].mesh;
        assert_eq!(mesh.indices.len(), 6);
        assert!(mesh.vertices.iter().all(|vertex| vertex.uv0.x <= 0.5));
        assert!(mesh.vertices.iter().all(|vertex| vertex.position.x <= 0.0));
    }
}
