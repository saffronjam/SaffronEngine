//! Plant-source resolution and atomic `.splantc` publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_geometry::{
    AlphaMode, ChunkKind, ImportedMaterial, ImportedModel, ImportedNode, Mesh, Submesh, VertexSkin,
    VirtualHierarchyMaterial, contour_alpha_card, cook_portable_virtual_hierarchy,
    decode_image_from_memory, decode_portable_virtual_hierarchy_sections, load_mesh_from_bytes,
    load_mesh_skin_from_bytes, sub_id_for, translate_model,
};
use saffron_scene::AssetType;
use saffron_spatial::DecisionScalar;
use saffron_vegetation::{
    AlphaClassification, ContentHash, CookDependency, CookDependencyAddress, CookNodeAddress,
    CookNodeRecord, CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
    CoverageSource, MaterialSurface, NormalizedPlantFamily, NormalizedPlantMesh,
    PLANT_ASSET_VERSION, PLANT_COMPILED_ARTIFACT_VERSION, PLANT_SOURCE_COMPILER_VERSION,
    PlantCompileDiagnostic, PlantCompileDiagnosticCode, PlantCompileDiagnosticSeverity,
    PlantCompileLimits, PlantCompileOutput, PlantCompiledArtifactHeader,
    PlantCompiledArtifactIndex, PlantCompiledSection, PlantCompiledSectionKind, PlantFamilyAsset,
    PlantFamilySource, PlantImportSettings, PlantPartSemantic, PlantPivot,
    PlantReimportConflictReason, PlantSemanticDestination, PlantSourceLocator,
    PlantSourceMaterialSnapshot, PlantSourceMeshSnapshot, PlantSourceReference, PlantSourceRole,
    PlantSourceSelector, PlantSourceSnapshot, PlantTangentPolicy, SourceAxis, SourceHandedness,
    SourceUnits, SourceUvOrigin, SourceWinding, compile_plant_family,
    native_botanical_graph_content_hash, native_plant_source_id, plant_asset_schema_hash,
    plant_compiled_artifact_schema_hash, plant_hierarchy_input, plant_hierarchy_material,
    vegetation_content_hash, write_plant_asset, write_plant_compiled_artifact,
};

use crate::cook_reader::CookAssetAccess;
use crate::material::{
    MaterialAsset, apply_overrides, default_material_asset, material_asset_from_json,
    material_asset_to_json,
};
use crate::model::imported_node_world_transforms;
use crate::spawn::{imported_nodes_from_json, imported_skin_from_json, node_mesh_ids_from_json};
use crate::vegetation::update_plant_family_asset;
use crate::{AssetServer, DEFAULT_MATERIAL_ID, Error, Result, VegetationArtifactPublication};

/// Inputs that pin one plant recook to an exact compiler and platform contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantRecookOptions {
    /// Hard source-normalization bounds.
    pub limits: PlantCompileLimits,
    /// Complete cook semantic versions.
    pub versions: CookVersionSet,
    /// Exact target/toolchain/content profile.
    pub platform: CookPlatformProfile,
}

/// Pure source-resolution and compiler result used by plant validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantValidationOutcome {
    /// The single compiler result.
    pub compile: PlantCompileOutput,
    /// Canonical exact dependencies used to calculate a recook key.
    pub dependencies: Vec<CookDependency>,
}

/// One resolved and compiled family ready for validation or immutable publication.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedPlantFamily {
    /// Pure compiler result and the exact dependencies used by its cook key.
    pub validation: PlantValidationOutcome,
    /// Authored family with observed source hashes accepted in memory.
    pub accepted_asset: PlantFamilyAsset,
    material_documents: BTreeMap<u64, Vec<u8>>,
    hierarchy_materials: Vec<VirtualHierarchyMaterial>,
}

impl PreparedPlantFamily {
    /// Calculates the immutable artifact key without publishing or changing authored data.
    pub fn cook_key(&self, options: &PlantRecookOptions) -> Result<ContentHash> {
        calculate_plant_cook_key(
            self.accepted_asset.id,
            &self.validation.dependencies,
            options,
        )
    }
}

/// Successful immutable plant publication and accepted authored source hashes.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishedPlantRecook {
    /// Validation result that was admitted for publication.
    pub validation: PlantValidationOutcome,
    /// Atomically published content-addressed artifact.
    pub publication: VegetationArtifactPublication,
    /// Exact pre-execution cook identity stored in the artifact header.
    pub cook_key: ContentHash,
    /// Authored family after accepting observed source hashes.
    pub accepted_asset: PlantFamilyAsset,
    /// Measured execution and cache statistics.
    pub work: CookWorkActual,
}

/// Typed result of a recook request.
#[derive(Clone, Debug, PartialEq)]
pub enum PlantRecookOutcome {
    /// Source resolution or compilation rejected publication.
    Rejected(Box<PlantValidationOutcome>),
    /// A validated artifact was published and source hashes were accepted.
    Published(Box<PublishedPlantRecook>),
}

/// Resolves every source through the asset layer and invokes the sole plant compiler.
pub fn validate_plant_family_sources(
    assets: &mut AssetServer,
    asset: &PlantFamilyAsset,
    limits: PlantCompileLimits,
) -> Result<PlantValidationOutcome> {
    Ok(prepare_plant_family_sources(assets, asset, limits)?.validation)
}

/// Resolves and compiles one family once for validation, cache lookup, or publication.
pub fn prepare_plant_family_sources(
    assets: &mut AssetServer,
    asset: &PlantFamilyAsset,
    limits: PlantCompileLimits,
) -> Result<PreparedPlantFamily> {
    prepare_plant_family_sources_from(assets, asset, limits)
}

pub(crate) fn prepare_plant_family_sources_from(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    limits: PlantCompileLimits,
) -> Result<PreparedPlantFamily> {
    let resolved = resolve_plant_inputs(assets, asset);
    let mut compile = compile_plant_family(asset, &resolved.snapshots, limits)?;
    let hierarchy_materials = hierarchy_materials(&compile, &resolved.snapshots);
    merge_resolution_issues(&mut compile, resolved.issues);
    let accepted_asset = asset_with_source_updates(asset, &compile.source_updates)?;
    let dependencies = plant_dependencies(
        &accepted_asset,
        &resolved.snapshots,
        &resolved.material_documents,
    )?;
    Ok(PreparedPlantFamily {
        validation: PlantValidationOutcome {
            compile,
            dependencies,
        },
        accepted_asset,
        material_documents: resolved.material_documents,
        hierarchy_materials,
    })
}

/// Recooks one family, validates the complete artifact, publishes atomically, and then accepts
/// observed imported-source hashes in the authored `.splant`.
pub fn recook_plant_family(
    assets: &mut AssetServer,
    asset: &PlantFamilyAsset,
    options: &PlantRecookOptions,
) -> Result<PlantRecookOutcome> {
    let prepared = prepare_plant_family_sources(assets, asset, options.limits)?;
    let expected = ContentHash::of(&write_plant_asset(asset)?);
    let outcome =
        stage_prepared_plant_family(&assets.vegetation_artifact_store(), prepared, options)?;
    if let PlantRecookOutcome::Published(publication) = &outcome {
        let store = assets.vegetation_artifact_store();
        let _lock = store.lock_authored()?;
        let current = crate::load_plant_family_asset(assets, publication.accepted_asset.id)?;
        if ContentHash::of(&write_plant_asset(&current)?) != expected {
            return Err(Error::VegetationCookInputChanged {
                path: format!("plant-family/{}", publication.accepted_asset.id.value()),
            });
        }
        update_plant_family_asset(
            assets,
            publication.accepted_asset.id,
            &publication.accepted_asset,
        )?;
    }
    Ok(outcome)
}

pub(crate) fn stage_prepared_plant_family(
    store: &crate::VegetationArtifactStore,
    prepared: PreparedPlantFamily,
    options: &PlantRecookOptions,
) -> Result<PlantRecookOutcome> {
    let started = Instant::now();
    let PreparedPlantFamily {
        validation,
        accepted_asset,
        material_documents,
        hierarchy_materials,
    } = prepared;
    if !validation.compile.publishable() {
        return Ok(PlantRecookOutcome::Rejected(Box::new(validation)));
    }

    let platform_profile = options.platform.identity()?;
    let cook_key = calculate_plant_cook_key(accepted_asset.id, &validation.dependencies, options)?;
    let sections = build_plant_sections(
        &accepted_asset,
        &validation.compile,
        &material_documents,
        &hierarchy_materials,
    )?;
    let bytes = write_plant_compiled_artifact(
        PlantCompiledArtifactHeader {
            family: accepted_asset.id,
            cook_key,
            platform_profile,
        },
        &sections,
    )?;
    validate_complete_plant_artifact(&bytes, accepted_asset.id, cook_key, platform_profile)?;
    let publication = store.publish_plant(&bytes)?;

    let elapsed_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let work = CookWorkActual {
        elapsed_micros,
        input_bytes: validation
            .dependencies
            .iter()
            .fold(0_u64, |total, _| total.saturating_add(32)),
        output_bytes: publication.bytes,
        rejection_count: validation.compile.statistics.rejected,
        cache_hit: publication.cache_hit,
        ..CookWorkActual::default()
    };
    Ok(PlantRecookOutcome::Published(Box::new(
        PublishedPlantRecook {
            validation,
            publication,
            cook_key,
            accepted_asset,
            work,
        },
    )))
}

fn merge_resolution_issues(compile: &mut PlantCompileOutput, issues: Vec<PlantCompileDiagnostic>) {
    if issues.is_empty() {
        return;
    }
    compile.diagnostics.extend(issues);
    compile.diagnostics.sort_by(|first, second| {
        (
            first.severity,
            first.code,
            first.source,
            &first.selector,
            &first.path,
            &first.message,
        )
            .cmp(&(
                second.severity,
                second.code,
                second.source,
                &second.selector,
                &second.path,
                &second.message,
            ))
    });
    compile.family = None;
    compile.family_hash = None;
}

fn hierarchy_materials(
    compile: &PlantCompileOutput,
    snapshots: &[PlantSourceSnapshot],
) -> Vec<VirtualHierarchyMaterial> {
    let resolved = snapshots
        .iter()
        .flat_map(|snapshot| &snapshot.materials)
        .map(|material| (material.material.value(), material))
        .collect::<BTreeMap<_, _>>();
    compile
        .family
        .as_ref()
        .into_iter()
        .flat_map(|family| family.materials.iter().enumerate())
        .filter_map(|(slot, material)| {
            resolved.get(&material.material.value()).map(|snapshot| {
                plant_hierarchy_material(
                    slot as u32,
                    &snapshot.surface,
                    snapshot.alpha_classification,
                )
            })
        })
        .collect()
}

fn asset_with_source_updates(
    asset: &PlantFamilyAsset,
    updates: &[saffron_vegetation::PlantSourceHashUpdate],
) -> Result<PlantFamilyAsset> {
    let mut accepted = asset.clone();
    let declared = match &mut accepted.source {
        PlantFamilySource::Imported(recipe) => &mut recipe.sources,
        PlantFamilySource::Native { grafts, .. } => grafts,
    };
    for update in updates {
        let source = declared
            .iter_mut()
            .find(|source| source.id == update.source)
            .ok_or_else(|| Error::Io("compiler returned an unknown plant source".to_owned()))?;
        source.content_hash = update.current;
    }
    Ok(accepted)
}

struct ResolvedPlantInputs {
    snapshots: Vec<PlantSourceSnapshot>,
    material_documents: BTreeMap<u64, Vec<u8>>,
    issues: Vec<PlantCompileDiagnostic>,
}

fn resolve_plant_inputs(
    assets: &mut dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
) -> ResolvedPlantInputs {
    let PlantFamilySource::Imported(recipe) = &asset.source else {
        return resolve_native_plant_input(assets, asset);
    };
    let mut snapshots = Vec::with_capacity(recipe.sources.len());
    let mut material_documents = BTreeMap::new();
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
                resolved.snapshot.content_hash = source_snapshot_hash(&resolved.snapshot);
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
                    snapshots.push(resolved.snapshot);
                }
            }
            Err(error) => issues.push(resolution_issue(source, error)),
        }
    }
    ResolvedPlantInputs {
        snapshots,
        material_documents,
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
        Ok((materials, mut material_documents, _)) => {
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
                        snapshots.push(resolved.snapshot);
                    }
                    Err(error) => issues.push(resolution_issue(graft, error.to_string())),
                }
            }
            ResolvedPlantInputs {
                snapshots,
                material_documents,
                issues,
            }
        }
        Err(error) => ResolvedPlantInputs {
            snapshots: Vec::new(),
            material_documents: BTreeMap::new(),
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
struct ResolvedPlantSource {
    /// What the source file states about what wrote it and how it may be reused. Empty for a source
    /// whose format has no asset block to state it in.
    origin: saffron_geometry::ImportedOrigin,
    snapshot: PlantSourceSnapshot,
    material_documents: BTreeMap<u64, Vec<u8>>,
    coverage_images: BTreeMap<u64, ResolvedCoverageImage>,
}

type ResolvedMaterialDocuments = BTreeMap<u64, Vec<u8>>;
type ResolvedCoverageImages = BTreeMap<u64, ResolvedCoverageImage>;
type ResolvedCatalogMaterials = (
    Vec<PlantSourceMaterialSnapshot>,
    ResolvedMaterialDocuments,
    ResolvedCoverageImages,
);

#[derive(Clone)]
struct ResolvedCoverageImage {
    alpha: Vec<u8>,
    width: u32,
    height: u32,
    cutoff: u8,
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

fn resolve_catalog_material_source(
    assets: &mut dyn CookAssetAccess,
    entry: &saffron_scene::AssetEntry,
) -> Result<ResolvedPlantSource> {
    let (materials, material_documents, coverage_images) = resolve_catalog_materials(
        assets,
        std::iter::once((entry.id, format!("materials/{}", entry.name))),
    )?;
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes: Vec::new(),
        materials,
        joints: Vec::new(),
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

fn resolve_catalog_materials(
    assets: &mut dyn CookAssetAccess,
    materials: impl IntoIterator<Item = (Uuid, String)>,
) -> Result<ResolvedCatalogMaterials> {
    let mut snapshots = Vec::new();
    let mut documents = BTreeMap::new();
    let mut coverage_images = BTreeMap::new();
    for (id, path) in materials {
        let (material, document) = resolve_catalog_material_strict(assets, id)?;
        let (classification, coverage_source) = material_coverage(&material);
        if let Some(image) = resolve_material_coverage_image(assets, &material)? {
            coverage_images.insert(id.value(), image);
        }
        let content_hash = vegetation_content_hash(&document);
        snapshots.push(PlantSourceMaterialSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(id.value()),
                path,
            },
            material: id,
            content_hash,
            surface: material.surface.clone(),
            alpha_classification: classification,
            coverage_source,
        });
        documents.insert(id.value(), document);
    }
    Ok((snapshots, documents, coverage_images))
}

fn resolve_catalog_material_strict(
    assets: &mut dyn CookAssetAccess,
    id: Uuid,
) -> Result<(MaterialAsset, Vec<u8>)> {
    let mut chain = Vec::new();
    let mut active = BTreeSet::new();
    let mut current = id;
    while current.value() != 0 {
        if !active.insert(current.value()) {
            return Err(Error::Io(
                "material parent hierarchy contains a cycle".to_owned(),
            ));
        }
        if chain.len() >= 1_024 {
            return Err(Error::Io(
                "material parent hierarchy exceeds 1024 entries".to_owned(),
            ));
        }
        let material = load_cook_material_asset_raw(assets, current)?;
        current = material.parent;
        chain.push(material);
    }
    let mut resolved = chain
        .pop()
        .ok_or_else(|| Error::Io("material identity is missing".to_owned()))?;
    while let Some(child) = chain.pop() {
        apply_overrides(&mut resolved, &child.overrides);
        resolved.parent = child.parent;
        resolved.overrides = child.overrides;
    }
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/plant-material-source/v1");
    append_json(&mut bytes, &material_asset_to_json(&resolved));
    let mut textures = material_texture_ids(&resolved);
    textures.sort_unstable_by_key(|texture| texture.value());
    textures.dedup();
    for texture in textures {
        append_u64(&mut bytes, texture.value());
        append_bytes(&mut bytes, &read_catalog_asset_bytes(assets, texture)?);
    }
    Ok((resolved, bytes))
}

fn load_cook_material_asset_raw(
    assets: &mut dyn CookAssetAccess,
    id: Uuid,
) -> Result<MaterialAsset> {
    if id == DEFAULT_MATERIAL_ID {
        return Ok(default_material_asset());
    }
    let entry = assets
        .catalog()
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Material {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "material",
        });
    }
    let bytes = if entry.container.value() == 0 {
        assets.read_file(&assets.root().join(&entry.path))?
    } else {
        let model = assets.load_model(entry.container).ok_or_else(|| {
            Error::Io(format!(
                "material {}: container {} is not loadable",
                id.value(),
                entry.container.value()
            ))
        })?;
        let source = assets.chunk_source(&model, ChunkKind::Material, id);
        if source.is_empty() {
            return Err(Error::ContainerMissingSubAsset {
                container: entry.container.value(),
                sub: id.value(),
            });
        }
        assets.read_source(&source)?
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::Io(format!("material {} is not UTF-8: {error}", id.value())))?;
    material_asset_from_json(&saffron_json::parse_json(text)?)
}

fn read_catalog_asset_bytes(assets: &mut dyn CookAssetAccess, id: Uuid) -> Result<Vec<u8>> {
    let entry = assets
        .catalog()
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Texture {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "texture",
        });
    }
    if entry.container.value() == 0 {
        return assets.read_file(&assets.root().join(entry.path));
    }
    let model = assets
        .load_model(entry.container)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", entry.container.value())))?;
    let source = assets.chunk_source(&model, ChunkKind::Texture, id);
    if source.is_empty() {
        return Err(Error::ContainerMissingSubAsset {
            container: entry.container.value(),
            sub: id.value(),
        });
    }
    assets.read_source(&source)
}

fn resolve_material_coverage_image(
    assets: &mut dyn CookAssetAccess,
    material: &MaterialAsset,
) -> Result<Option<ResolvedCoverageImage>> {
    let (texture, cutoff, multiply_base_alpha) = match &material.surface {
        MaterialSurface::ThinSheetFoliage(parameters) => {
            let texture = match parameters.coverage_source {
                CoverageSource::AlbedoAlpha => material.albedo_texture,
                CoverageSource::Texture(texture) => texture,
                CoverageSource::ModeledGeometry => return Ok(None),
            };
            (
                texture,
                unit_interval_to_u8(parameters.coverage.reference_cutoff),
                matches!(parameters.coverage_source, CoverageSource::AlbedoAlpha),
            )
        }
        MaterialSurface::Standard if material.blend == "masked" => (
            material.albedo_texture,
            normalized_f32_to_u8(material.alpha_cutoff),
            true,
        ),
        MaterialSurface::Standard => return Ok(None),
    };
    if texture.value() == 0 {
        return Ok(None);
    }
    let decoded = decode_image_from_memory(&read_catalog_asset_bytes(assets, texture)?)?;
    let base_alpha = if multiply_base_alpha {
        material.base_color.w
    } else {
        1.0
    };
    Ok(Some(resolved_coverage_image(
        decoded.rgba,
        decoded.width,
        decoded.height,
        cutoff,
        base_alpha,
    )))
}

fn imported_material_coverage_image(
    material: &ImportedMaterial,
) -> Result<Option<ResolvedCoverageImage>> {
    if material.alpha_mode != AlphaMode::Mask {
        return Ok(None);
    }
    let Some(texture) = &material.albedo else {
        return Ok(None);
    };
    let decoded = decode_image_from_memory(&texture.bytes)?;
    Ok(Some(resolved_coverage_image(
        decoded.rgba,
        decoded.width,
        decoded.height,
        normalized_f32_to_u8(material.alpha_cutoff),
        material.base_color.w,
    )))
}

fn resolved_coverage_image(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    cutoff: u8,
    base_alpha: f32,
) -> ResolvedCoverageImage {
    let alpha = rgba
        .chunks_exact(4)
        .map(|pixel| normalized_f32_to_u8(f32::from(pixel[3]) / 255.0 * base_alpha))
        .collect();
    ResolvedCoverageImage {
        alpha,
        width,
        height,
        cutoff,
    }
}

fn unit_interval_to_u8(value: saffron_spatial::UnitInterval) -> u8 {
    u8::try_from((u32::from(value.bits()) * 255 + 32_767) / 65_535)
        .expect("unit interval maps to u8")
}

fn normalized_f32_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
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

fn resolve_file_source(
    assets: &dyn CookAssetAccess,
    asset: &PlantFamilyAsset,
    uri: &str,
) -> Result<ResolvedPlantSource> {
    let path = source_file_path(assets.root(), uri)?;
    let _ = assets.read_file(&path)?;
    let graph = translate_model(&path)?;
    resolve_imported_model(asset, uri, &path, graph)
}

fn resolve_imported_model(
    asset: &PlantFamilyAsset,
    uri: &str,
    path: &Path,
    graph: ImportedModel,
) -> Result<ResolvedPlantSource> {
    let origin = graph.origin.clone();
    let model_key = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Io("plant source file has no UTF-8 stem".to_owned()))?;
    let world = imported_node_world_transforms(&graph.nodes)?;
    let paths = node_paths(&graph.nodes)?;
    let material_selectors = graph
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| PlantSourceSelector::Element {
            id: u128::from(
                sub_id_for(
                    model_key,
                    "material",
                    &material_name(material, index),
                    index as u32,
                )
                .value(),
            ),
            path: format!("materials/{}", material_name(material, index)),
        })
        .collect::<Vec<_>>();
    let material_ids = material_selectors
        .iter()
        .map(|selector| mapped_file_material(asset, uri, selector))
        .collect::<Result<Vec<_>>>()?;
    let mut materials = Vec::new();
    let mut material_documents = BTreeMap::new();
    let mut coverage_images = BTreeMap::new();
    for ((source_material, selector), material) in graph
        .materials
        .iter()
        .zip(&material_selectors)
        .zip(&material_ids)
    {
        let document = imported_material_document(source_material);
        let (classification, coverage_source) = imported_material_coverage(source_material);
        materials.push(PlantSourceMaterialSnapshot {
            selector: selector.clone(),
            material: *material,
            content_hash: vegetation_content_hash(&document),
            surface: MaterialSurface::Standard,
            alpha_classification: classification,
            coverage_source,
        });
        if let Some(image) = imported_material_coverage_image(source_material)? {
            coverage_images.insert(material.value(), image);
        }
        material_documents.insert(material.value(), document);
    }
    let mut meshes = Vec::new();
    let skin_node = graph
        .skin
        .as_ref()
        .map(|skin| skin.desc.mesh_node)
        .unwrap_or(-1);
    for (index, node) in graph.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.clone() else {
            continue;
        };
        let mesh_id = sub_id_for(model_key, "mesh", &node.name, index as u32);
        let skin = if index as i32 == skin_node {
            graph
                .skin
                .as_ref()
                .map(|skin| skin.stream.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        meshes.push(PlantSourceMeshSnapshot {
            selector: PlantSourceSelector::Element {
                id: u128::from(mesh_id.value()),
                path: paths[index].clone(),
            },
            transform: world[index],
            mesh,
            skin,
            material_slots: material_ids.clone(),
        });
    }
    let joints = match &graph.skin {
        Some(skin) => imported_joints(model_key, &graph.nodes, &world, &skin.desc.joints)?,
        None => Vec::new(),
    };
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
        origin,
        snapshot,
        material_documents,
        coverage_images,
    })
}

fn catalog_joints(
    model_key: &str,
    nodes: &[ImportedNode],
    world: &[Mat4],
    skin: &saffron_json::Value,
) -> Result<Vec<saffron_vegetation::PlantSourceJointSnapshot>> {
    let skin = imported_skin_from_json(skin);
    imported_joints(model_key, nodes, world, &skin.joints)
}

fn imported_joints(
    model_key: &str,
    nodes: &[ImportedNode],
    world: &[Mat4],
    joints: &[i32],
) -> Result<Vec<saffron_vegetation::PlantSourceJointSnapshot>> {
    let joint_nodes = joints
        .iter()
        .map(|joint| {
            usize::try_from(*joint)
                .ok()
                .filter(|index| *index < nodes.len())
                .ok_or_else(|| Error::Io("skin joint index is out of range".to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let selectors = joint_nodes
        .iter()
        .map(|index| joint_selector(model_key, &nodes[*index], *index))
        .collect::<Vec<_>>();
    let selector_by_node = joint_nodes
        .iter()
        .copied()
        .zip(selectors.iter().cloned())
        .collect::<BTreeMap<_, _>>();
    joint_nodes
        .iter()
        .zip(selectors)
        .map(|(node, selector)| {
            let parent = usize::try_from(nodes[*node].parent)
                .ok()
                .and_then(|parent| selector_by_node.get(&parent).cloned());
            Ok(saffron_vegetation::PlantSourceJointSnapshot {
                selector,
                parent,
                rest_transform: *world
                    .get(*node)
                    .ok_or_else(|| Error::Io("skin joint transform is missing".to_owned()))?,
            })
        })
        .collect()
}

fn joint_selector(model_key: &str, node: &ImportedNode, index: usize) -> PlantSourceSelector {
    PlantSourceSelector::Element {
        id: u128::from(sub_id_for(model_key, "joint", &node.name, index as u32).value()),
        path: format!("joints/{}", node_component(node, index)),
    }
}

fn node_paths(nodes: &[ImportedNode]) -> Result<Vec<String>> {
    let mut paths = Vec::with_capacity(nodes.len());
    for index in 0..nodes.len() {
        let mut components = vec![node_component(&nodes[index], index)];
        let mut parent = nodes[index].parent;
        let mut active = BTreeSet::new();
        active.insert(index);
        while parent >= 0 {
            let parent_index = usize::try_from(parent)
                .map_err(|_| Error::Io("model node parent is invalid".to_owned()))?;
            let node = nodes
                .get(parent_index)
                .ok_or_else(|| Error::Io("model node parent is out of range".to_owned()))?;
            if !active.insert(parent_index) {
                return Err(Error::Io(
                    "model node hierarchy contains a cycle".to_owned(),
                ));
            }
            components.push(node_component(node, parent_index));
            parent = node.parent;
        }
        components.reverse();
        paths.push(components.join("/"));
    }
    Ok(paths)
}

fn node_component(node: &ImportedNode, index: usize) -> String {
    if node.name.trim().is_empty() {
        format!("node_{index}")
    } else {
        node.name.clone()
    }
}

fn source_file_path(root: &Path, uri: &str) -> Result<PathBuf> {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(extension.as_str(), "gltf" | "glb" | "obj") {
        return Err(Error::Io(
            "plant file source must be a .gltf, .glb, or .obj".to_owned(),
        ));
    }
    if !path.is_file() {
        return Err(Error::Io(format!(
            "plant source '{}' is not a file",
            path.display()
        )));
    }
    Ok(path)
}

fn mapped_file_material(
    asset: &PlantFamilyAsset,
    uri: &str,
    selector: &PlantSourceSelector,
) -> Result<Uuid> {
    let PlantFamilySource::Imported(recipe) = &asset.source else {
        return Err(Error::Io("plant family is not imported".to_owned()));
    };
    let mut slots = BTreeSet::new();
    for source in recipe.sources.iter().filter(|source| {
        source.role == PlantSourceRole::Material
            && source.locator == PlantSourceLocator::File(uri.to_owned())
    }) {
        for target in recipe.semantic_targets.iter().filter(|target| {
            target.source == source.id && selectors_match(&target.selector, selector)
        }) {
            if let PlantSemanticDestination::MaterialSlot(slot) = target.destination {
                slots.insert(slot);
            }
        }
    }
    if slots.len() != 1 {
        return Err(Error::Io(format!(
            "file material selector {selector:?} requires exactly one material-slot target"
        )));
    }
    let slot = slots
        .first()
        .copied()
        .ok_or_else(|| Error::Io("file material target is missing".to_owned()))?
        as usize;
    asset
        .material_slots
        .get(slot)
        .copied()
        .ok_or_else(|| Error::Io("file material target is out of range".to_owned()))
}

fn selectors_match(first: &PlantSourceSelector, second: &PlantSourceSelector) -> bool {
    match (first, second) {
        (PlantSourceSelector::Whole, PlantSourceSelector::Whole) => true,
        (
            PlantSourceSelector::Element { id: first, .. },
            PlantSourceSelector::Element { id: second, .. },
        ) => first == second,
        (
            PlantSourceSelector::Submesh {
                element: first_element,
                index: first_index,
            },
            PlantSourceSelector::Submesh {
                element: second_element,
                index: second_index,
            },
        ) => first_element == second_element && first_index == second_index,
        _ => false,
    }
}

fn material_name(material: &ImportedMaterial, index: usize) -> String {
    if material.name.trim().is_empty() {
        format!("material_{index}")
    } else {
        material.name.clone()
    }
}

fn imported_material_document(material: &ImportedMaterial) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/imported-plant-material/v1");
    append_string(&mut bytes, &material.name);
    for value in material.base_color.to_array() {
        append_u32(&mut bytes, value.to_bits());
    }
    append_u32(&mut bytes, material.metallic.to_bits());
    append_u32(&mut bytes, material.roughness.to_bits());
    for value in material.emissive.to_array() {
        append_u32(&mut bytes, value.to_bits());
    }
    append_u32(&mut bytes, material.emissive_strength.to_bits());
    append_u32(&mut bytes, material.alpha_cutoff.to_bits());
    bytes.push(match material.alpha_mode {
        AlphaMode::Opaque => 0,
        AlphaMode::Mask => 1,
        AlphaMode::Blend => 2,
    });
    bytes.push(u8::from(material.double_sided));
    for texture in [
        &material.albedo,
        &material.metallic_roughness,
        &material.normal,
        &material.occlusion,
        &material.emissive_tex,
    ] {
        match texture {
            Some(texture) => {
                bytes.push(1);
                append_string(&mut bytes, &texture.ext);
                append_bytes(&mut bytes, &texture.bytes);
            }
            None => bytes.push(0),
        }
    }
    bytes
}

/// Decodes one pinned material document into its resolved [`MaterialAsset`]. The two
/// document forms dispatch on their domain: a catalog material pins its parent-resolved
/// `.smat` JSON ([`resolve_catalog_material_strict`]); an imported source pins the
/// binary parameter record ([`imported_material_document`]). The embedded texture
/// payloads are cook inputs (coverage/atlas derivation) and are skipped; textures
/// resolve through the catalog by id.
pub(crate) fn decode_plant_material_document(bytes: &[u8]) -> Result<MaterialAsset> {
    let mut probe = SectionReader::new(bytes);
    if probe.read_bytes()? == b"saffron-anima/plant-material-source/v1" {
        let document = saffron_json::parse_json(&String::from_utf8_lossy(probe.read_bytes()?))
            .map_err(|err| Error::Io(format!("pinned plant material document: {err}")))?;
        return crate::material::material_asset_from_json(&document);
    }
    decode_imported_material_document(bytes)
}

/// Decodes one pinned imported-material document into its resolved parameter-level
/// [`MaterialAsset`] — the decode mirror of [`imported_material_document`]. The embedded
/// texture payloads are cook inputs (coverage/atlas derivation) and are skipped; the
/// runtime material carries the factors, blend axis, and sidedness.
fn decode_imported_material_document(bytes: &[u8]) -> Result<MaterialAsset> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/imported-plant-material/v1")?;
    let _name = reader.read_string()?;
    let read_f32 =
        |reader: &mut SectionReader<'_>| -> Result<f32> { Ok(f32::from_bits(reader.read_u32()?)) };
    let base_color = [
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
    ];
    let metallic = read_f32(&mut reader)?;
    let roughness = read_f32(&mut reader)?;
    let emissive = [
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
        read_f32(&mut reader)?,
    ];
    let emissive_strength = read_f32(&mut reader)?;
    let alpha_cutoff = read_f32(&mut reader)?;
    let blend = match reader.read_u8()? {
        0 => "opaque",
        1 => "masked",
        2 => "translucent",
        _ => {
            return Err(Error::Io(
                "imported plant material alpha mode is unknown".to_owned(),
            ));
        }
    };
    let double_sided = reader.read_u8()? != 0;
    for _ in 0..5 {
        if reader.read_u8()? != 0 {
            let _ext = reader.read_string()?;
            let _payload = reader.read_bytes()?;
        }
    }
    Ok(MaterialAsset {
        blend: blend.to_owned(),
        double_sided,
        base_color: saffron_geometry::glam::Vec4::from_array(base_color),
        metallic,
        roughness,
        emissive: saffron_geometry::glam::Vec3::from_array(emissive),
        emissive_strength,
        alpha_cutoff,
        ..MaterialAsset::default()
    })
}

fn material_coverage(material: &MaterialAsset) -> (AlphaClassification, CoverageSource) {
    match &material.surface {
        MaterialSurface::ThinSheetFoliage(parameters) => (
            parameters.coverage.classification,
            parameters.coverage_source,
        ),
        MaterialSurface::Standard => match material.blend.as_str() {
            "masked" => (AlphaClassification::Masked, CoverageSource::AlbedoAlpha),
            "translucent" => (
                AlphaClassification::Transmissive,
                CoverageSource::AlbedoAlpha,
            ),
            _ => (AlphaClassification::Opaque, CoverageSource::ModeledGeometry),
        },
    }
}

fn imported_material_coverage(
    material: &ImportedMaterial,
) -> (AlphaClassification, CoverageSource) {
    match material.alpha_mode {
        AlphaMode::Opaque => (AlphaClassification::Opaque, CoverageSource::ModeledGeometry),
        AlphaMode::Mask => (AlphaClassification::Masked, CoverageSource::AlbedoAlpha),
        AlphaMode::Blend => (
            AlphaClassification::Transmissive,
            CoverageSource::AlbedoAlpha,
        ),
    }
}

fn material_texture_ids(material: &MaterialAsset) -> Vec<Uuid> {
    let mut textures = vec![
        material.albedo_texture,
        material.orm_texture,
        material.normal_texture,
        material.emissive_texture,
        material.height_texture,
        material.vector_displacement_texture,
    ];
    if let MaterialSurface::ThinSheetFoliage(parameters) = &material.surface
        && let CoverageSource::Texture(texture) = parameters.coverage_source
    {
        textures.push(texture);
    }
    textures.retain(|texture| texture.value() != 0);
    textures
}

/// Reports what a source file states about its own origin when the authored provenance does not
/// already carry it.
///
/// A studio's plant export usually names the tool that wrote it, and some tools' licences require
/// attribution. The file's statement is surfaced verbatim rather than folded into the authored
/// provenance: filling in a licence the engine inferred would put a legal claim in the artifact that
/// nobody authored.
fn attribution_notice(
    source: &PlantSourceReference,
    origin: &saffron_geometry::ImportedOrigin,
) -> Option<PlantCompileDiagnostic> {
    if origin.generator.is_empty() && origin.copyright.is_empty() {
        return None;
    }
    let stated = !origin.copyright.is_empty();
    let recorded = !source.provenance.attribution.trim().is_empty();
    if recorded || !stated {
        return None;
    }
    Some(PlantCompileDiagnostic {
        severity: PlantCompileDiagnosticSeverity::Warning,
        code: PlantCompileDiagnosticCode::MissingSource,
        source: Some(source.id),
        selector: Some(source.selector.clone()),
        path: "source.imported.sources.provenance.attribution".to_owned(),
        message: format!(
            "source file states copyright '{}' from generator '{}' and the plant source records no attribution",
            origin.copyright, origin.generator
        ),
    })
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

fn source_snapshot_hash(snapshot: &PlantSourceSnapshot) -> [u8; 32] {
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

fn plant_dependencies(
    asset: &PlantFamilyAsset,
    snapshots: &[PlantSourceSnapshot],
    material_documents: &BTreeMap<u64, Vec<u8>>,
) -> Result<Vec<CookDependency>> {
    let snapshots = snapshots
        .iter()
        .map(|snapshot| (snapshot.source, snapshot))
        .collect::<BTreeMap<_, _>>();
    let mut dependencies = BTreeMap::<Vec<u8>, CookDependency>::new();
    insert_dependency(
        &mut dependencies,
        CookDependency {
            address: CookDependencyAddress::SourceAsset { asset: asset.id },
            content_hash: ContentHash::of(&write_plant_asset(asset)?),
            bounds: None,
            halo: DecisionScalar::from_bits(0),
            ancestor_level: None,
        },
    )?;
    if let PlantFamilySource::Imported(recipe) = &asset.source {
        for source in &recipe.sources {
            let Some(snapshot) = snapshots.get(&source.id) else {
                continue;
            };
            let address = match &source.locator {
                PlantSourceLocator::Asset(asset) => {
                    CookDependencyAddress::SourceAsset { asset: *asset }
                }
                PlantSourceLocator::File(uri) => {
                    CookDependencyAddress::SourceFile { uri: uri.clone() }
                }
            };
            insert_dependency(
                &mut dependencies,
                CookDependency {
                    address,
                    content_hash: ContentHash::new(snapshot.content_hash),
                    bounds: None,
                    halo: DecisionScalar::from_bits(0),
                    ancestor_level: None,
                },
            )?;
        }
    }
    for (material, document) in material_documents {
        insert_dependency(
            &mut dependencies,
            CookDependency {
                address: CookDependencyAddress::MaterialCoverage {
                    material: Uuid(*material),
                },
                content_hash: ContentHash::of(document),
                bounds: None,
                halo: DecisionScalar::from_bits(0),
                ancestor_level: None,
            },
        )?;
    }
    for (namespace, hash) in [
        (
            "plant-asset-schema-v4",
            ContentHash::new(plant_asset_schema_hash()),
        ),
        (
            "plant-compiled-artifact-schema-v2",
            plant_compiled_artifact_schema_hash(),
        ),
        (
            "plant-source-compiler-v2",
            ContentHash::of(&PLANT_SOURCE_COMPILER_VERSION.to_be_bytes()),
        ),
    ] {
        insert_dependency(
            &mut dependencies,
            CookDependency {
                address: CookDependencyAddress::Contract {
                    namespace: namespace.to_owned(),
                },
                content_hash: hash,
                bounds: None,
                halo: DecisionScalar::from_bits(0),
                ancestor_level: None,
            },
        )?;
    }
    Ok(dependencies.into_values().collect())
}

fn insert_dependency(
    dependencies: &mut BTreeMap<Vec<u8>, CookDependency>,
    dependency: CookDependency,
) -> Result<()> {
    let key = dependency.address.canonical_bytes()?;
    if let Some(previous) = dependencies.get(&key) {
        if previous.content_hash != dependency.content_hash {
            return Err(Error::Io(
                "one plant dependency address resolved to conflicting content".to_owned(),
            ));
        }
        return Ok(());
    }
    dependencies.insert(key, dependency);
    Ok(())
}

fn calculate_plant_cook_key(
    family: Uuid,
    dependencies: &[CookDependency],
    options: &PlantRecookOptions,
) -> Result<ContentHash> {
    let record = CookNodeRecord {
        address: CookNodeAddress::Plant { family },
        cook_key: ContentHash::default(),
        output_hash: ContentHash::of(b"plant-cook-key-placeholder"),
        dependencies: dependencies.to_vec(),
        estimate: CookWorkEstimate::default(),
        actual: CookWorkActual::default(),
    };
    Ok(record.calculate_cook_key(options.versions, &options.platform)?)
}

fn build_plant_sections(
    asset: &PlantFamilyAsset,
    compile: &PlantCompileOutput,
    material_documents: &BTreeMap<u64, Vec<u8>>,
    hierarchy_materials: &[VirtualHierarchyMaterial],
) -> Result<Vec<PlantCompiledSection>> {
    let family = compile
        .family
        .as_ref()
        .ok_or_else(|| Error::Io("plant compiler produced no publishable family".to_owned()))?;
    let mut accepted_compile = compile.clone();
    accepted_compile.source_updates.clear();
    accepted_compile
        .diagnostics
        .retain(|diagnostic| diagnostic.code != PlantCompileDiagnosticCode::SourceChanged);
    let hierarchy_input = plant_hierarchy_input(asset, family, hierarchy_materials)?;
    let hierarchy = cook_portable_virtual_hierarchy(&hierarchy_input)?;
    Ok(vec![
        PlantCompiledSection::new(
            PlantCompiledSectionKind::SourceNormalization,
            source_normalization_section(asset, compile, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::PartTable,
            part_table_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Geometry,
            mesh_section(family, PlantSourceRole::Geometry, true),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::MaterialsCoverage,
            material_section(family, material_documents),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::SkeletonWeights,
            skeleton_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Phenotypes,
            phenotype_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Collision,
            collision_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Navigation,
            navigation_section(asset, family),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Provenance,
            provenance_section(asset),
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::TriangleHierarchy,
            hierarchy.triangle_hierarchy_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::VoxelHierarchy,
            hierarchy.voxel_hierarchy_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Deformation,
            hierarchy.deformation_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::PageDirectory,
            hierarchy.page_directory_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::RayTracing,
            hierarchy.ray_tracing_bytes()?,
        ),
        PlantCompiledSection::new(
            PlantCompiledSectionKind::Validation,
            validation_section(&accepted_compile),
        ),
    ])
}

fn validate_complete_plant_artifact(
    bytes: &[u8],
    family: Uuid,
    cook_key: ContentHash,
    platform: ContentHash,
) -> Result<()> {
    let index = PlantCompiledArtifactIndex::open(
        bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
    if index.family != family || index.cook_key != cook_key || index.platform_profile != platform {
        return Err(Error::Io(
            "compiled plant artifact header does not match its cook request".to_owned(),
        ));
    }
    index.family_tags(bytes)?;
    for kind in PlantCompiledSectionKind::ALL {
        if index
            .section(bytes, kind)?
            .is_none_or(|section| section.is_empty())
        {
            return Err(Error::Io(format!(
                "compiled plant artifact is missing {kind:?}"
            )));
        }
    }
    let triangle = index
        .section(bytes, PlantCompiledSectionKind::TriangleHierarchy)?
        .ok_or_else(|| Error::Io("compiled plant triangle hierarchy is missing".to_owned()))?;
    let voxel = index
        .section(bytes, PlantCompiledSectionKind::VoxelHierarchy)?
        .ok_or_else(|| Error::Io("compiled plant voxel hierarchy is missing".to_owned()))?;
    let deformation = index
        .section(bytes, PlantCompiledSectionKind::Deformation)?
        .ok_or_else(|| Error::Io("compiled plant deformation is missing".to_owned()))?;
    let pages = index
        .section(bytes, PlantCompiledSectionKind::PageDirectory)?
        .ok_or_else(|| Error::Io("compiled plant page directory is missing".to_owned()))?;
    let ray_tracing = index
        .section(bytes, PlantCompiledSectionKind::RayTracing)?
        .ok_or_else(|| Error::Io("compiled plant RT metadata is missing".to_owned()))?;
    decode_portable_virtual_hierarchy_sections(
        triangle.as_ref(),
        voxel.as_ref(),
        deformation.as_ref(),
        pages.as_ref(),
        ray_tracing.as_ref(),
    )?;
    Ok(())
}

fn source_normalization_section(
    asset: &PlantFamilyAsset,
    compile: &PlantCompileOutput,
    family: &NormalizedPlantFamily,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/source-normalization/v1");
    append_u64(&mut bytes, asset.id.value());
    bytes.extend_from_slice(&compile.family_hash.unwrap_or_default());
    append_u64(&mut bytes, family.sources.len() as u64);
    let hashes = family.sources.iter().copied().collect::<BTreeMap<_, _>>();
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            bytes.push(0);
            for source in &recipe.sources {
                bytes.extend_from_slice(&source.id.to_be_bytes());
                bytes.push(source_role_tag(source.role));
                append_selector(&mut bytes, &source.selector);
                bytes.extend_from_slice(
                    hashes
                        .get(&source.id)
                        .copied()
                        .unwrap_or([0; 32])
                        .as_slice(),
                );
                append_import_settings(&mut bytes, &source.settings);
            }
        }
        PlantFamilySource::Native { graph, .. } => {
            bytes.push(1);
            let source = native_plant_source_id(asset.id);
            bytes.extend_from_slice(&source.to_be_bytes());
            bytes.extend_from_slice(&graph.identity().bytes());
            bytes.extend_from_slice(hashes.get(&source).copied().unwrap_or([0; 32]).as_slice());
        }
    }
    bytes
}

fn part_table_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/part-table/v2");
    append_u64(&mut bytes, asset.tags.len() as u64);
    for tag in &asset.tags {
        append_u64(&mut bytes, tag.value());
    }
    append_u64(&mut bytes, asset.parts.len() as u64);
    for part in &asset.parts {
        bytes.extend_from_slice(&part.id.to_be_bytes());
        append_optional_u128(&mut bytes, part.parent);
        bytes.push(part_semantic_tag(part.semantic));
        append_u32(&mut bytes, part.material_slot);
        append_u64(&mut bytes, part.sources.len() as u64);
        for source in &part.sources {
            bytes.extend_from_slice(&source.to_be_bytes());
        }
    }
    append_dimensions(&mut bytes, asset.dimensions);
    for value in [
        asset.mechanics.stiffness,
        asset.mechanics.drag,
        asset.mechanics.flutter,
        asset.mechanics.damage_threshold,
        asset.mechanics.break_threshold,
    ] {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
    bytes.extend_from_slice(&asset.mechanics.damping.bits().to_be_bytes());
    bytes.extend_from_slice(&asset.mechanics.bend_limit.bits().to_be_bytes());
    append_u64(&mut bytes, asset.variations.len() as u64);
    for variation in &asset.variations {
        append_u32(&mut bytes, variation.id);
        append_string(&mut bytes, &variation.name);
        append_u128_list(&mut bytes, &variation.sources);
        append_u128_list(&mut bytes, &variation.active_parts);
    }
    append_u32(&mut bytes, asset.interaction_policy as u32);
    match &asset.habitat {
        Some(habitat) => {
            bytes.push(1);
            append_u64(&mut bytes, habitat.fields.len() as u64);
            for (channel, minimum, maximum) in &habitat.fields {
                append_field_channel(&mut bytes, *channel);
                bytes.extend_from_slice(&minimum.bits().to_be_bytes());
                bytes.extend_from_slice(&maximum.bits().to_be_bytes());
            }
            append_u64(&mut bytes, habitat.surface_tags.len() as u64);
            for tag in &habitat.surface_tags {
                append_u64(&mut bytes, *tag);
            }
            bytes.extend_from_slice(&habitat.shade_tolerance.bits().to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes
}

fn mesh_section(
    family: &NormalizedPlantFamily,
    role: PlantSourceRole,
    include_materials: bool,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/mesh-facet/v1");
    let meshes = family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == role)
        .collect::<Vec<_>>();
    append_u64(&mut bytes, meshes.len() as u64);
    for mesh in meshes {
        append_normalized_mesh(&mut bytes, mesh, include_materials, false);
    }
    bytes
}

fn material_section(family: &NormalizedPlantFamily, documents: &BTreeMap<u64, Vec<u8>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/materials-coverage/v1");
    append_u64(&mut bytes, family.materials.len() as u64);
    for material in &family.materials {
        append_u64(&mut bytes, material.material.value());
        bytes.extend_from_slice(&material.content_hash);
        append_bytes(
            &mut bytes,
            documents
                .get(&material.material.value())
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
    }
    bytes
}

fn skeleton_section(asset: &PlantFamilyAsset, family: &NormalizedPlantFamily) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/skeleton-weights/v1");
    append_u64(&mut bytes, family.joints.len() as u64);
    for joint in &family.joints {
        bytes.extend_from_slice(&joint.source.to_be_bytes());
        append_selector(&mut bytes, &joint.selector);
        append_optional_selector(&mut bytes, joint.parent.as_ref());
        for value in joint.transform_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    let skinned = family
        .meshes
        .iter()
        .filter(|mesh| !mesh.skin.is_empty())
        .collect::<Vec<_>>();
    append_u64(&mut bytes, skinned.len() as u64);
    for mesh in skinned {
        bytes.extend_from_slice(&mesh.source.to_be_bytes());
        append_selector(&mut bytes, &mesh.selector);
        append_u64(&mut bytes, mesh.skin.len() as u64);
        for skin in &mesh.skin {
            for joint in skin.joints {
                bytes.extend_from_slice(&joint.to_be_bytes());
            }
            for weight in skin.weights {
                bytes.extend_from_slice(&weight.to_be_bytes());
            }
        }
    }
    append_u64(&mut bytes, asset.spines.len() as u64);
    for spine in &asset.spines {
        bytes.extend_from_slice(&spine.id.to_be_bytes());
        bytes.extend_from_slice(&spine.part.to_be_bytes());
        append_optional_u128(&mut bytes, spine.parent);
        append_u64(&mut bytes, spine.rest_points.len() as u64);
        for (point, radius) in spine.rest_points.iter().zip(&spine.radii) {
            for value in point {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
    }
    bytes
}

fn phenotype_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/phenotypes/v1");
    append_u64(&mut bytes, asset.phenotypes.len() as u64);
    for phenotype in &asset.phenotypes {
        append_u32(&mut bytes, phenotype.id);
        bytes.push(match phenotype.role {
            saffron_vegetation::PhenotypeRole::Healthy => 0,
            saffron_vegetation::PhenotypeRole::Harvested => 1,
            saffron_vegetation::PhenotypeRole::Damaged => 2,
            saffron_vegetation::PhenotypeRole::Burned => 3,
            saffron_vegetation::PhenotypeRole::Dead => 4,
            saffron_vegetation::PhenotypeRole::Flowering => 5,
            saffron_vegetation::PhenotypeRole::Fruiting => 6,
            saffron_vegetation::PhenotypeRole::Senescent => 7,
            saffron_vegetation::PhenotypeRole::Wet => 8,
        });
        match phenotype.season_window {
            Some((start, end)) => {
                bytes.push(1);
                bytes.extend_from_slice(&start.to_le_bytes());
                bytes.extend_from_slice(&end.to_le_bytes());
            }
            None => bytes.push(0),
        }
        append_u32(&mut bytes, phenotype.variation);
        append_u64(&mut bytes, phenotype.material_remap.len() as u64);
        for (from, to) in &phenotype.material_remap {
            append_u32(&mut bytes, *from);
            append_u32(&mut bytes, *to);
        }
        append_u128_list(&mut bytes, &phenotype.active_parts);
    }
    bytes
}

fn collision_section(asset: &PlantFamilyAsset, family: &NormalizedPlantFamily) -> Vec<u8> {
    let mut bytes = mesh_section(family, PlantSourceRole::Collision, false);
    append_u64(&mut bytes, asset.collision_proxies.len() as u64);
    for proxy in &asset.collision_proxies {
        bytes.extend_from_slice(&proxy.id.to_be_bytes());
        bytes.push(match proxy.shape {
            saffron_vegetation::PlantCollisionShape::Box => 0,
            saffron_vegetation::PlantCollisionShape::Sphere => 1,
            saffron_vegetation::PlantCollisionShape::Capsule => 2,
            saffron_vegetation::PlantCollisionShape::ConvexHull => 3,
        });
        bytes.extend_from_slice(&proxy.part.to_be_bytes());
        for value in proxy.center.into_iter().chain(proxy.dimensions) {
            bytes.extend_from_slice(&value.bits().to_be_bytes());
        }
        bytes.push(u8::from(proxy.breakable));
    }
    bytes
}

fn navigation_section(asset: &PlantFamilyAsset, family: &NormalizedPlantFamily) -> Vec<u8> {
    let mut bytes = mesh_section(family, PlantSourceRole::Navigation, false);
    append_u64(&mut bytes, asset.navigation_proxies.len() as u64);
    for proxy in &asset.navigation_proxies {
        bytes.extend_from_slice(&proxy.id.to_be_bytes());
        append_u64(&mut bytes, proxy.footprint.len() as u64);
        for point in &proxy.footprint {
            for value in point {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
        }
        bytes.extend_from_slice(&proxy.height.bits().to_be_bytes());
        bytes.extend_from_slice(&proxy.cost.bits().to_be_bytes());
    }
    bytes
}

fn provenance_section(asset: &PlantFamilyAsset) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/provenance/v1");
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            bytes.push(0);
            append_u64(&mut bytes, recipe.sources.len() as u64);
            for source in &recipe.sources {
                bytes.extend_from_slice(&source.id.to_be_bytes());
                match &source.locator {
                    PlantSourceLocator::Asset(asset) => {
                        bytes.push(0);
                        append_u64(&mut bytes, asset.value());
                    }
                    PlantSourceLocator::File(uri) => {
                        bytes.push(1);
                        append_string(&mut bytes, uri);
                    }
                }
                bytes.extend_from_slice(&source.content_hash);
                for value in [
                    &source.provenance.source,
                    &source.provenance.source_uri,
                    &source.provenance.license_id,
                    &source.provenance.license_uri,
                    &source.provenance.author,
                    &source.provenance.attribution,
                ] {
                    append_string(&mut bytes, value);
                }
                bytes.push(u8::from(source.provenance.requires_attribution));
            }
        }
        PlantFamilySource::Native { graph, .. } => {
            bytes.push(1);
            append_u64(&mut bytes, 1);
            bytes.extend_from_slice(&native_plant_source_id(asset.id).to_be_bytes());
            bytes.extend_from_slice(&graph.identity().bytes());
            bytes.extend_from_slice(&native_botanical_graph_content_hash(graph));
        }
    }
    bytes
}

fn validation_section(compile: &PlantCompileOutput) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/validation/v1");
    append_u64(&mut bytes, compile.diagnostics.len() as u64);
    for diagnostic in &compile.diagnostics {
        bytes.push(match diagnostic.severity {
            PlantCompileDiagnosticSeverity::Info => 0,
            PlantCompileDiagnosticSeverity::Warning => 1,
            PlantCompileDiagnosticSeverity::Error => 2,
        });
        bytes.push(diagnostic_code_tag(diagnostic.code));
        append_optional_u128(&mut bytes, diagnostic.source);
        append_optional_selector(&mut bytes, diagnostic.selector.as_ref());
        append_string(&mut bytes, &diagnostic.path);
        append_string(&mut bytes, &diagnostic.message);
    }
    for value in [
        compile.statistics.sources,
        compile.statistics.meshes,
        compile.statistics.vertices,
        compile.statistics.indices,
        compile.statistics.joints,
        compile.statistics.materials,
        compile.statistics.rejected,
    ] {
        append_u64(&mut bytes, value);
    }
    append_u64(&mut bytes, compile.source_updates.len() as u64);
    for update in &compile.source_updates {
        bytes.extend_from_slice(&update.source.to_be_bytes());
        bytes.extend_from_slice(&update.previous);
        bytes.extend_from_slice(&update.current);
    }
    append_u64(&mut bytes, compile.conflicts.conflicts.len() as u64);
    for conflict in &compile.conflicts.conflicts {
        bytes.extend_from_slice(&conflict.target.to_be_bytes());
        bytes.extend_from_slice(&conflict.source.to_be_bytes());
        append_selector(&mut bytes, &conflict.selector);
        append_destination(&mut bytes, conflict.destination);
        bytes.push(match conflict.reason {
            PlantReimportConflictReason::MissingSource => 0,
            PlantReimportConflictReason::MissingElement => 1,
        });
    }
    bytes
}

fn append_normalized_mesh(
    bytes: &mut Vec<u8>,
    mesh: &NormalizedPlantMesh,
    include_materials: bool,
    include_skin: bool,
) {
    bytes.extend_from_slice(&mesh.source.to_be_bytes());
    append_selector(bytes, &mesh.selector);
    append_u64(bytes, mesh.vertices.len() as u64);
    for vertex in &mesh.vertices {
        for value in vertex.position_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.normal_snorm {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.uv_bits {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in vertex.tangent_snorm {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    append_u64(bytes, mesh.indices.len() as u64);
    for index in &mesh.indices {
        append_u32(bytes, *index);
    }
    if include_materials {
        append_u64(bytes, mesh.submeshes.len() as u64);
        for submesh in &mesh.submeshes {
            append_u32(bytes, submesh.first_index);
            append_u32(bytes, submesh.index_count);
            append_u32(bytes, submesh.material_slot);
        }
    } else {
        append_u64(bytes, 0);
    }
    if include_skin {
        append_u64(bytes, mesh.skin.len() as u64);
        for skin in &mesh.skin {
            for joint in skin.joints {
                bytes.extend_from_slice(&joint.to_be_bytes());
            }
            for weight in skin.weights {
                bytes.extend_from_slice(&weight.to_be_bytes());
            }
        }
    } else {
        append_u64(bytes, 0);
    }
}

fn append_import_settings(bytes: &mut Vec<u8>, settings: &PlantImportSettings) {
    bytes.push(match settings.units {
        SourceUnits::Meters => 0,
        SourceUnits::Centimeters => 1,
        SourceUnits::Millimeters => 2,
        SourceUnits::Feet => 3,
    });
    bytes.push(axis_tag(settings.up_axis));
    bytes.push(axis_tag(settings.forward_axis));
    bytes.push(match settings.handedness {
        SourceHandedness::Right => 0,
        SourceHandedness::Left => 1,
    });
    bytes.extend_from_slice(&settings.scale.bits().to_be_bytes());
    match &settings.pivot {
        PlantPivot::SourceOrigin => bytes.push(0),
        PlantPivot::BoundsBaseCenter => bytes.push(1),
        PlantPivot::Explicit(position) => {
            bytes.push(2);
            for value in position {
                bytes.extend_from_slice(&value.bits().to_be_bytes());
            }
        }
        PlantPivot::SemanticPart(part) => {
            bytes.push(3);
            bytes.extend_from_slice(&part.to_be_bytes());
        }
    }
    bytes.push(match settings.winding {
        SourceWinding::CounterClockwise => 0,
        SourceWinding::Clockwise => 1,
    });
    bytes.push(match settings.uv_origin {
        SourceUvOrigin::TopLeft => 0,
        SourceUvOrigin::BottomLeft => 1,
    });
    for value in settings.uv_scale.into_iter().chain(settings.uv_offset) {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
    bytes.push(match settings.tangent_policy {
        PlantTangentPolicy::Require => 0,
        PlantTangentPolicy::GenerateMissing => 1,
        PlantTangentPolicy::Regenerate => 2,
    });
}

fn append_dimensions(bytes: &mut Vec<u8>, dimensions: saffron_vegetation::PlantDimensions) {
    for value in [dimensions.height, dimensions.trunk_radius]
        .into_iter()
        .chain(dimensions.crown_radius)
        .chain(dimensions.root_radius)
        .chain(dimensions.local_bounds_min)
        .chain(dimensions.local_bounds_max)
    {
        bytes.extend_from_slice(&value.bits().to_be_bytes());
    }
}

fn append_field_channel(bytes: &mut Vec<u8>, channel: saffron_spatial::FieldChannel) {
    let (tag, user) = channel.canonical_code();
    bytes.push(tag);
    append_u64(bytes, user);
}

fn append_destination(bytes: &mut Vec<u8>, destination: PlantSemanticDestination) {
    match destination {
        PlantSemanticDestination::Part(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::Spine(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::MaterialSlot(slot) => {
            bytes.push(2);
            append_u32(bytes, slot);
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            bytes.push(3);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            bytes.push(4);
            bytes.extend_from_slice(&id.to_be_bytes());
        }
        PlantSemanticDestination::Phenotype(id) => {
            bytes.push(5);
            append_u32(bytes, id);
        }
    }
}

fn append_selector(bytes: &mut Vec<u8>, selector: &PlantSourceSelector) {
    match selector {
        PlantSourceSelector::Whole => bytes.push(0),
        PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            append_string(bytes, path);
        }
        PlantSourceSelector::Submesh { element, index } => {
            bytes.push(2);
            bytes.extend_from_slice(&element.to_be_bytes());
            append_u32(bytes, *index);
        }
    }
}

fn append_optional_selector(bytes: &mut Vec<u8>, selector: Option<&PlantSourceSelector>) {
    match selector {
        Some(selector) => {
            bytes.push(1);
            append_selector(bytes, selector);
        }
        None => bytes.push(0),
    }
}

fn append_skin(bytes: &mut Vec<u8>, skin: &[VertexSkin]) {
    append_u64(bytes, skin.len() as u64);
    for skin in skin {
        for joint in skin.joints {
            bytes.extend_from_slice(&joint.to_be_bytes());
        }
        for weight in skin.weights {
            append_u32(bytes, weight.to_bits());
        }
    }
}

fn append_json(bytes: &mut Vec<u8>, value: &saffron_json::Value) {
    append_bytes(bytes, saffron_json::dump_json_sorted(value, -1).as_bytes());
}

/// One decoded Geometry-section mesh row — the prototype-order source of the family's
/// flattened render vertex stream. The decode mirrors [`append_normalized_mesh`]
/// field-for-field so the two stay in lockstep.
pub(crate) struct PlantGeometryMesh {
    /// Recipe source identity.
    pub source: u128,
    /// Exact selected source element or submesh.
    pub selector: PlantSourceSelector,
    /// Quantized family-local vertices.
    pub vertices: Vec<saffron_vegetation::NormalizedPlantVertex>,
    /// Prototype-local triangle indices.
    pub indices: Vec<u32>,
    /// Material-homogeneous draw ranges.
    pub submeshes: Vec<saffron_vegetation::NormalizedPlantSubmesh>,
}

/// One decoded MaterialsCoverage-section row: the material identity plus its resolved
/// `.smat` document bytes.
pub(crate) struct PlantMaterialRow {
    /// The referenced material asset identity.
    pub material: Uuid,
    /// The resolved `.smat` JSON document.
    pub document: Vec<u8>,
}

/// A big-endian, length-prefixed section reader — the decode mirror of the `append_*`
/// writers above.
struct SectionReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SectionReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::Io("plant section payload is truncated".to_owned()))?;
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn read_u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(
            self.take(16)?.try_into().expect("16 bytes"),
        ))
    }

    fn read_length(&mut self) -> Result<usize> {
        let value = self.read_u64()?;
        usize::try_from(value)
            .ok()
            .filter(|count| *count <= self.bytes.len())
            .ok_or_else(|| Error::Io("plant section length exceeds its payload".to_owned()))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let count = self.read_length()?;
        self.take(count)
    }

    fn read_string(&mut self) -> Result<String> {
        String::from_utf8(self.read_bytes()?.to_vec())
            .map_err(|_| Error::Io("plant section string is not UTF-8".to_owned()))
    }

    fn read_selector(&mut self) -> Result<PlantSourceSelector> {
        match self.read_u8()? {
            0 => Ok(PlantSourceSelector::Whole),
            1 => Ok(PlantSourceSelector::Element {
                id: self.read_u128()?,
                path: self.read_string()?,
            }),
            2 => Ok(PlantSourceSelector::Submesh {
                element: self.read_u128()?,
                index: self.read_u32()?,
            }),
            _ => Err(Error::Io(
                "plant section selector tag is unknown".to_owned(),
            )),
        }
    }

    fn expect_domain(&mut self, domain: &[u8]) -> Result<()> {
        if self.read_bytes()? != domain {
            return Err(Error::Io(
                "plant section domain does not match its kind".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Decodes one Geometry-section payload into its prototype-ordered mesh rows.
pub(crate) fn decode_mesh_section(bytes: &[u8]) -> Result<Vec<PlantGeometryMesh>> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/mesh-facet/v1")?;
    let mesh_count = reader.read_length()?;
    let mut meshes = Vec::with_capacity(mesh_count);
    for _ in 0..mesh_count {
        let source = reader.read_u128()?;
        let selector = reader.read_selector()?;
        let vertex_count = reader.read_length()?;
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            let mut vertex = saffron_vegetation::NormalizedPlantVertex::default();
            for value in &mut vertex.position_bits {
                *value = reader.read_i32()?;
            }
            for value in &mut vertex.normal_snorm {
                *value = reader.read_i16()?;
            }
            for value in &mut vertex.uv_bits {
                *value = reader.read_i32()?;
            }
            for value in &mut vertex.tangent_snorm {
                *value = reader.read_i16()?;
            }
            vertices.push(vertex);
        }
        let index_count = reader.read_length()?;
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(reader.read_u32()?);
        }
        let submesh_count = reader.read_length()?;
        let mut submeshes = Vec::with_capacity(submesh_count);
        for _ in 0..submesh_count {
            submeshes.push(saffron_vegetation::NormalizedPlantSubmesh {
                first_index: reader.read_u32()?,
                index_count: reader.read_u32()?,
                material_slot: reader.read_u32()?,
            });
        }
        let skin_count = reader.read_length()?;
        for _ in 0..skin_count {
            // 4 joint u16s + 4 weight u16s per record; the render loader has no use for
            // them (structural deformation binds through the skeleton section).
            reader.take(16)?;
        }
        meshes.push(PlantGeometryMesh {
            source,
            selector,
            vertices,
            indices,
            submeshes,
        });
    }
    Ok(meshes)
}

/// One decoded phenotype row: the identity, its variation, and the material remap.
pub(crate) struct PlantPhenotypeRow {
    /// Stable family-local phenotype identity.
    pub id: u32,
    /// Semantic role.
    pub role: saffron_vegetation::PhenotypeRole,
    /// Authored seasonal window in per-mille of the year, wrapping through 1000.
    pub season_window: Option<(u16, u16)>,
    /// The variation the phenotype renders.
    pub variation: u32,
    /// Material slot remap `(from, to)`.
    pub material_remap: Vec<(u32, u32)>,
}

/// Decodes one Phenotypes-section payload — the decode mirror of
/// [`phenotype_section`]. Active-part sets are cook inputs (baked into the assembly
/// masks) and are skipped.
pub(crate) fn decode_phenotype_section(bytes: &[u8]) -> Result<Vec<PlantPhenotypeRow>> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/phenotypes/v1")?;
    let count = reader.read_length()?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let id = reader.read_u32()?;
        let role = match reader.read_u8()? {
            0 => saffron_vegetation::PhenotypeRole::Healthy,
            1 => saffron_vegetation::PhenotypeRole::Harvested,
            2 => saffron_vegetation::PhenotypeRole::Damaged,
            3 => saffron_vegetation::PhenotypeRole::Burned,
            4 => saffron_vegetation::PhenotypeRole::Dead,
            5 => saffron_vegetation::PhenotypeRole::Flowering,
            6 => saffron_vegetation::PhenotypeRole::Fruiting,
            7 => saffron_vegetation::PhenotypeRole::Senescent,
            8 => saffron_vegetation::PhenotypeRole::Wet,
            other => {
                return Err(Error::Io(format!(
                    "compiled plant phenotype role {other} is unknown"
                )));
            }
        };
        let season_window = match reader.read_u8()? {
            0 => None,
            1 => {
                let start = u16::from_le_bytes(reader.take(2)?.try_into().expect("two bytes"));
                let end = u16::from_le_bytes(reader.take(2)?.try_into().expect("two bytes"));
                Some((start, end))
            }
            other => {
                return Err(Error::Io(format!(
                    "compiled plant phenotype window flag {other} is unknown"
                )));
            }
        };
        let variation = reader.read_u32()?;
        let remap_count = reader.read_length()?;
        let mut material_remap = Vec::with_capacity(remap_count);
        for _ in 0..remap_count {
            material_remap.push((reader.read_u32()?, reader.read_u32()?));
        }
        let active_count = reader.read_length()?;
        for _ in 0..active_count {
            reader.take(16)?;
        }
        rows.push(PlantPhenotypeRow {
            id,
            role,
            season_window,
            variation,
            material_remap,
        });
    }
    Ok(rows)
}

/// Decodes one MaterialsCoverage-section payload into its material rows.
pub(crate) fn decode_material_section(bytes: &[u8]) -> Result<Vec<PlantMaterialRow>> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/materials-coverage/v1")?;
    let material_count = reader.read_length()?;
    let mut materials = Vec::with_capacity(material_count);
    for _ in 0..material_count {
        let material = Uuid(reader.read_u64()?);
        // The 32-byte content hash pins the resolved document; the loader trusts the
        // artifact's own validation and keeps only the document.
        reader.take(32)?;
        let document = reader.read_bytes()?.to_vec();
        materials.push(PlantMaterialRow { material, document });
    }
    Ok(materials)
}

fn append_domain(bytes: &mut Vec<u8>, domain: &[u8]) {
    append_bytes(bytes, domain);
}

fn append_string(bytes: &mut Vec<u8>, value: &str) {
    append_bytes(bytes, value.as_bytes());
}

fn append_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    append_u64(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
}

fn append_u128_list(bytes: &mut Vec<u8>, values: &[u128]) {
    append_u64(bytes, values.len() as u64);
    for value in values {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
}

fn append_optional_u128(bytes: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn append_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn append_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn source_role_tag(role: PlantSourceRole) -> u8 {
    match role {
        PlantSourceRole::Geometry => 0,
        PlantSourceRole::Material => 1,
        PlantSourceRole::Skeleton => 2,
        PlantSourceRole::Collision => 3,
        PlantSourceRole::Navigation => 4,
    }
}

fn axis_tag(axis: SourceAxis) -> u8 {
    match axis {
        SourceAxis::PositiveX => 0,
        SourceAxis::NegativeX => 1,
        SourceAxis::PositiveY => 2,
        SourceAxis::NegativeY => 3,
        SourceAxis::PositiveZ => 4,
        SourceAxis::NegativeZ => 5,
    }
}

fn part_semantic_tag(semantic: PlantPartSemantic) -> u8 {
    match semantic {
        PlantPartSemantic::Trunk => 0,
        PlantPartSemantic::Branch => 1,
        PlantPartSemantic::Root => 2,
        PlantPartSemantic::Frond => 3,
        PlantPartSemantic::Leaf => 4,
        PlantPartSemantic::Flower => 5,
        PlantPartSemantic::Fruit => 6,
        PlantPartSemantic::Blade => 7,
    }
}

fn diagnostic_code_tag(code: PlantCompileDiagnosticCode) -> u8 {
    match code {
        PlantCompileDiagnosticCode::MissingSource => 0,
        PlantCompileDiagnosticCode::DuplicateSource => 1,
        PlantCompileDiagnosticCode::EmptySelection => 2,
        PlantCompileDiagnosticCode::InvalidGeometry => 3,
        PlantCompileDiagnosticCode::MissingMaterial => 4,
        PlantCompileDiagnosticCode::InvalidMaterial => 5,
        PlantCompileDiagnosticCode::InvalidSkeleton => 6,
        PlantCompileDiagnosticCode::MissingCoverageUv => 7,
        PlantCompileDiagnosticCode::InvalidLeafOrientation => 8,
        PlantCompileDiagnosticCode::BoundsMismatch => 9,
        PlantCompileDiagnosticCode::LimitExceeded => 10,
        PlantCompileDiagnosticCode::SourceChanged => 11,
        PlantCompileDiagnosticCode::OrphanedEdit => 12,
    }
}

const _: () = assert!(PLANT_ASSET_VERSION == 4);
const _: () = assert!(PLANT_COMPILED_ARTIFACT_VERSION == 2);

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{Mesh, Submesh, Vertex, compute_tangents, save_mesh_to_buffer};
    use saffron_scene::{AssetEntry, AssetType};
    use saffron_spatial::UnitInterval;
    use saffron_vegetation::{
        BotanicalGraphDocument, CookVersionSet, ImportedPlantFamilyRecipe, InteractionPolicy,
        MechanicalResponse, PhenotypeRole, PlantDimensions, PlantFamilySource,
        PlantManualSemanticTarget, PlantPart, PlantPhenotype, PlantReimportConflictReason,
        PlantSourceLocator, PlantSourceReference, PlantVariation, SourceProvenance,
        ThinSheetFoliageParameters, VoxelMaterialMoments,
    };

    use super::*;
    use crate::{
        MaterialAsset, load_plant_family_asset, save_material_asset, save_plant_family_asset,
    };

    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Scratch {
        project: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let project = std::env::temp_dir().join(format!(
                "saffron-plant-cook-{tag}-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(project.join("assets")).expect("create scratch assets");
            Self { project }
        }

        fn assets(&self) -> AssetServer {
            AssetServer::new(self.project.join("assets"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.project);
        }
    }

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).expect("fixture scalar")
    }

    fn triangle() -> Mesh {
        let mut mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-0.5, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.5, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        compute_tangents(&mut mesh);
        mesh
    }

    fn alpha_card() -> Mesh {
        let mut mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 1.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(-1.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        compute_tangents(&mut mesh);
        mesh
    }

    fn provenance() -> SourceProvenance {
        SourceProvenance {
            source: "fixture".to_owned(),
            source_uri: "https://example.invalid/oak".to_owned(),
            license_id: "CC0-1.0".to_owned(),
            license_uri: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            author: "Fixture".to_owned(),
            attribution: "Oak fixture".to_owned(),
            requires_attribution: false,
        }
    }

    fn base_family(material: Uuid, source: PlantFamilySource) -> PlantFamilyAsset {
        let imported = matches!(source, PlantFamilySource::Imported(_));
        PlantFamilyAsset {
            version: PLANT_ASSET_VERSION,
            id: Uuid(9_000),
            name: "Oak".to_owned(),
            tags: vec![saffron_vegetation::PlantTagId::new(17).unwrap()],
            source,
            parts: vec![PlantPart {
                id: 40,
                parent: None,
                semantic: PlantPartSemantic::Trunk,
                material_slot: 0,
                sources: if imported { vec![10] } else { Vec::new() },
            }],
            dimensions: PlantDimensions {
                height: fixed(2),
                trunk_radius: fixed(1),
                crown_radius: [fixed(1); 2],
                root_radius: [fixed(1); 2],
                local_bounds_min: [fixed(-1), fixed(-1), fixed(-1)],
                local_bounds_max: [fixed(1), fixed(2), fixed(1)],
            },
            material_slots: vec![material],
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(1),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: fixed(1),
                bend_limit: UnitInterval::from_bits(1),
                damage_threshold: fixed(1),
                break_threshold: fixed(2),
            },
            variations: vec![PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: if imported {
                    vec![10, 11]
                } else {
                    vec![saffron_vegetation::native_variation_source_id(0)]
                },
                active_parts: Vec::new(),
            }],
            phenotypes: vec![PlantPhenotype {
                id: 0,
                role: PhenotypeRole::Healthy,
                season_window: None,
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Decorative,
            habitat: None,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        }
    }

    fn imported_family(material: Uuid, mesh: Uuid) -> PlantFamilyAsset {
        let mesh_selector = PlantSourceSelector::Element {
            id: u128::from(mesh.value()),
            path: "oak".to_owned(),
        };
        let material_selector = PlantSourceSelector::Element {
            id: u128::from(material.value()),
            path: "materials/oak".to_owned(),
        };
        base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::Asset(mesh),
                        role: PlantSourceRole::Geometry,
                        selector: mesh_selector.clone(),
                        content_hash: [1; 32],
                        settings: PlantImportSettings {
                            pivot: PlantPivot::SourceOrigin,
                            ..PlantImportSettings::default()
                        },
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::Asset(material),
                        role: PlantSourceRole::Material,
                        selector: material_selector.clone(),
                        content_hash: [2; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: mesh_selector,
                        destination: PlantSemanticDestination::Part(40),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 11,
                        selector: material_selector,
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                ],
            }),
        )
    }

    fn native_family(material: Uuid) -> PlantFamilyAsset {
        base_family(
            material,
            PlantFamilySource::Native {
                graph: BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
        )
    }

    fn fixture_server(tag: &str) -> (Scratch, AssetServer, Uuid, Uuid) {
        fixture_server_with_material(tag, &MaterialAsset::default())
    }

    fn fixture_server_with_material(
        tag: &str,
        material_asset: &MaterialAsset,
    ) -> (Scratch, AssetServer, Uuid, Uuid) {
        let scratch = Scratch::new(tag);
        let mut assets = scratch.assets();
        let material = save_material_asset(&mut assets, material_asset, "Oak material", "plants")
            .expect("save material");
        let mesh = Uuid(7_001);
        let relative = "meshes/oak.smesh";
        std::fs::create_dir_all(assets.root.join("meshes")).expect("create meshes");
        std::fs::write(
            assets.root.join(relative),
            save_mesh_to_buffer(&triangle(), &[], None).expect("encode mesh"),
        )
        .expect("write mesh");
        assets.catalog.put(AssetEntry {
            id: mesh,
            name: "oak".to_owned(),
            asset_type: AssetType::Mesh,
            path: relative.to_owned(),
            ..AssetEntry::default()
        });
        (scratch, assets, material, mesh)
    }

    fn options() -> PlantRecookOptions {
        PlantRecookOptions {
            limits: PlantCompileLimits::default(),
            versions: CookVersionSet::current(),
            platform: CookPlatformProfile {
                target: "test-target".to_owned(),
                content_profile: "portable-vulkan".to_owned(),
                toolchain: "rust-test".to_owned(),
                features: vec!["plant-phase-4".to_owned()],
            },
        }
    }

    fn save_family(assets: &mut AssetServer, family: PlantFamilyAsset) -> PlantFamilyAsset {
        let id = save_plant_family_asset(assets, family, "Oak", "plants").expect("save family");
        load_plant_family_asset(assets, id).expect("reload family")
    }

    #[test]
    fn compiled_part_table_starts_with_canonical_family_tags() {
        let family = native_family(Uuid(8_001));
        let bytes = part_table_section(&family);
        let domain = b"saffron-anima/splantc/part-table/v2";
        let count_offset = 8 + domain.len();
        assert_eq!(
            u64::from_be_bytes(bytes[..8].try_into().unwrap()),
            u64::try_from(domain.len()).unwrap()
        );
        assert_eq!(&bytes[8..count_offset], domain);
        assert_eq!(
            u64::from_be_bytes(bytes[count_offset..count_offset + 8].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_be_bytes(
                bytes[count_offset + 8..count_offset + 16]
                    .try_into()
                    .unwrap()
            ),
            17
        );
    }

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
                width: 4,
                height: 2,
                cutoff: 128,
            },
        )]);
        apply_geometry_first_contours(&mut snapshot, &coverage).unwrap();
        let mesh = &snapshot.meshes[0].mesh;
        assert_eq!(mesh.indices.len(), 6);
        assert!(mesh.vertices.iter().all(|vertex| vertex.uv0.x <= 0.5));
        assert!(mesh.vertices.iter().all(|vertex| vertex.position.x <= 0.0));
    }

    #[test]
    fn missing_manual_selector_reports_conflict_and_publishes_nothing() {
        let (_scratch, mut assets, material, mesh) = fixture_server("missing-selector");
        let mut family = imported_family(material, mesh);
        let PlantFamilySource::Imported(recipe) = &mut family.source else {
            unreachable!();
        };
        recipe.sources[0].selector = PlantSourceSelector::Whole;
        recipe.semantic_targets[0].selector = PlantSourceSelector::Element {
            id: 999_999,
            path: "oak/missing".to_owned(),
        };
        let family = save_family(&mut assets, family);
        let plant_path = assets
            .catalog
            .find(family.id)
            .expect("plant row")
            .path
            .clone();
        let before = std::fs::read(assets.root.join(&plant_path)).expect("authored bytes");
        let outcome =
            recook_plant_family(&mut assets, &family, &options()).expect("recook outcome");
        let PlantRecookOutcome::Rejected(validation) = outcome else {
            panic!("missing selector published");
        };
        assert_eq!(validation.compile.conflicts.conflicts.len(), 1);
        assert_eq!(
            validation.compile.conflicts.conflicts[0].reason,
            PlantReimportConflictReason::MissingElement
        );
        assert!(!assets.vegetation_cache_root.exists());
        let after = std::fs::read(assets.root.join(&plant_path)).expect("authored bytes");
        assert_eq!(before, after);
    }

    #[test]
    fn cache_deletion_rebuilds_byte_identical_plant_artifact() {
        let (_scratch, mut assets, material, mesh) = fixture_server("cache-identity");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let first = recook_plant_family(&mut assets, &family, &options()).expect("first recook");
        let PlantRecookOutcome::Published(first) = first else {
            panic!("valid family was rejected");
        };
        let first_bytes = std::fs::read(&first.publication.path).expect("first artifact");
        std::fs::remove_file(&first.publication.path).expect("delete disposable cache artifact");

        let accepted = load_plant_family_asset(&assets, family.id).expect("accepted family");
        let second =
            recook_plant_family(&mut assets, &accepted, &options()).expect("second recook");
        let PlantRecookOutcome::Published(second) = second else {
            panic!("accepted family was rejected");
        };
        assert!(!second.publication.cache_hit);
        assert_eq!(first.cook_key, second.cook_key);
        assert_eq!(
            first.publication.content_hash,
            second.publication.content_hash
        );
        assert_eq!(
            first_bytes,
            std::fs::read(&second.publication.path).expect("rebuilt artifact")
        );

        let third = recook_plant_family(&mut assets, &accepted, &options()).expect("third recook");
        let PlantRecookOutcome::Published(third) = third else {
            panic!("accepted family was rejected");
        };
        assert!(third.publication.cache_hit);
        assert!(third.work.cache_hit);
    }

    #[test]
    fn native_and_imported_sources_publish_to_the_same_artifact_contract() {
        let (_scratch, mut assets, material, mesh) = fixture_server("source-union");
        let imported = save_family(&mut assets, imported_family(material, mesh));
        let native = save_family(&mut assets, native_family(material));
        let mut observed = Vec::new();
        for family in [&imported, &native] {
            let outcome =
                recook_plant_family(&mut assets, family, &options()).expect("shared recook");
            let PlantRecookOutcome::Published(published) = outcome else {
                panic!("family source variant was rejected");
            };
            assert!(published.validation.compile.publishable());
            assert_eq!(
                published
                    .publication
                    .path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str()),
                Some("plants")
            );
            let bytes = std::fs::read(&published.publication.path).expect("artifact");
            let index = PlantCompiledArtifactIndex::open(
                &bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )
            .expect("artifact index");
            let sections = PlantCompiledSectionKind::ALL
                .into_iter()
                .map(|kind| index.section(&bytes, kind).expect("section").is_some())
                .collect::<Vec<_>>();
            assert!(sections.iter().all(|present| *present));
            observed.push(sections);
        }
        assert_eq!(observed[0], observed[1]);
    }

    /// A native family built the ordinary way — several variations, derived appearances, derived
    /// proxies — publishes through the shared recook, and its hierarchy offers a use combination for
    /// every authored (variation, phenotype) pair.
    #[test]
    fn a_multi_variation_native_family_publishes_with_every_combination() {
        let (_scratch, mut assets, bark, _mesh) = fixture_server("native-variations");
        let leaf = save_material_asset(&mut assets, &MaterialAsset::default(), "Leaf", "plants")
            .expect("save leaf material");
        let mut graph = saffron_vegetation::BotanicalGraphDocument::sapling(0x5a11);
        graph
            .variations
            .push(saffron_vegetation::BotanicalVariation {
                seed: 0x5a11,
                age: saffron_spatial::UnitInterval::from_bits(24_000),
                name: "Sapling".to_owned(),
            });
        let family = saffron_vegetation::native_plant_family(
            Uuid(9_411),
            "Variegated",
            graph,
            vec![bark, leaf],
        )
        .expect("the family builds");
        assert_eq!(family.variations.len(), 2);
        assert!(!family.collision_proxies.is_empty());
        assert!(!family.navigation_proxies.is_empty());
        let expected: Vec<(u32, u32)> = family
            .phenotypes
            .iter()
            .map(|phenotype| (phenotype.variation, phenotype.id))
            .collect();

        let family_slots = family.material_slots.len();
        let saved = save_family(&mut assets, family);
        let outcome = recook_plant_family(&mut assets, &saved, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("a derived native family was rejected");
        };
        assert!(published.validation.compile.publishable());
        // Two variations, two geometry sources, and a use combination per authored pair.
        let compiled = published
            .validation
            .compile
            .family
            .as_ref()
            .expect("a published family");
        assert_eq!(compiled.sources.len(), 1, "one native graph source");
        assert!(compiled.meshes.len() >= 2, "one mesh set per variation");
        let materials: Vec<saffron_geometry::VirtualHierarchyMaterial> = (0..family_slots)
            .map(|slot| saffron_geometry::VirtualHierarchyMaterial::opaque(slot as u32))
            .collect();
        let hierarchy = saffron_vegetation::plant_hierarchy_input(&saved, compiled, &materials)
            .expect("a portable hierarchy input");
        let offered: Vec<(u32, u32)> = hierarchy
            .combinations
            .iter()
            .map(|combination| (combination.variation, combination.phenotype))
            .collect();
        assert_eq!(offered, expected);
    }

    #[test]
    fn portable_hierarchy_round_trip_and_cut_are_hole_free() {
        let (_scratch, mut assets, material, mesh) = fixture_server("portable-hierarchy");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = |kind| {
            index
                .section(&bytes, kind)
                .expect("valid section")
                .expect("required section")
        };
        let triangle = section(PlantCompiledSectionKind::TriangleHierarchy);
        let voxel = section(PlantCompiledSectionKind::VoxelHierarchy);
        let deformation = section(PlantCompiledSectionKind::Deformation);
        let pages = section(PlantCompiledSectionKind::PageDirectory);
        let ray_tracing = section(PlantCompiledSectionKind::RayTracing);
        let hierarchy = decode_portable_virtual_hierarchy_sections(
            triangle.as_ref(),
            voxel.as_ref(),
            deformation.as_ref(),
            pages.as_ref(),
            ray_tracing.as_ref(),
        )
        .expect("portable hierarchy");
        assert!(!hierarchy.triangle_clusters.is_empty());
        assert!(!hierarchy.voxel_bricks.is_empty());
        assert!(hierarchy.pages.iter().all(|page| {
            page.dependency
                .is_none_or(|dependency| dependency < page.id)
        }));

        let root_pages = hierarchy
            .roots
            .iter()
            .map(|root| hierarchy.nodes[*root as usize].page)
            .collect::<BTreeSet<_>>();
        let coarse = saffron_geometry::select_portable_hierarchy_cut(&hierarchy, &root_pages, 0)
            .expect("coarse cut");
        assert_eq!(coarse, hierarchy.roots);

        let all_pages = hierarchy.pages.iter().map(|page| page.id).collect();
        let fine = saffron_geometry::select_portable_hierarchy_cut(&hierarchy, &all_pages, 0)
            .expect("fine cut");
        assert!(!fine.is_empty());
        assert!(fine.iter().all(|node| {
            hierarchy.nodes[*node as usize].children.is_empty()
                || hierarchy.nodes[*node as usize].appearance_error.total == 0
        }));
        assert_eq!(
            hierarchy.triangle_hierarchy_bytes().expect("re-encode"),
            triangle.as_ref()
        );
        assert_eq!(
            hierarchy.voxel_hierarchy_bytes().expect("re-encode"),
            voxel.as_ref()
        );

        let mut corrupt = pages.to_vec();
        corrupt.push(0);
        assert!(saffron_geometry::decode_page_directory(&corrupt).is_err());
    }

    #[test]
    fn render_decode_flattens_the_published_artifact_against_its_prototypes() {
        let (_scratch, mut assets, material, mesh) = fixture_server("render-decode");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let decoded =
            crate::plant_render::decode_plant_render_sections(&bytes).expect("render decode");

        // The flat vertex stream is the prototypes' streams concatenated in id order.
        let vertex_total: usize = decoded
            .hierarchy
            .prototypes
            .iter()
            .map(|prototype| prototype.vertex_count as usize)
            .sum();
        assert_eq!(decoded.mesh.vertices.len(), vertex_total);
        assert!(!decoded.mesh.indices.is_empty());
        assert!(
            decoded
                .mesh
                .indices
                .iter()
                .all(|index| (*index as usize) < decoded.mesh.vertices.len()),
            "flattened indices are stream-global"
        );
        // Every prototype is placed by at least one use, and every use names a live
        // prototype — the assembly upload's contract.
        assert!(!decoded.hierarchy.micro_instances.is_empty());
        assert!(
            decoded
                .hierarchy
                .micro_instances
                .iter()
                .all(|instance| (instance.prototype as usize) < decoded.hierarchy.prototypes.len())
        );
        // The fixture family resolves one material slot; its pinned document decodes to
        // the resolved material asset.
        assert_eq!(decoded.material_slots, vec![material]);
        assert_eq!(decoded.material_documents.len(), 1);
        decode_plant_material_document(&decoded.material_documents[0].1)
            .expect("pinned material resolves");
    }

    /// Extends the imported fixture with a second geometry source (a second mesh asset
    /// mapped to its own part), so the compiled family carries two prototypes and the
    /// runtime load builds a real assembly table.
    fn two_prototype_family(
        assets: &mut AssetServer,
        material: Uuid,
        first_mesh: Uuid,
    ) -> PlantFamilyAsset {
        let second_mesh = Uuid(7_002);
        let relative = "meshes/birch.smesh";
        std::fs::write(
            assets.root.join(relative),
            save_mesh_to_buffer(&triangle(), &[], None).expect("encode mesh"),
        )
        .expect("write mesh");
        assets.catalog.put(AssetEntry {
            id: second_mesh,
            name: "birch".to_owned(),
            asset_type: AssetType::Mesh,
            path: relative.to_owned(),
            ..AssetEntry::default()
        });
        let selector = PlantSourceSelector::Element {
            id: u128::from(second_mesh.value()),
            path: "birch".to_owned(),
        };
        let mut family = imported_family(material, first_mesh);
        let PlantFamilySource::Imported(recipe) = &mut family.source else {
            unreachable!();
        };
        recipe.sources.push(PlantSourceReference {
            id: 12,
            locator: PlantSourceLocator::Asset(second_mesh),
            role: PlantSourceRole::Geometry,
            selector: selector.clone(),
            content_hash: [3; 32],
            settings: PlantImportSettings {
                pivot: PlantPivot::SourceOrigin,
                ..PlantImportSettings::default()
            },
            provenance: provenance(),
        });
        recipe.semantic_targets.push(PlantManualSemanticTarget {
            id: 32,
            source: 12,
            selector,
            destination: PlantSemanticDestination::Part(41),
        });
        family.parts.push(PlantPart {
            id: 41,
            parent: Some(40),
            semantic: PlantPartSemantic::Leaf,
            material_slot: 0,
            sources: vec![12],
        });
        family.variations[0].sources.push(12);
        family
    }

    /// The full runtime seam: a published two-prototype family loads from the artifact
    /// store into an assembly-carrying `GpuMesh`, registered under the family id in the
    /// shared mesh + page-payload caches with its material slot table. Skips when no
    /// Vulkan device is present.
    #[test]
    fn published_family_loads_as_an_assembly_mesh_under_the_family_id() {
        use saffron_rendering::{
            BindlessFreeList, Descriptors, Device, SurfaceSource, Uploader, validation_issue_count,
        };
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");

        let (_scratch, mut assets, material, first_mesh) = fixture_server("render-load");
        let two_prototype = two_prototype_family(&mut assets, material, first_mesh);
        let family = save_family(&mut assets, two_prototype);
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };

        let gpu = crate::gpu::RendererUploader::new(&uploader, &descriptors, false);
        let render = assets
            .load_plant_family(&gpu, family.id, published.publication.content_hash)
            .expect("family render");
        let assembly = render.mesh.assembly.as_ref().expect("assembly table");
        assert_eq!(
            assembly.prototypes.len(),
            2,
            "one prototype per source mesh"
        );
        assert!(assembly.uses.len() >= 2, "every prototype is placed");
        assert_eq!(assembly.prototypes[0].vertex_base, 0);
        assert_eq!(
            assembly.prototypes[1].vertex_base * 2,
            render.mesh.vertex_count,
            "two identical source meshes split the flattened stream evenly"
        );
        assert!(
            render.mesh.blas.is_none(),
            "an assembly carries no merged BLAS"
        );
        assert_eq!(render.materials.as_ref(), &[material]);

        // The family registers under its own id, so the mirror's mesh path resolves it.
        let registered = assets
            .load_mesh_asset(&gpu, family.id)
            .expect("family mesh resolves by id");
        assert!(std::sync::Arc::ptr_eq(&registered, &render.mesh));
        assert!(
            matches!(
                assets.page_payload_source(family.id),
                Some(crate::page_stream::PagePayloadSource::Cooked(_))
            ),
            "family pages stream from the retained cooked hierarchy"
        );

        device.wait_idle().expect("idle before teardown");
        assets.clear_asset_caches();
        drop(render);
        drop(registered);
        drop(assets);
        drop(uploader);
        drop(descriptors);
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn triangle_to_voxel_error_covers_thin_sheet_appearance() {
        let parameters = ThinSheetFoliageParameters {
            voxel_moments: VoxelMaterialMoments {
                occupancy: UnitInterval::from_bits(40_000),
                albedo_mean: [DecisionScalar::from_bits(20_000); 3],
                roughness_mean: UnitInterval::from_bits(30_000),
                transmission_mean: [DecisionScalar::from_bits(15_000); 3],
                thickness_mean: DecisionScalar::from_bits(400),
                normal_second_moments: [DecisionScalar::from_bits(12_000); 6],
            },
            ..ThinSheetFoliageParameters::default()
        };
        let material_asset = MaterialAsset {
            surface: MaterialSurface::ThinSheetFoliage(parameters),
            blend: "masked".to_owned(),
            double_sided: true,
            ..MaterialAsset::default()
        };
        let (_scratch, mut assets, material, mesh) =
            fixture_server_with_material("thin-sheet-error", &material_asset);
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let triangle = index
            .section(&bytes, PlantCompiledSectionKind::TriangleHierarchy)
            .unwrap()
            .unwrap();
        let voxel = index
            .section(&bytes, PlantCompiledSectionKind::VoxelHierarchy)
            .unwrap()
            .unwrap();
        let deformation = index
            .section(&bytes, PlantCompiledSectionKind::Deformation)
            .unwrap()
            .unwrap();
        let pages = index
            .section(&bytes, PlantCompiledSectionKind::PageDirectory)
            .unwrap()
            .unwrap();
        let ray_tracing = index
            .section(&bytes, PlantCompiledSectionKind::RayTracing)
            .unwrap()
            .unwrap();
        let hierarchy = decode_portable_virtual_hierarchy_sections(
            triangle.as_ref(),
            voxel.as_ref(),
            deformation.as_ref(),
            pages.as_ref(),
            ray_tracing.as_ref(),
        )
        .expect("portable hierarchy");
        let root = &hierarchy.nodes[hierarchy.roots[0] as usize];
        assert!(root.appearance_error.silhouette > 0);
        assert!(root.appearance_error.coverage > 0);
        assert!(root.appearance_error.transmission > 0);
        assert!(root.appearance_error.material > 0);
        assert!(root.appearance_error.normal_distribution > 0);
        assert_eq!(
            hierarchy.ray_tracing[hierarchy.roots[0] as usize].material_class,
            saffron_geometry::VirtualMaterialClass::ThinSheet
        );
    }

    #[test]
    fn obj_file_source_resolves_geometry_and_material_through_one_snapshot() {
        let scratch = Scratch::new("obj-source");
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(source_dir.join("oak.mtl"), "newmtl Bark\nKd 0.6 0.4 0.2\n")
            .expect("material source");
        std::fs::write(
            source_dir.join("oak.obj"),
            concat!(
                "mtllib oak.mtl\n",
                "o Oak\n",
                "v -0.5 0 0\n",
                "v 0.5 0 0\n",
                "v 0 1 0\n",
                "vt 0 0\n",
                "vt 1 0\n",
                "vt 0.5 1\n",
                "vn 0 0 1\n",
                "usemtl Bark\n",
                "f 1/1/1 2/2/1 3/3/1\n",
            ),
        )
        .expect("geometry source");
        let material = Uuid(8_001);
        let material_selector = PlantSourceSelector::Element {
            id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
            path: "materials/Bark".to_owned(),
        };
        let source_uri = "sources/oak.obj".to_owned();
        let family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings {
                            pivot: PlantPivot::SourceOrigin,
                            ..PlantImportSettings::default()
                        },
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [2; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: PlantSourceSelector::Whole,
                        destination: PlantSemanticDestination::Part(40),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 11,
                        selector: material_selector,
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                ],
            }),
        );
        let validation =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate OBJ source");
        assert!(
            validation.compile.publishable(),
            "{:?}",
            validation.compile.diagnostics
        );
        assert_eq!(validation.compile.statistics.meshes, 1);
        assert_eq!(validation.compile.statistics.materials, 1);
    }
}

/// The checked-in vegetation E2E fixture must stay cookable: its plant family (an
/// imported OBJ-trunk recipe) resolves, compiles, and publishes against the current
/// compiler. Regenerate with `cargo run -p xtask -- gen-vegetation-e2e-fixture` when a
/// format changes.
#[cfg(test)]
mod e2e_fixture {
    use super::*;
    use crate::AssetServer;

    fn options() -> PlantRecookOptions {
        PlantRecookOptions {
            limits: PlantCompileLimits::default(),
            versions: saffron_vegetation::CookVersionSet::current(),
            platform: saffron_vegetation::CookPlatformProfile {
                target: "test-target".to_owned(),
                content_profile: "portable-vulkan".to_owned(),
                toolchain: "rust-test".to_owned(),
                features: vec!["plant-phase-4".to_owned()],
            },
        }
    }

    #[test]
    fn fixture_family_publishes_against_the_current_compiler() {
        let fixture_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/e2e/fixtures/vegetation-phase3.json"
        );
        let raw = std::fs::read_to_string(fixture_path).expect("fixture json");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("fixture parse");
        let from_hex = |value: &str| -> Vec<u8> {
            (0..value.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
                .collect()
        };
        let plant_bytes = from_hex(json["plantHex"].as_str().unwrap());
        let trunk = from_hex(json["trunkObjHex"].as_str().unwrap());
        let trunk_path = json["trunkObjPath"].as_str().unwrap();

        let root =
            std::env::temp_dir().join(format!("saffron-fixture-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let mut assets = AssetServer::new(root.join("assets"));
        let full = assets.root.join(trunk_path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &trunk).unwrap();
        let family = saffron_vegetation::read_plant_asset(&plant_bytes).expect("fixture plant");
        let id = crate::save_plant_family_asset(&mut assets, family, "E2E birch", "plants")
            .expect("register fixture plant");
        let family = crate::load_plant_family_asset(&assets, id).expect("reload fixture plant");
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        match outcome {
            PlantRecookOutcome::Published(published) => {
                let bytes = std::fs::read(&published.publication.path).expect("artifact");
                let decoded = crate::plant_render::decode_plant_render_sections(&bytes)
                    .expect("fixture family render-decodes");
                assert!(
                    !decoded.mesh.vertices.is_empty(),
                    "the fixture family carries renderable geometry"
                );
            }
            PlantRecookOutcome::Rejected(validation) => {
                panic!(
                    "rejected: conflicts {:#?} diagnostics {:#?}",
                    validation.compile.conflicts, validation.compile.diagnostics
                );
            }
        }
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;

    fn source() -> PlantSourceReference {
        PlantSourceReference {
            id: 10,
            locator: PlantSourceLocator::File("file:///plants/oak.gltf".to_owned()),
            role: PlantSourceRole::Geometry,
            selector: PlantSourceSelector::Whole,
            content_hash: [0; 32],
            settings: PlantImportSettings::default(),
            provenance: saffron_vegetation::SourceProvenance::default(),
        }
    }

    /// A file that states a copyright the plant source does not record raises it, so an export from a
    /// tool whose licence requires attribution cannot be cooked in silence.
    #[test]
    fn a_stated_copyright_without_recorded_attribution_is_reported() {
        let origin = saffron_geometry::ImportedOrigin {
            generator: "SpeedTree Modeler 9.5.2".to_owned(),
            copyright: "(c) 2026 Example Studio".to_owned(),
        };
        let notice = attribution_notice(&source(), &origin).expect("a notice");
        assert_eq!(notice.severity, PlantCompileDiagnosticSeverity::Warning);
        assert!(notice.message.contains("SpeedTree Modeler 9.5.2"));
        assert!(notice.message.contains("Example Studio"));

        // Once the source records an attribution there is nothing to raise.
        let mut recorded = source();
        recorded.provenance.attribution = "Oak by Example Studio".to_owned();
        assert!(attribution_notice(&recorded, &origin).is_none());

        // A generator alone is not a licence claim, and a file that states nothing raises nothing.
        let generator_only = saffron_geometry::ImportedOrigin {
            generator: "Blender 4.2".to_owned(),
            copyright: String::new(),
        };
        assert!(attribution_notice(&source(), &generator_only).is_none());
        assert!(
            attribution_notice(&source(), &saffron_geometry::ImportedOrigin::default()).is_none()
        );
    }
}
