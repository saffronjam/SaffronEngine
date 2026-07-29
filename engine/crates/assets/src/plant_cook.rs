//! Plant-source resolution and atomic `.splantc` publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_geometry::{
    AlphaMode, ChunkKind, ImportedMaterial, ImportedModel, ImportedNode, Mesh,
    PortableVirtualHierarchy, Submesh, VertexSkin, VirtualHierarchyMaterial, VirtualMaterialClass,
    calibrate_voxel_appearance_error, canonical_hierarchy_reference_fixtures, contour_alpha_card,
    cook_portable_virtual_hierarchy, decode_image_from_memory,
    decode_portable_virtual_hierarchy_sections, load_mesh_from_bytes, load_mesh_skin_from_bytes,
    sub_id_for, translate_model,
};
use saffron_scene::AssetType;
use saffron_spatial::DecisionScalar;
use saffron_vegetation::{
    AlphaClassification, ContentHash, CookDependency, CookDependencyAddress, CookNodeAddress,
    CookNodeRecord, CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
    CoverageSource, MaterialSurface, NormalizedPlantFamily, NormalizedPlantMesh,
    NormalizedPlantSkin, NormalizedPlantVertex, PLANT_ASSET_VERSION,
    PLANT_COMPILED_ARTIFACT_VERSION, PLANT_SOURCE_COMPILER_VERSION, PlantCompileDiagnostic,
    PlantCompileDiagnosticCode, PlantCompileDiagnosticSeverity, PlantCompileLimits,
    PlantCompileOutput, PlantCompiledArtifactHeader, PlantCompiledArtifactIndex,
    PlantCompiledSection, PlantCompiledSectionKind, PlantFamilyAsset, PlantFamilySource,
    PlantImportSettings, PlantPartSemantic, PlantPivot, PlantReimportConflictReason,
    PlantSemanticDestination, PlantSourceJointSnapshot, PlantSourceLocator,
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

/// Largest edge a cooked family atlas may reach before packing refuses the set.
///
/// A family that cannot fit is not atlased at all rather than partially packed: a silently dropped
/// slot renders untextured, which reads as a material bug rather than the budget one it is.
const FAMILY_ATLAS_MAX_EDGE: u32 = 4096;

/// Texels of separation between packed slots, carried as edge colour at zero alpha.
///
/// Bilinear filtering reaches past a sub-rectangle's edge, so touching rectangles bleed — a leaf
/// carrying a sliver of bark, visible only at distance.
const FAMILY_ATLAS_GUTTER: u32 = crate::DEFAULT_ATLAS_GUTTER;

/// Raster resolution the voxel appearance-error calibration renders each transition at.
///
/// The measurement costs one render per voxel node per direction fixture, so this is a cook-time
/// budget as much as an accuracy choice. 32 is what the conformance tests measure at, and the
/// component the coarse resolution would under-report — silhouette — is the one thin features fail
/// on, so raising it would only ever widen further.
const VOXEL_CALIBRATION_RESOLUTION: u32 = 32;

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
    /// Decoded coverage per material slot, kept for the family atlas the cook packs from them.
    coverage_images: ResolvedCoverageImages,
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

/// Resolves `.splant` module calls through the asset layer.
///
/// Depth and cycles are this type's job rather than the evaluator's: the evaluator sees one graph
/// at a time, and only the chain of assets a call reaches through can tell whether it has come
/// back to where it started.
pub struct PlantModules<'a> {
    assets: &'a dyn CookAssetAccess,
    references: &'a [saffron_vegetation::PlantModuleReference],
    /// The assets on this chain, root first. A repeat is a cycle.
    chain: Vec<Uuid>,
    /// Module calls still allowed below this level.
    depth_budget: u16,
}

impl<'a> PlantModules<'a> {
    /// The resolver for one family's own module calls.
    #[must_use]
    pub fn for_family(assets: &'a AssetServer, asset: &'a PlantFamilyAsset) -> Self {
        Self {
            assets,
            references: &asset.modules,
            chain: vec![asset.id],
            depth_budget: asset.module_recursion_limit,
        }
    }
}

impl saffron_vegetation::BotanicalModuleResolver for PlantModules<'_> {
    fn grow_module(
        &self,
        call_guid: u128,
    ) -> saffron_vegetation::Result<saffron_vegetation::BotanicalModuleGrowth> {
        let field = |name: &str| saffron_vegetation::Error::InvalidFormat {
            format: ".splant",
            field: name.to_owned(),
        };
        if self.depth_budget == 0 {
            return Err(field("modules.depth"));
        }
        let reference = self
            .references
            .iter()
            .find(|reference| reference.call_guid == call_guid)
            .ok_or_else(|| field("modules.callGuid"))?;
        if self.chain.contains(&reference.plant) {
            return Err(field("modules.cycle"));
        }
        let module = crate::vegetation::load_plant_family_asset_from(self.assets, reference.plant)
            .map_err(|_| field("modules.plant"))?;
        if module.role != saffron_vegetation::PlantFamilyRole::Module {
            return Err(field("modules.role"));
        }
        let saffron_vegetation::PlantFamilySource::Native { graph, .. } = module.source.clone()
        else {
            // Only a graph grows. An imported family is finished geometry, which a call site
            // would have to place rather than grow, and no such operator exists.
            return Err(field("modules.source"));
        };
        let mut chain = self.chain.clone();
        chain.push(module.id);
        let nested = PlantModules {
            assets: self.assets,
            references: &module.modules,
            chain,
            // The chain's own limit never widens what an ancestor allowed.
            depth_budget: self
                .depth_budget
                .saturating_sub(1)
                .min(module.module_recursion_limit),
        };
        // A module grows in full even under a preview budget: the caller's bound stops its own
        // walk between nodes, and half a preset is a different preset.
        let growth = saffron_vegetation::grow(
            &graph,
            reference.variation as usize,
            &nested,
            &saffron_vegetation::BotanicalBudget::COOK,
        )?;
        Ok(saffron_vegetation::BotanicalModuleGrowth {
            assembly: growth.assembly,
            scale: reference.scale,
        })
    }
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
    let modules = PlantModules {
        assets: &*assets,
        references: &asset.modules,
        chain: vec![asset.id],
        depth_budget: asset.module_recursion_limit,
    };
    let mut compile = compile_plant_family(asset, &resolved.snapshots, limits, &modules)?;
    let hierarchy_materials = hierarchy_materials(&compile, &resolved.snapshots);
    merge_resolution_issues(&mut compile, resolved.issues);
    let accepted_asset = asset_with_source_updates(asset, &compile.source_updates)?;
    let dependencies = plant_dependencies(
        &accepted_asset,
        &resolved.snapshots,
        &resolved.material_documents,
        &resolved.coverage_images,
    )?;
    Ok(PreparedPlantFamily {
        validation: PlantValidationOutcome {
            compile,
            dependencies,
        },
        accepted_asset,
        material_documents: resolved.material_documents,
        hierarchy_materials,
        coverage_images: resolved.coverage_images,
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
        coverage_images,
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
        &coverage_images,
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
    coverage_images: ResolvedCoverageImages,
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
                // family-specific derivation runs. Geometry-first contours rewrite the snapshot
                // for a geometry source and not for a material source, so hashing afterwards makes
                // two sources over one file disagree about that file's identity — and the cook key
                // rejects one address carrying two contents. The derivation is a pure function of
                // the family asset and the file, both of which the key already depends on.
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedCoverageImage {
    alpha: Vec<u8>,
    /// The decoded RGBA texels the alpha plane was derived from.
    ///
    /// The contour pass needs coverage alone, but the family atlas composites colour: a gutter
    /// carrying transparent black filters into a slot's edge as a dark fringe, so packing needs
    /// the edge texel's hue as well as its coverage.
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    cutoff: u8,
    /// Catalog texture the coverage was decoded from, with the hash of the bytes read.
    ///
    /// The material document names its coverage texture by id and nothing else, so repainting the
    /// texture leaves the document byte-identical. Without the texture's own bytes in the cook key,
    /// an edited leaf cutout returns the previous artifact — the coverage guards catch it at
    /// publication, but a guard is a staleness check, not a key. `None` for an imported material,
    /// whose texture bytes travel inside the model source and are already covered by that source's
    /// content hash.
    texture: Option<(Uuid, ContentHash)>,
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
    let bytes = read_catalog_asset_bytes(assets, texture)?;
    let content_hash = ContentHash::of(&bytes);
    let decoded = decode_image_from_memory(&bytes)?;
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
        Some((texture, content_hash)),
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
        None,
    )))
}

fn resolved_coverage_image(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    cutoff: u8,
    base_alpha: f32,
    texture: Option<(Uuid, ContentHash)>,
) -> ResolvedCoverageImage {
    let alpha = rgba
        .chunks_exact(4)
        .map(|pixel| normalized_f32_to_u8(f32::from(pixel[3]) / 255.0 * base_alpha))
        .collect();
    ResolvedCoverageImage {
        alpha,
        rgba,
        width,
        height,
        cutoff,
        texture,
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
    let bytes = assets.read_file(&path)?;
    // USD carries a plant's structure in `UsdSkel` prims rather than in a model graph, and the
    // model importers do not read it at all. Routing it here — rather than teaching `translate_model`
    // a fourth format — keeps the skeleton reader the single truth about that file.
    let usd = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension == "usd" || extension == "usda"
        });
    if usd {
        return resolve_usd_skeleton_source(uri, &bytes);
    }
    let graph = translate_model(&path)?;
    resolve_imported_model(asset, uri, &path, graph)
}

/// Resolves a USD stage into a skeleton-only source contribution.
///
/// A USD source supplies STRUCTURE, not geometry: its joints become the family's spine, and the
/// meshes a plant renders come from its geometry sources. Reporting an empty stage as an error
/// rather than as an empty skeleton matters — a recipe that names a file with no skeleton in it is
/// a mistake, and an empty joint list would surface much later as a plant that refuses to bend.
fn resolve_usd_skeleton_source(uri: &str, bytes: &[u8]) -> Result<ResolvedPlantSource> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        Error::Io("USD plant source is not UTF-8 (binary .usdc is unsupported)".to_owned())
    })?;
    let (skeletons, unsupported) = saffron_vegetation::read_usd_skeletons(text)
        .map_err(|err| Error::Io(format!("USD plant source: {err}")))?;
    let Some(skeleton) = skeletons.into_iter().next() else {
        return Err(Error::Io(format!(
            "USD plant source '{uri}' declares no UsdSkel skeleton"
        )));
    };
    // A joint's selector is its authored path token, which is the identity USD itself uses — so a
    // re-import against an edited stage matches joints by name rather than by position, and adding
    // a joint does not silently re-parent every one after it.
    let selector_for = |path: &str| PlantSourceSelector::Element {
        id: u128::from(sub_id_for(uri, "joint", path, 0).value()),
        path: path.to_owned(),
    };
    let joints = skeleton
        .joints
        .iter()
        .map(|joint| PlantSourceJointSnapshot {
            selector: selector_for(&joint.path),
            parent: joint
                .parent
                .and_then(|index| skeleton.joints.get(index))
                .map(|parent| selector_for(&parent.path)),
            rest_transform: usd_row_major_transform(joint.rest),
        })
        .collect();
    let mut snapshot = PlantSourceSnapshot {
        source: 0,
        content_hash: [0; 32],
        meshes: Vec::new(),
        materials: Vec::new(),
        joints,
        semantic_elements: Vec::new(),
    };
    snapshot.content_hash = source_snapshot_hash(&snapshot);
    // Attributes the reader could not express are reported rather than dropped: a stage carrying
    // a custom plant schema should say so, not import as if it had none.
    if !unsupported.is_empty() {
        tracing::info!(
            "USD plant source '{uri}': {} unsupported attribute(s): {}",
            unsupported.len(),
            unsupported.join(", ")
        );
    }
    Ok(ResolvedPlantSource {
        origin: saffron_geometry::ImportedOrigin::default(),
        snapshot,
        material_documents: BTreeMap::new(),
        coverage_images: ResolvedCoverageImages::new(),
    })
}

/// Converts a USD `matrix4d` into a `Mat4`.
///
/// USD writes the matrix row-major and composes with row vectors, so its translation is the last
/// ROW; `Mat4` is column-major and composes with column vectors, putting translation in the last
/// COLUMN. Converting between the two conventions is a transpose — and a transpose of a row-major
/// array read as a column-major one is the identity on the storage, so the values pass through in
/// order. Permuting them here would transpose twice and put the translation back in the last row,
/// where `col(3)` reads it as a basis vector.
fn usd_row_major_transform(rows: [f64; 16]) -> Mat4 {
    Mat4::from_cols_array(&rows.map(|value| value as f32))
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
    if !matches!(extension.as_str(), "gltf" | "glb" | "obj" | "usd" | "usda") {
        return Err(Error::Io(
            "plant file source must be a .gltf, .glb, .obj, .usd, or .usda".to_owned(),
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
    coverage_images: &ResolvedCoverageImages,
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
    // The material document above names its coverage texture by id, so the document's hash does not
    // move when the texture's pixels do. The cook reads those pixels — the alpha plane drives the
    // geometry-first contour today and the opacity micromap tomorrow — so the texture is an input to
    // the artifact and belongs in the key that identifies it. Distinct textures dedupe through
    // `insert_dependency`, and two materials sharing one texture name one dependency.
    for image in coverage_images.values() {
        let Some((texture, content_hash)) = image.texture else {
            continue;
        };
        insert_dependency(
            &mut dependencies,
            CookDependency {
                address: CookDependencyAddress::SourceAsset { asset: texture },
                content_hash,
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
            return Err(Error::Io(format!(
                "plant dependency {:?} resolved to conflicting content",
                dependency.address
            )));
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

/// Derives one opacity micromap per non-opaque flattened submesh, against the atlas alpha plane.
///
/// A micromap may only ever REMOVE classifier work — every micro-triangle it settles was proven to
/// classify that way for every hash, anchor and phase — so an absent or empty one is always correct
/// and never a rendering difference. That is what makes it safe to skip a submesh for any reason.
///
/// It reads the ATLAS plane rather than a slot's own image because the family's UVs address the
/// atlas by this point; a pyramid over the unpacked image would answer about texels the GPU never
/// reads. Flattening goes through the same `flatten_prototype_rows` the runtime uses, so the
/// submesh indices here and the BLAS geometries at upload agree by construction rather than by two
/// implementations of one walk.
/// The plant's local bounds-sphere top, the height the wind prepass scales its modes by.
///
/// The prepass reads `localBounds.y + localBounds.w` — a sphere centre plus its radius — so the
/// cook derives the same quantity from the hierarchy it is about to publish rather than from the
/// authored dimensions, which describe the source before normalization.
fn hierarchy_top_height(hierarchy: &saffron_geometry::PortableVirtualHierarchy) -> f32 {
    let mut minimum = [i64::MAX; 3];
    let mut maximum = [i64::MIN; 3];
    for node in &hierarchy.nodes {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(i64::from(node.bounds.min_bits[axis]));
            maximum[axis] = maximum[axis].max(i64::from(node.bounds.max_bits[axis]));
        }
    }
    if minimum[0] > maximum[0] {
        return 0.0;
    }
    let radius: [f32; 3] =
        std::array::from_fn(|axis| (maximum[axis] - minimum[axis]) as f32 / 65_536.0 * 0.5);
    let center_y = (minimum[1] + maximum[1]) as f32 / 65_536.0 * 0.5;
    center_y + radius.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// The wind modes an aggregate voxel drops, at the amplitude the runtime can never exceed.
///
/// The prepass derives both amplitudes from the sampled wind speed and then CLAMPS the wind term
/// — `min(speed * 0.02, 0.5)` for the branch mode, `min(speed * 0.012, 0.2)` for flutter — before
/// the authored response scales it. The largest either can reach is therefore a property of the
/// family alone, and a cook-time bound is exact rather than a guess at a reference gust.
///
/// Takes the PACKED words rather than the authored struct, because those are the bytes the GPU
/// reads: the same decode, including the all-zero case the prepass treats as no authored response
/// and answers from the plant's height alone.
fn modal_aggregation_bound(
    mechanics: [u32; 4],
    top_height: f32,
) -> saffron_geometry::ModalAggregationBound {
    let authored = mechanics != [0; 4];
    let scalar = |word: u32| word as i32 as f32 / 65_536.0;
    let stiffness = if authored {
        scalar(mechanics[0]).max(0.05)
    } else {
        1.0
    };
    let drag = if authored { scalar(mechanics[1]) } else { 1.0 };
    let flutter = if authored { scalar(mechanics[2]) } else { 1.0 };
    let settle = if authored {
        1.0 - 0.5 * ((mechanics[3] & 0xffff) as f32 / 65_535.0)
    } else {
        1.0
    };
    // A zero authored limit is unlimited, exactly as the prepass reads it: a plant that may not
    // bend at all is a prop rather than a bend limit.
    let limit = (mechanics[3] >> 16) as f32 / 65_535.0;
    let bend_limit = if authored && limit > 0.0 {
        limit * 2.0
    } else {
        f32::INFINITY
    };
    let height = top_height.max(0.5);
    saffron_geometry::ModalAggregationBound {
        branch: (0.5 * (height * 0.25).min(1.0) * drag * settle / stiffness).min(bend_limit),
        flutter: (0.2 * flutter * settle).min(bend_limit),
    }
}

fn derive_family_micromaps(
    hierarchy: &PortableVirtualHierarchy,
    family: &NormalizedPlantFamily,
    hierarchy_materials: &[VirtualHierarchyMaterial],
    material_documents: &BTreeMap<u64, Vec<u8>>,
    coverage_images: &ResolvedCoverageImages,
    atlas: &crate::FamilyAtlas,
) -> Result<Vec<saffron_geometry::PortableOpacityMicromap>> {
    let rows = decode_mesh_section(&mesh_section(family, PlantSourceRole::Geometry, true))?;
    let flat = crate::plant_render::flatten_prototype_rows(hierarchy, &rows)?;
    let alpha = atlas.alpha_plane();
    let uvs: Vec<[f32; 2]> = flat
        .vertices
        .iter()
        .map(|vertex| [vertex.uv0.x, vertex.uv0.y])
        .collect();
    let mut micromaps = Vec::new();
    for (index, submesh) in flat.submeshes.iter().enumerate() {
        let Some(material) = hierarchy_materials
            .iter()
            .find(|entry| entry.slot == submesh.material_slot)
        else {
            continue;
        };
        // A fully covered surface commits hits without consulting the classifier, so there is no
        // per-micro-triangle work for a micromap to remove.
        if material.class.is_opaque() || !material.opacity_micromap {
            continue;
        }
        let Some(slot_material) = family.materials.get(submesh.material_slot as usize) else {
            continue;
        };
        let Some(coverage) = coverage_images.get(&slot_material.material.value()) else {
            continue;
        };
        let asset = material_documents
            .get(&slot_material.material.value())
            .and_then(|document| decode_plant_material_document(document).ok());
        // The atlas packs raw decoded texels, so the material's own base alpha has not been
        // folded in. Passing 1.0 would over-estimate coverage and could settle a micro-triangle
        // opaque that the shader renders cut out — the one way a micromap may change a pixel.
        let base_alpha = asset.as_ref().map_or(1.0, |asset| asset.base_color.w);
        // Thin-sheet foliage authors its own derivation policy; a plain masked surface has no
        // field to author one in, so it gets the conservative default. Authored thresholds
        // INTERSECT the provable set — they can only narrow it, never widen it — which is what
        // keeps "a micromap never changes correctness" a property of the code rather than of the
        // numbers someone typed.
        let policy = match asset.as_ref().map(|asset| &asset.surface) {
            Some(MaterialSurface::ThinSheetFoliage(parameters)) => parameters.opacity_micromap,
            _ => default_masked_micromap_policy(),
        };
        let (classification, _) = material_coverage_class(material.class);
        let rule = saffron_geometry::CoverageRule::new(
            classification,
            matches!(material.class, VirtualMaterialClass::ThinSheet),
            f32::from(coverage.cutoff) / 255.0,
        );
        let start = submesh.first_index as usize;
        let end = start.saturating_add(submesh.index_count as usize);
        let Some(indices) = flat.indices.get(start..end.min(flat.indices.len())) else {
            continue;
        };
        let build = saffron_geometry::derive_opacity_micromap(
            indices,
            &uvs,
            saffron_geometry::CoverageSourcePlane {
                alpha: &alpha,
                width: atlas.layout.width,
                height: atlas.layout.height,
                rule,
                base_alpha,
            },
            &policy,
        );
        if build.blocks.is_empty() {
            continue;
        }
        micromaps.push(saffron_geometry::PortableOpacityMicromap {
            submesh: u32::try_from(index).unwrap_or(u32::MAX),
            indices: build.indices,
            blocks: build
                .blocks
                .iter()
                .map(|block| (block.data_offset, block.subdivision_level, block.format))
                .collect(),
            data: build.data,
            usage: build
                .usage
                .iter()
                .map(|row| (row.count, row.subdivision_level, row.format))
                .collect(),
            classes: (
                build.classes.opaque,
                build.classes.transparent,
                build.classes.unknown,
            ),
        });
    }
    Ok(micromaps)
}

/// The derivation policy for a masked surface, which has no field to author one in.
///
/// Enabled with the widest thresholds, so the derivation is bounded only by what it can PROVE:
/// a micro-triangle settles opaque or transparent when the min/max pyramid over its dilated UV
/// footprint puts every point on one side of the cutoff, and stays unknown otherwise.
fn default_masked_micromap_policy() -> saffron_material::OpacityMicromapDerivation {
    saffron_material::OpacityMicromapDerivation {
        enabled: true,
        max_subdivision: 5,
        transparent_threshold: saffron_spatial::UnitInterval::from_bits(0),
        opaque_threshold: saffron_spatial::UnitInterval::from_bits(u16::MAX),
    }
}

/// The alpha classification a cooked material class implies, and whether it transmits.
const fn material_coverage_class(class: VirtualMaterialClass) -> (AlphaClassification, bool) {
    match class {
        VirtualMaterialClass::Opaque => (AlphaClassification::Opaque, false),
        VirtualMaterialClass::Transmissive => (AlphaClassification::Transmissive, true),
        VirtualMaterialClass::Masked | VirtualMaterialClass::ThinSheet => {
            (AlphaClassification::Masked, false)
        }
    }
}

/// Packs the family's coverage images into one atlas and rewrites its UVs to address it.
///
/// Returns the atlas, or `None` when the family has no coverage images or the set cannot be packed
/// within [`FAMILY_ATLAS_MAX_EDGE`]. Either way the family is left coherent: an un-atlased family
/// keeps slot-local UVs and binds slot-local textures, so the two never half-apply.
///
/// This MUST run after `apply_geometry_first_contours`, which re-tessellates alpha cards against
/// slot-local coverage and emits new UVs. Remapping first would leave the contour addressing atlas
/// space with a slot-local alpha plane.
///
/// UVs fork downstream into both the Geometry section and the hierarchy's quantized cluster
/// vertices, and both read `NormalizedPlantVertex::uv_bits` — so rewriting here, upstream of the
/// fork, is what keeps the two from disagreeing.
fn atlas_normalized_family(
    family: &mut NormalizedPlantFamily,
    coverage_images: &ResolvedCoverageImages,
) -> Option<crate::FamilyAtlas> {
    let mut slots = Vec::new();
    for (slot, material) in family.materials.iter().enumerate() {
        let Some(image) = coverage_images.get(&material.material.value()) else {
            continue;
        };
        slots.push(crate::FamilySlotImage {
            slot: u32::try_from(slot).ok()?,
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        });
    }
    if slots.is_empty() {
        return None;
    }
    // The cutoff the mip chain must preserve coverage against. Slots may declare different ones;
    // the lowest is the conservative choice, since a chain that preserves coverage at the lowest
    // cutoff preserves it at every higher one.
    let cutoff = family
        .materials
        .iter()
        .filter_map(|material| coverage_images.get(&material.material.value()))
        .map(|image| u16::from(image.cutoff) << 8)
        .min()
        .unwrap_or(u16::MAX / 2);
    let atlas =
        crate::generate_family_atlas(&slots, FAMILY_ATLAS_MAX_EDGE, FAMILY_ATLAS_GUTTER, cutoff)?;
    remap_family_uvs(family, &atlas.layout);
    Some(atlas)
}

/// Rewrites every vertex's UV from its slot's own image into atlas space.
///
/// A vertex reached from two submeshes with different material slots has no single answer, so it is
/// DUPLICATED — one copy per slot, with that slot's remap — and the offending indices rewritten.
/// Refusing to atlas such a family would be the quieter option and the wrong one: it would make
/// atlasing depend on a mesh detail nobody authored deliberately.
fn remap_family_uvs(family: &mut NormalizedPlantFamily, layout: &crate::AtlasLayout) {
    for mesh in &mut family.meshes {
        // Slot claimed by each vertex, and the duplicate minted for any second claimant.
        let mut claimed = vec![u32::MAX; mesh.vertices.len()];
        let mut duplicates = std::collections::BTreeMap::<(u32, u32), u32>::new();
        let mut minted: Vec<NormalizedPlantVertex> = Vec::new();
        let mut minted_skin: Vec<NormalizedPlantSkin> = Vec::new();
        for submesh in &mesh.submeshes {
            let slot = submesh.material_slot;
            let start = submesh.first_index as usize;
            let end = start.saturating_add(submesh.index_count as usize);
            for position in start..end.min(mesh.indices.len()) {
                let vertex = mesh.indices[position];
                let Some(claim) = claimed.get_mut(vertex as usize) else {
                    continue;
                };
                if *claim == u32::MAX {
                    *claim = slot;
                } else if *claim != slot {
                    let next =
                        u32::try_from(mesh.vertices.len() + minted.len()).unwrap_or(u32::MAX);
                    let copy = *duplicates.entry((vertex, slot)).or_insert_with(|| {
                        minted.push(mesh.vertices[vertex as usize]);
                        if let Some(skin) = mesh.skin.get(vertex as usize) {
                            minted_skin.push(*skin);
                        }
                        next
                    });
                    mesh.indices[position] = copy;
                }
            }
        }
        let duplicate_slots: std::collections::BTreeMap<u32, u32> = duplicates
            .iter()
            .map(|(&(_, slot), &copy)| (copy, slot))
            .collect();
        let base = mesh.vertices.len();
        mesh.vertices.extend(minted);
        if !mesh.skin.is_empty() {
            mesh.skin.extend(minted_skin);
        }
        for (index, vertex) in mesh.vertices.iter_mut().enumerate() {
            let slot = if index < base {
                claimed[index]
            } else {
                duplicate_slots
                    .get(&u32::try_from(index).unwrap_or(u32::MAX))
                    .copied()
                    .unwrap_or(u32::MAX)
            };
            let Some(placement) = layout.placement(slot) else {
                continue;
            };
            let uv = [q16_to_f32(vertex.uv_bits[0]), q16_to_f32(vertex.uv_bits[1])];
            let remapped = placement.remap(uv, layout.width, layout.height);
            vertex.uv_bits = [f32_to_q16(remapped[0]), f32_to_q16(remapped[1])];
        }
    }
}

/// Q15.16 fixed point to float, the canonical UV encoding both cook consumers read.
fn q16_to_f32(bits: i32) -> f32 {
    bits as f32 / 65_536.0
}

/// Float to Q15.16, rounding to nearest so a remap round-trips within one ulp of the grid.
fn f32_to_q16(value: f32) -> i32 {
    (value * 65_536.0).round() as i32
}

fn build_plant_sections(
    asset: &PlantFamilyAsset,
    compile: &PlantCompileOutput,
    material_documents: &BTreeMap<u64, Vec<u8>>,
    hierarchy_materials: &[VirtualHierarchyMaterial],
    coverage_images: &ResolvedCoverageImages,
) -> Result<Vec<PlantCompiledSection>> {
    let mut family = compile
        .family
        .as_ref()
        .ok_or_else(|| Error::Io("plant compiler produced no publishable family".to_owned()))?
        .clone();
    // Pack the family's coverage into one atlas and rewrite its UVs to address it, before anything
    // downstream reads them. Every section then describes ONE family — the atlased one — rather
    // than the geometry describing atlas space while the normalization record describes the
    // compiler's slot-local output.
    let atlas = atlas_normalized_family(&mut family, coverage_images);
    let family = &family;
    let mut accepted_compile = compile.clone();
    accepted_compile.family = Some(family.clone());
    accepted_compile.source_updates.clear();
    accepted_compile
        .diagnostics
        .retain(|diagnostic| diagnostic.code != PlantCompileDiagnosticCode::SourceChanged);
    let hierarchy_input = plant_hierarchy_input(asset, family, hierarchy_materials)?;
    let mut hierarchy = cook_portable_virtual_hierarchy(&hierarchy_input)?;
    // The cooker's appearance error is an analytic guess from bounds and material moments, and it
    // guesses low for thin separated features — a brick fills the gaps a comb of blades leaves, so
    // the aggregate reads as a slab where the triangles read as a comb. The cut selector trusts
    // that number to decide when a voxel brick may stand in for triangles, so a low one swaps early
    // and pops. Measuring the transition and widening to what it shows is the only thing that makes
    // the declared value mean what the selector reads it as. Vegetation is entirely thin features,
    // which is why this runs on every plant rather than on request.
    // The measurement also has to cover what aggregating TAKES AWAY, not only what it gets
    // wrong standing still. A triangle cut swings each assembly use about its pivot and
    // shimmers the leaves; an aggregate brick has no parts and applies neither, keeping only
    // the whole-plant sway. Distant vegetation therefore moves LESS than near vegetation by a
    // known amount, and the declared transition error is what has to cover that difference —
    // otherwise a plant visibly stiffens at the moment the cut coarsens.
    let modal = modal_aggregation_bound(
        crate::gpu_scene_mirror::packed_mechanics(Some(asset.mechanics)),
        hierarchy_top_height(&hierarchy),
    );
    calibrate_voxel_appearance_error(
        &mut hierarchy,
        &canonical_hierarchy_reference_fixtures(),
        VOXEL_CALIBRATION_RESOLUTION,
        modal,
    )?;
    if let Some(atlas) = atlas.as_ref() {
        hierarchy.opacity_micromaps = derive_family_micromaps(
            &hierarchy,
            family,
            hierarchy_materials,
            material_documents,
            coverage_images,
            atlas,
        )?;
    }
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
            material_section(family, material_documents, atlas.as_ref()),
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
        PlantCompiledSection::new(
            PlantCompiledSectionKind::DistanceField,
            distance_field_section(&hierarchy),
        ),
    ])
}

/// The family-space signed distance field, derived from the coarsest aggregate voxel
/// brick's occupancy — the same grid the aggregate raster form draws, so what a march
/// occludes against is what the coarse cut shows. Deterministic: an exact integer
/// squared-distance transform over the occupancy bits, quantized once at encode.
///
/// Empty bytes when the family cooked no voxel brick (nothing aggregate to occlude
/// with); the reader treats an empty section as "no field", not an error.
fn distance_field_section(hierarchy: &saffron_geometry::PortableVirtualHierarchy) -> Vec<u8> {
    use saffron_geometry::glam::Vec3;

    // The DEEPEST voxel level's bricks: they tile the plant tightly, where the root's
    // single 8³ brick — dilated for watertight reconstruction — reads as one solid box
    // the size of the family, which is a wall, not a plant.
    let mut depth_of = vec![0_u32; hierarchy.nodes.len()];
    for node in &hierarchy.nodes {
        if let Some(parent) = node.parent {
            depth_of[node.id as usize] = depth_of[parent as usize] + 1;
        }
    }
    let voxel_nodes: Vec<(&saffron_geometry::PortableHierarchyNode, u32)> = hierarchy
        .nodes
        .iter()
        .filter_map(|node| match node.representation {
            saffron_geometry::HierarchyRepresentation::Voxel { brick } => Some((node, brick)),
            saffron_geometry::HierarchyRepresentation::Triangles { .. } => None,
        })
        .collect();
    let Some(max_depth) = voxel_nodes
        .iter()
        .map(|(node, _)| depth_of[node.id as usize])
        .max()
    else {
        return Vec::new();
    };
    let selected: Vec<&saffron_geometry::PortableVoxelBrick> = voxel_nodes
        .iter()
        .filter(|(node, _)| depth_of[node.id as usize] == max_depth)
        .filter_map(|(_, brick)| hierarchy.voxel_bricks.iter().find(|b| b.id == *brick))
        .collect();
    if selected.is_empty() {
        return Vec::new();
    }

    let q = |bits: i32| bits as f32 / 65_536.0;
    let corner = |bounds: &saffron_geometry::PortableBounds, max: bool| {
        let bits = if max {
            bounds.max_bits
        } else {
            bounds.min_bits
        };
        Vec3::new(q(bits[0]), q(bits[1]), q(bits[2]))
    };
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for brick in &selected {
        lo = lo.min(corner(&brick.bounds, false));
        hi = hi.max(corner(&brick.bounds, true));
    }
    if !(hi - lo).min_element().is_finite() || (hi - lo).min_element() <= 0.0 {
        return Vec::new();
    }
    // The mesh bake's own grid sizing (padded bounds, capped axes), so a plant's field
    // resolves like an imported mesh's.
    let grid = saffron_geometry::bake_grid(lo, hi, 1.0);
    let [nx, ny, nz] = grid.dims;
    let total = (nx * ny * nz) as usize;
    let cell = grid.cell();

    // Rasterize each selected brick's set bits into the family grid: a set bit covers
    // its voxel's world box; every grid cell whose center falls inside is matter.
    let mut occupied_grid = vec![false; total];
    let idx = |x: u32, y: u32, z: u32| ((z * ny + y) * nx + x) as usize;
    for brick in &selected {
        let [bx, by, bz] = brick.dimensions.map(u32::from);
        if bx == 0 || by == 0 || bz == 0 || brick.occupancy.is_empty() {
            continue;
        }
        let blo = corner(&brick.bounds, false);
        let bhi = corner(&brick.bounds, true);
        let bcell = (bhi - blo) / Vec3::new(bx as f32, by as f32, bz as f32);
        for vz in 0..bz {
            for vy in 0..by {
                for vx in 0..bx {
                    let bit = (vx + bx * (vy + by * vz)) as usize;
                    if brick.occupancy[bit / 8] & (1u8 << (bit % 8)) == 0 {
                        continue;
                    }
                    let vmin = blo + bcell * Vec3::new(vx as f32, vy as f32, vz as f32);
                    let vmax = vmin + bcell;
                    let gmin = ((vmin - grid.bounds_min) / cell).floor().max(Vec3::ZERO);
                    let gmax = ((vmax - grid.bounds_min) / cell).ceil();
                    for gz in gmin.z as u32..(gmax.z as u32).min(nz) {
                        for gy in gmin.y as u32..(gmax.y as u32).min(ny) {
                            for gx in gmin.x as u32..(gmax.x as u32).min(nx) {
                                let center = grid.voxel_center(gx, gy, gz);
                                if center.cmpge(vmin).all() && center.cmplt(vmax).all() {
                                    occupied_grid[idx(gx, gy, gz)] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !occupied_grid.iter().any(|&occupied| occupied) {
        return Vec::new();
    }

    // Exact 3D Euclidean distance transform (Felzenszwalb–Huttenlocher, one 1D pass per
    // axis), run twice: distance to the nearest occupied cell and to the nearest empty
    // one. Integer squared distances throughout, so the result is bit-identical on
    // every target.
    let edt = |inside: bool| -> Vec<u64> {
        const INF: u64 = u64::MAX / 4;
        let mut field: Vec<u64> = (0..total)
            .map(|index| {
                if occupied_grid[index] == inside {
                    0
                } else {
                    INF
                }
            })
            .collect();
        let mut pass = |axis: usize| {
            let (a_len, b_len, c_len) = match axis {
                0 => (nx, ny, nz),
                1 => (ny, nx, nz),
                _ => (nz, nx, ny),
            };
            let mut line = vec![0_u64; a_len as usize];
            for c in 0..c_len {
                for b in 0..b_len {
                    for a in 0..a_len {
                        let (x, y, z) = match axis {
                            0 => (a, b, c),
                            1 => (b, a, c),
                            _ => (b, c, a),
                        };
                        line[a as usize] = field[idx(x, y, z)];
                    }
                    let transformed = squared_distance_transform_1d(&line);
                    for a in 0..a_len {
                        let (x, y, z) = match axis {
                            0 => (a, b, c),
                            1 => (b, a, c),
                            _ => (b, c, a),
                        };
                        field[idx(x, y, z)] = transformed[a as usize];
                    }
                }
            }
        };
        pass(0);
        pass(1);
        pass(2);
        field
    };
    let to_occupied = edt(true);
    let to_empty = edt(false);

    let voxel = cell.max_element();
    let dense: Vec<i16> = (0..total)
        .map(|index| {
            // Signed distance in world units: positive outside occupied matter, negative
            // inside, from the two unsigned transforms.
            let signed = if occupied_grid[index] {
                -((to_empty[index] as f64).sqrt() as f32) * voxel
            } else {
                ((to_occupied[index] as f64).sqrt() as f32) * voxel
            };
            let normalized = (signed / grid.max_dist).clamp(-1.0, 1.0);
            (normalized * f32::from(i16::MAX)) as i16
        })
        .collect();
    let mut field = saffron_geometry::Sdf::from_dense_field(&grid, &dense);

    // The field carries the CALIBRATED aggregate occupancy and albedo, not the drawn
    // material's: slot 0 of a placed plant is its trunk, and a canopy is not a solid.
    // NOT the coverage fraction either: the march integrates this as Beer-Lambert
    // DENSITY over the occupied path, so the value that preserves energy is the one that
    // reproduces the aggregate's calibrated transmission across its mean thickness — the
    // same parity the thin-sheet materials use.
    let moments = &selected[0].moments;
    let transmission = moments
        .transmission_mean
        .map(|value| value as f32 / 65_536.0);
    let thickness = (moments.thickness_mean as f32 / 65_536.0).max(0.05);
    let occupancy = crate::render_material::derive_parity_occupancy(transmission, thickness);
    field.header.occupancy_unorm = (occupancy.clamp(0.0, 1.0) * 65_535.0 + 0.5) as u32;
    let albedo = |axis: usize| -> u32 {
        ((moments.albedo_mean[axis] as f32 / 65_536.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u32
    };
    field.header.proxy_albedo = albedo(0) | (albedo(1) << 8) | (albedo(2) << 16);
    saffron_geometry::sdf_set_to_bytes(&[field])
}

/// Felzenszwalb–Huttenlocher 1D squared-distance transform over integer parabolas.
fn squared_distance_transform_1d(f: &[u64]) -> Vec<u64> {
    const INF: u64 = u64::MAX / 4;
    let n = f.len();
    let mut v = vec![0_usize; n];
    let mut z = vec![0_i64; n + 1];
    let mut k = 0_usize;
    v[0] = 0;
    z[0] = i64::MIN / 2;
    z[1] = i64::MAX / 2;
    // Parabola intersections in fixed-point twice-the-boundary units, exact in integers.
    let intersect = |q: usize, p: usize| -> i64 {
        let (q, p, fq, fp) = (q as i64, p as i64, f[q] as i64, f[p] as i64);
        // ((f[q] + q²) − (f[p] + p²)) / (2q − 2p), kept as a scaled numerator to stay
        // integral: compare s*2*(q−p) against boundaries scaled by 2*(q−p).
        (fq + q * q - fp - p * p) / (2 * (q - p)).max(1)
    };
    for q in 1..n {
        if f[q] >= INF && f[v[k]] >= INF {
            continue;
        }
        let mut s = intersect(q, v[k]);
        while k > 0 && s <= z[k] {
            k -= 1;
            s = intersect(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = i64::MAX / 2;
    }
    let mut k = 0_usize;
    let mut out = vec![0_u64; n];
    for (q, slot) in out.iter_mut().enumerate() {
        while z[k + 1] < q as i64 {
            k += 1;
        }
        let d = q as i64 - v[k] as i64;
        *slot = f[v[k]].saturating_add((d * d) as u64);
    }
    out
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
    // Parsing the response here means an unparseable part table fails the cook rather than
    // reaching the renderer, where the failure would read as a plant that will not sway.
    index.mechanical_response(bytes)?;
    for kind in PlantCompiledSectionKind::ALL {
        // The distance field is the one optional facet: a family that cooked no voxel
        // brick has nothing aggregate to occlude with and writes no section.
        if kind == PlantCompiledSectionKind::DistanceField {
            continue;
        }
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

fn material_section(
    family: &NormalizedPlantFamily,
    documents: &BTreeMap<u64, Vec<u8>>,
    atlas: Option<&crate::FamilyAtlas>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_domain(&mut bytes, b"saffron-anima/splantc/materials-coverage/v2");
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
    // The packed family atlas, whose extent the geometry's UVs already address. A family with no
    // coverage images, or one whose slots do not fit the budget, writes the absent marker and
    // keeps slot-local UVs — the two halves never half-apply.
    let Some(atlas) = atlas else {
        append_u32(&mut bytes, 0);
        return bytes;
    };
    append_u32(&mut bytes, 1);
    append_u32(&mut bytes, atlas.layout.width);
    append_u32(&mut bytes, atlas.layout.height);
    append_u32(&mut bytes, atlas.layout.gutter);
    append_u64(&mut bytes, atlas.layout.placements.len() as u64);
    for placement in &atlas.layout.placements {
        append_u32(&mut bytes, placement.slot);
        append_u32(&mut bytes, placement.x);
        append_u32(&mut bytes, placement.y);
        append_u32(&mut bytes, placement.width);
        append_u32(&mut bytes, placement.height);
    }
    append_u64(&mut bytes, atlas.levels.len() as u64);
    for level in &atlas.levels {
        append_u32(&mut bytes, level.width);
        append_u32(&mut bytes, level.height);
        append_bytes(&mut bytes, &level.rgba);
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

/// Decodes one MaterialsCoverage-section payload into its material rows and packed atlas.
pub(crate) fn decode_material_section(
    bytes: &[u8],
) -> Result<(Vec<PlantMaterialRow>, Option<crate::FamilyAtlas>)> {
    let mut reader = SectionReader::new(bytes);
    reader.expect_domain(b"saffron-anima/splantc/materials-coverage/v2")?;
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
    if reader.read_u32()? == 0 {
        return Ok((materials, None));
    }
    let width = reader.read_u32()?;
    let height = reader.read_u32()?;
    let gutter = reader.read_u32()?;
    let placement_count = reader.read_length()?;
    let mut placements = Vec::with_capacity(placement_count);
    for _ in 0..placement_count {
        placements.push(crate::AtlasPlacement {
            slot: reader.read_u32()?,
            x: reader.read_u32()?,
            y: reader.read_u32()?,
            width: reader.read_u32()?,
            height: reader.read_u32()?,
        });
    }
    let level_count = reader.read_length()?;
    let mut levels = Vec::with_capacity(level_count);
    for _ in 0..level_count {
        let level_width = reader.read_u32()?;
        let level_height = reader.read_u32()?;
        levels.push(crate::CoverageMip {
            width: level_width,
            height: level_height,
            rgba: reader.read_bytes()?.to_vec(),
        });
    }
    Ok((
        materials,
        Some(crate::FamilyAtlas {
            layout: crate::AtlasLayout {
                width,
                height,
                placements,
                gutter,
            },
            levels,
        }),
    ))
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

const _: () = assert!(PLANT_ASSET_VERSION == 5);
const _: () = assert!(PLANT_COMPILED_ARTIFACT_VERSION == 4);

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
            role: saffron_vegetation::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
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

    /// Replaces the sapling's leaf placement with a call to `call_guid`, and binds it to `module`.
    fn family_calling(material: Uuid, module: Uuid, call_guid: u128) -> PlantFamilyAsset {
        let mut family = native_family(material);
        let PlantFamilySource::Native { graph, .. } = &mut family.source else {
            panic!("the native fixture carries a graph");
        };
        let leaves = graph
            .nodes
            .iter()
            .position(|node| {
                matches!(
                    node.operator,
                    saffron_vegetation::BotanicalOperator::Instance { .. }
                )
            })
            .expect("the sapling places leaves");
        graph.nodes[leaves].operator =
            saffron_vegetation::BotanicalOperator::ModuleCall { call_guid };
        let family_output = graph
            .nodes
            .iter()
            .find(|node| matches!(node.operator, saffron_vegetation::BotanicalOperator::Family))
            .expect("the sapling has a family output")
            .guid;
        let module_node = graph.nodes[leaves].guid;
        graph.edges.push(saffron_vegetation::BotanicalEdge {
            from_node: module_node,
            from_pin: "shells".to_owned(),
            to_node: family_output,
            to_pin: "shells".to_owned(),
        });
        graph.edges.sort();
        family.modules = vec![saffron_vegetation::PlantModuleReference {
            plant: module,
            call_guid,
            variation: 0,
            scale: saffron_spatial::DecisionScalar::from_integer(1).unwrap(),
        }];
        family
    }

    #[test]
    fn a_module_call_composes_the_referenced_family() {
        // The feature in one assertion: a preset authored once, called from another family, and
        // reaching the compiled result. Before this a graph could only inline its own nodes.
        let (_scratch, mut assets, material, _mesh) = fixture_server("module-compose");
        let mut module = native_family(material);
        module.id = Uuid(0x1_0001);
        module.role = saffron_vegetation::PlantFamilyRole::Module;
        let module = save_family(&mut assets, module);
        let caller = family_calling(material, module.id, 0x77);
        let plain = native_family(material);

        let composed =
            prepare_plant_family_sources(&mut assets, &caller, PlantCompileLimits::default())
                .expect("the calling family compiles");
        let alone =
            prepare_plant_family_sources(&mut assets, &plain, PlantCompileLimits::default())
                .expect("the plain family compiles");
        assert!(
            composed.validation.compile.statistics.vertices
                > alone.validation.compile.statistics.vertices,
            "the module adds geometry the caller did not have"
        );
    }

    #[test]
    fn a_module_call_requires_a_module_role() {
        // An ordinary family placed by a world is not a preset. Allowing the call would make every
        // family silently reusable and every world placement ambiguous.
        let (_scratch, mut assets, material, _mesh) = fixture_server("module-role");
        let mut ordinary = native_family(material);
        ordinary.id = Uuid(0x1_0002);
        let ordinary = save_family(&mut assets, ordinary);
        let caller = family_calling(material, ordinary.id, 0x77);
        let outcome =
            prepare_plant_family_sources(&mut assets, &caller, PlantCompileLimits::default())
                .expect("compilation reports rather than fails");
        assert!(
            !outcome.validation.compile.publishable(),
            "a call to a non-module family must not publish"
        );
    }

    #[test]
    fn a_module_cycle_is_rejected_rather_than_grown() {
        // A preset that calls itself has no fixed point, and the depth bound alone would turn it
        // into a slow failure instead of an immediate one.
        let (_scratch, mut assets, material, _mesh) = fixture_server("module-cycle");
        let mut module = family_calling(material, Uuid(0), 0x77);
        module.id = Uuid(0x1_0003);
        module.role = saffron_vegetation::PlantFamilyRole::Module;
        module.modules[0].plant = Uuid(0x1_0004);
        let mut other = family_calling(material, module.id, 0x88);
        other.id = Uuid(0x1_0004);
        other.role = saffron_vegetation::PlantFamilyRole::Module;
        let module = save_family(&mut assets, module);
        let _other = save_family(&mut assets, other);
        let caller = family_calling(material, module.id, 0x99);
        let outcome =
            prepare_plant_family_sources(&mut assets, &caller, PlantCompileLimits::default())
                .expect("compilation reports rather than fails");
        assert!(
            !outcome.validation.compile.publishable(),
            "a cycle must not publish"
        );
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
            &saffron_vegetation::NoBotanicalModules,
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
    fn the_cooked_part_table_returns_the_authored_mechanical_response() {
        // The writer emits mechanics into the part table and the wind prepass consumes them.
        // A reader that drifted from the writer by one field would decode a plausible wrong
        // number rather than fail, so the round trip is asserted on a distinctive value.
        let (_scratch, mut assets, material, mesh) = fixture_server("mechanics-round-trip");
        let mut asset = imported_family(material, mesh);
        asset.mechanics = MechanicalResponse {
            stiffness: fixed(7),
            damping: UnitInterval::from_bits(9_000),
            drag: fixed(3),
            flutter: fixed(5),
            bend_limit: UnitInterval::from_bits(21_000),
            damage_threshold: fixed(11),
            break_threshold: fixed(13),
        };
        let authored = asset.mechanics;
        let family = save_family(&mut assets, asset);
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
        assert_eq!(
            index.mechanical_response(&bytes).expect("mechanics"),
            authored
        );
    }

    /// The cooked field must be porous: coverage-as-density reads as a black interior,
    /// so the header carries the transmission-parity occupancy instead — strictly below
    /// the raw coverage for any transmitting canopy — plus a real sign structure
    /// (negative somewhere inside, positive somewhere outside).
    #[test]
    fn plant_distance_field_is_calibrated_and_signed() {
        let (_scratch, mut assets, material, mesh) = fixture_server("plant-distance-field");
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
        let section = index
            .section(&bytes, PlantCompiledSectionKind::DistanceField)
            .expect("valid section")
            .expect("required section");
        assert!(!section.is_empty(), "a family with bricks cooks a field");
        let fields = saffron_geometry::sdf_set_from_bytes(section.as_ref()).expect("decode");
        assert_eq!(fields.len(), 1);
        let field = &fields[0];
        eprintln!(
            "occupancy_unorm={} albedo={:#x} dims={:?} max_dist={}",
            field.header.occupancy_unorm,
            field.header.proxy_albedo,
            field.header.dims,
            field.header.max_dist,
        );
        assert!(
            field.header.occupancy_unorm > 0,
            "the field carries its occupancy"
        );
        let dims = field.header.dims;
        let mut negative = 0_u32;
        let mut positive = 0_u32;
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let d = field.sample_voxel(x, y, z);
                    if d < 0.0 {
                        negative += 1;
                    } else if d > 0.0 {
                        positive += 1;
                    }
                }
            }
        }
        eprintln!("negative={negative} positive={positive}");
        assert!(negative > 0, "somewhere is inside matter");
        assert!(positive > 0, "somewhere is open");
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
    fn a_cooked_plant_declares_an_error_every_transition_fits_within() {
        // The claim the cut selector reads every frame: when a voxel brick stands in for its
        // triangle descendants, the declared appearance error covers how different it actually
        // looks. The cooker's analytic estimate does not guarantee that — it derives from bounds
        // and material moments and cannot see that a brick fills the gaps between thin separated
        // features, so it guesses low exactly where vegetation lives. The cook measures and widens.
        //
        // MUTATION-CHECKED: removing the `calibrate_voxel_appearance_error` call from
        // `build_plant_sections` fails this test with a named node, so it is testing the
        // calibration rather than the analytic estimate happening to be conservative.
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
            fixture_server_with_material("transition-error", &material_asset);
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
        let section = |kind| index.section(&bytes, kind).unwrap().unwrap();
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

        // Re-measured with the SAME modal bound the cook calibrated against, which is the
        // second half of the claim: distant vegetation keeps the whole-plant sway and loses the
        // per-part modes, and the declared error has to cover that loss as well as the shape
        // difference. Measuring with a zero bound here would assert the easier statement.
        let modal = modal_aggregation_bound(
            crate::gpu_scene_mirror::packed_mechanics(Some(family.mechanics)),
            hierarchy_top_height(&hierarchy),
        );
        assert!(
            !modal.is_zero(),
            "the fixture family must author modes for the bound to mean anything"
        );
        let comparisons = saffron_geometry::compare_triangle_voxel_transitions(
            &hierarchy,
            &canonical_hierarchy_reference_fixtures(),
            VOXEL_CALIBRATION_RESOLUTION,
            modal,
        )
        .expect("the published hierarchy measures");
        // Without transitions there is nothing to be within anything, and the loop below would
        // pass over an empty set — the exact shape of a test that proves nothing.
        assert!(
            !comparisons.is_empty(),
            "the fixture must cook transitions to measure"
        );
        for comparison in &comparisons {
            assert!(
                comparison.is_within_declared_error(),
                "voxel node {} exceeds its declared error: measured {:?} against declared {:?}",
                comparison.voxel_node,
                comparison.measured,
                comparison.declared
            );
        }
    }

    /// Encodes a cut-out RGBA PNG: a diagonal split from opaque to transparent.
    ///
    /// A UNIFORM texture is useless for micromap derivation and would look like a broken
    /// derivation rather than a degenerate fixture — every micro-triangle would be uniformly
    /// covered, which correctly emits the format's special index and no block at all. Only a
    /// plane with real coverage variation produces triangles that straddle the cutoff.
    fn cutout_png(edge: u32) -> Vec<u8> {
        let pixels: Vec<u8> = (0..edge * edge)
            .flat_map(|index| {
                let x = index % edge;
                let y = index / edge;
                let alpha = if x + y < edge { 255 } else { 0 };
                [255, 255, 255, alpha]
            })
            .collect();
        let image = image::RgbaImage::from_raw(edge, edge, pixels).expect("raw image");
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode png");
        bytes
    }

    /// Encodes a solid-alpha RGBA PNG so a test can repaint a coverage texture on disk.
    fn coverage_png(alpha: u8) -> Vec<u8> {
        let pixels: Vec<u8> = (0..4).flat_map(|_| [255, 255, 255, alpha]).collect();
        let image = image::RgbaImage::from_raw(2, 2, pixels).expect("raw image");
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode png");
        bytes
    }

    #[test]
    fn repainting_a_coverage_texture_moves_the_cook_key() {
        // The staleness hole this closes: a material document names its coverage texture by id and
        // carries none of its pixels, so repainting the texture leaves the document byte-identical.
        // Hashing only the document therefore produced the SAME cook key for a different input, and
        // the cooker answered an edited cutout with the previous artifact. The coverage guards did
        // catch it at publication — but a guard is a staleness check, not a key, and a cache that
        // returns a hit never reaches the guard.
        let scratch = Scratch::new("coverage-repaint");
        let mut assets = scratch.assets();
        let texture = Uuid(7_402);
        let relative = "textures/leaf.png";
        std::fs::create_dir_all(assets.root.join("textures")).expect("create textures");
        std::fs::write(assets.root.join(relative), coverage_png(255)).expect("write texture");
        assets.catalog.put(AssetEntry {
            id: texture,
            name: "leaf".to_owned(),
            asset_type: AssetType::Texture,
            path: relative.to_owned(),
            ..AssetEntry::default()
        });
        let material_asset = MaterialAsset {
            surface: MaterialSurface::Standard,
            blend: "masked".to_owned(),
            albedo_texture: texture,
            ..MaterialAsset::default()
        };
        let material = save_material_asset(&mut assets, &material_asset, "Leaf", "plants")
            .expect("save material");
        let mesh = Uuid(7_403);
        std::fs::create_dir_all(assets.root.join("meshes")).expect("create meshes");
        std::fs::write(
            assets.root.join("meshes/leaf.smesh"),
            save_mesh_to_buffer(&triangle(), &[], None).expect("encode mesh"),
        )
        .expect("write mesh");
        assets.catalog.put(AssetEntry {
            id: mesh,
            name: "leaf".to_owned(),
            asset_type: AssetType::Mesh,
            path: "meshes/leaf.smesh".to_owned(),
            ..AssetEntry::default()
        });
        let family = save_family(&mut assets, imported_family(material, mesh));

        let before =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate before")
                .dependencies;
        // The texture must actually be an input, or the comparison below is between two cook keys
        // that never mentioned it and would agree for the wrong reason.
        let dependency_of = |dependencies: &[CookDependency]| {
            dependencies
                .iter()
                .find(|dependency| {
                    dependency.address == CookDependencyAddress::SourceAsset { asset: texture }
                })
                .map(|dependency| dependency.content_hash)
        };
        let hash_before =
            dependency_of(&before).expect("the coverage texture is a cook dependency");

        std::fs::write(assets.root.join(relative), coverage_png(64)).expect("repaint texture");
        assets.clear_asset_caches();
        let after =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate after")
                .dependencies;
        let hash_after = dependency_of(&after).expect("the coverage texture is still a dependency");

        assert_ne!(
            hash_before, hash_after,
            "repainting the coverage texture must move its dependency hash"
        );
        assert_ne!(before, after, "the cook key must move with it");
    }

    /// Builds a family whose one material carries a real coverage texture, and cooks it.
    fn cook_family_with_coverage(tag: &str) -> (Scratch, AssetServer, Vec<u8>, ContentHash) {
        let scratch = Scratch::new(tag);
        let mut assets = scratch.assets();
        let texture = Uuid(7_502);
        std::fs::create_dir_all(assets.root.join("textures")).expect("create textures");
        std::fs::write(assets.root.join("textures/leaf.png"), cutout_png(64))
            .expect("write texture");
        assets.catalog.put(AssetEntry {
            id: texture,
            name: "leaf".to_owned(),
            asset_type: AssetType::Texture,
            path: "textures/leaf.png".to_owned(),
            ..AssetEntry::default()
        });
        let material_asset = MaterialAsset {
            surface: MaterialSurface::Standard,
            blend: "masked".to_owned(),
            albedo_texture: texture,
            ..MaterialAsset::default()
        };
        let material = save_material_asset(&mut assets, &material_asset, "Leaf", "plants")
            .expect("save material");
        let mesh = Uuid(7_503);
        std::fs::create_dir_all(assets.root.join("meshes")).expect("create meshes");
        std::fs::write(
            assets.root.join("meshes/leaf.smesh"),
            save_mesh_to_buffer(&triangle(), &[], None).expect("encode mesh"),
        )
        .expect("write mesh");
        assets.catalog.put(AssetEntry {
            id: mesh,
            name: "leaf".to_owned(),
            asset_type: AssetType::Mesh,
            path: "meshes/leaf.smesh".to_owned(),
            ..AssetEntry::default()
        });
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        (scratch, assets, bytes, published.publication.content_hash)
    }

    #[test]
    fn the_published_atlas_reads_back_as_an_image_its_placements_fit_inside() {
        // The inspection surface the Plant workspace shows. It reads the PUBLISHED artifact rather
        // than re-packing: the family's UVs address exactly one layout, and a second packing of the
        // same slots would produce a different one — an atlas view that did that would show an
        // image the plant is not sampling.
        let (_scratch, assets, _bytes, hash) = cook_family_with_coverage("family-atlas-image");
        let image = crate::plant_family_atlas_image(&assets, hash, 0)
            .expect("atlas reads")
            .expect("a family with a coverage texture is atlased");
        assert!(image.level_count > 1, "the chain reached the view");
        assert!(image.width > 0 && image.height > 0);
        // Decodes as a real PNG at the extent it reports, which is what a viewer needs and what a
        // raw-bytes reply could get wrong without anything noticing.
        let decoded = image::load_from_memory(&image.png).expect("the reply decodes as PNG");
        assert_eq!(decoded.width(), image.width);
        assert_eq!(decoded.height(), image.height);
        // Every placement lands inside the atlas it claims to be in — a placement that did not is
        // how a slot samples a neighbour's texels.
        assert!(!image.placements.is_empty());
        for placement in &image.placements {
            assert!(placement.x + placement.width <= image.width);
            assert!(placement.y + placement.height <= image.height);
        }

        // A smaller level is what the renderer samples at distance, so it has to be reachable and
        // has to be smaller.
        let mip = crate::plant_family_atlas_image(&assets, hash, 1)
            .expect("level 1 reads")
            .expect("atlased");
        assert!(mip.width < image.width || mip.height < image.height);
        // And a level past the chain is an error rather than a silent clamp to the last one.
        assert!(crate::plant_family_atlas_image(&assets, hash, image.level_count).is_err());
    }

    #[test]
    fn the_published_hierarchy_reads_back_with_both_representations_and_their_errors() {
        // What the hierarchy view shows. It reads the PUBLISHED cut rather than re-cooking, because
        // the cut a view selects is chosen against the errors in these bytes — a freshly cooked
        // hierarchy would answer a question about a different plant than the one on screen.
        let (_scratch, assets, _bytes, hash) = cook_family_with_coverage("family-hierarchy");
        let hierarchy = crate::plant_family_hierarchy(&assets, hash).expect("hierarchy reads");
        assert!(!hierarchy.nodes.is_empty());

        let voxels = hierarchy
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.representation,
                    saffron_geometry::HierarchyRepresentation::Voxel { .. }
                )
            })
            .count();
        // Thin foliage cooks an aggregate form; a hierarchy of triangles alone would mean the view
        // has no representation transition to show and the cut control nothing to move between.
        assert!(voxels > 0, "the family cooked an aggregate node");

        // Exactly one root, and every other node's parent is a real node — the shape the view
        // indents by, and a cycle or a dangling parent would hang a walker rather than mis-draw.
        let roots = hierarchy
            .nodes
            .iter()
            .filter(|node| node.parent.is_none())
            .count();
        assert_eq!(roots, 1);
        for node in &hierarchy.nodes {
            if let Some(parent) = node.parent {
                assert!((parent as usize) < hierarchy.nodes.len());
            }
            // The total is the saturating sum the selector compares, so it can never read below a
            // component — a view showing a total under its own silhouette error would be lying
            // about which node gets picked.
            assert!(node.appearance_error.total >= node.appearance_error.silhouette);
        }
    }

    /// A minimal USD stage carrying one skeleton, matching the reader's own fixture shape.
    const USD_SKEL_STAGE: &str = r#"#usda 1.0
(
    upAxis = "Y"
)

def SkelRoot "BirchRig"
{
    def Skeleton "Birch"
    {
        uniform token[] joints = ["Root", "Root/Trunk", "Root/TrunkGuard", "Root/Trunk/Branch"]
        uniform matrix4d[] restTransforms = [
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (1, 2, 0, 1) ),
            ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 4, 0, 1) )
        ]
    }
}
"#;

    #[test]
    fn a_usd_stage_resolves_as_a_skeleton_source_input() {
        // The box asks for USD skeletons as SOURCE INPUTS, not merely as something inspectable.
        // Reading a stage and reporting its joints is a different capability from a recipe being
        // able to name that file and get a spine out of it, and only the second one lets a plant
        // bend the way its author rigged it.
        let resolved =
            resolve_usd_skeleton_source("rig.usda", USD_SKEL_STAGE.as_bytes()).expect("resolves");
        let joints = &resolved.snapshot.joints;
        assert_eq!(joints.len(), 4);
        // A USD source contributes STRUCTURE only — the meshes a plant renders come from its
        // geometry sources, and inventing an empty mesh here would give the compiler a source that
        // claims to draw nothing rather than one that claims not to draw.
        assert!(resolved.snapshot.meshes.is_empty());
        assert!(resolved.snapshot.materials.is_empty());

        // Parentage survives the translation, including the trap the reader guards: `Root/Trunk`
        // is a string prefix of `Root/TrunkGuard`, and hanging the guard off the trunk would bend
        // geometry the wrong way while passing every count and length check.
        assert_eq!(joints[0].parent, None);
        assert_eq!(joints[1].parent.as_ref(), Some(&joints[0].selector));
        assert_eq!(joints[2].parent.as_ref(), Some(&joints[0].selector));
        assert_eq!(joints[3].parent.as_ref(), Some(&joints[1].selector));

        // USD writes `matrix4d` row-major and `Mat4` is column-major, so a translation that
        // survives the transpose is the check that the two conventions were actually reconciled
        // rather than copied across.
        assert_eq!(joints[1].rest_transform.col(3).truncate().y, 2.0);
        assert_eq!(joints[3].rest_transform.col(3).truncate().y, 4.0);
        assert_eq!(joints[2].rest_transform.col(3).truncate().x, 1.0);

        // Identity by selector, not by position: re-importing an edited stage must match joints by
        // the path USD itself uses, or inserting one joint silently re-parents every later one.
        let PlantSourceSelector::Element { path, .. } = &joints[1].selector else {
            panic!("a joint selector is an addressable element");
        };
        assert_eq!(path, "Root/Trunk");
    }

    #[test]
    fn a_usd_stage_with_no_skeleton_is_refused_rather_than_imported_empty() {
        // A recipe naming a file with no skeleton in it is a mistake. Returning an empty joint list
        // would surface much later as a plant that refuses to bend, with nothing pointing at the
        // source that caused it.
        let bare = "#usda 1.0\n(\n    upAxis = \"Y\"\n)\n";
        assert!(resolve_usd_skeleton_source("bare.usda", bare.as_bytes()).is_err());
    }

    #[test]
    fn a_cooked_family_carries_its_atlas_and_uvs_that_address_it() {
        // The atlas and the UVs are one decision, not two. A family that ships a packed atlas but
        // slot-local UVs — or the reverse — samples texels the cook never placed there, and
        // nothing downstream can detect the disagreement because both halves look well-formed.
        // So this asserts they agree, on a real cooked artifact.
        let (_scratch, _assets, bytes, _hash) = cook_family_with_coverage("family-atlas");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = index
            .section(&bytes, PlantCompiledSectionKind::MaterialsCoverage)
            .unwrap()
            .unwrap();
        let (materials, atlas) = decode_material_section(section.as_ref()).expect("materials");
        assert!(!materials.is_empty(), "the family binds a material");
        let atlas = atlas.expect("a family with a coverage texture is atlased");

        // A layout with no placement for slot 0 would leave that slot's UVs unremapped while the
        // artifact still claimed an atlas — the exact half-applied state this box guards against.
        let placement = atlas
            .layout
            .placement(0)
            .expect("the one material slot has a placement");
        assert!(placement.width > 0 && placement.height > 0);
        assert!(placement.x + placement.width <= atlas.layout.width);
        assert!(placement.y + placement.height <= atlas.layout.height);
        // Level 0 plus a full chain down to 1x1; a single level would mean the coverage-preserving
        // mips never ran and distant foliage would thin out.
        assert!(atlas.levels.len() > 1, "the atlas carries a mip chain");
        assert_eq!(atlas.levels[0].width, atlas.layout.width);

        let rows = decode_mesh_section(
            index
                .section(&bytes, PlantCompiledSectionKind::Geometry)
                .unwrap()
                .unwrap()
                .as_ref(),
        )
        .expect("geometry");
        let mut sampled = 0_usize;
        for row in &rows {
            for vertex in &row.vertices {
                sampled += 1;
                let uv = [q16_to_f32(vertex.uv_bits[0]), q16_to_f32(vertex.uv_bits[1])];
                let u = uv[0] * atlas.layout.width as f32;
                let v = uv[1] * atlas.layout.height as f32;
                // Atlas-space UVs land inside the slot's own rectangle. Slot-local UVs would span
                // the whole atlas instead, which is what this catches.
                assert!(
                    u >= placement.x as f32 - 1.0
                        && u <= (placement.x + placement.width) as f32 + 1.0
                        && v >= placement.y as f32 - 1.0
                        && v <= (placement.y + placement.height) as f32 + 1.0,
                    "uv {uv:?} is outside slot 0's rectangle {placement:?}"
                );
            }
        }
        assert!(sampled > 0, "the family cooked vertices to check");
    }

    #[test]
    fn a_cooked_family_derives_micromaps_that_only_ever_remove_work() {
        // The derivation's whole licence is that it can only REMOVE classifier work: a
        // micro-triangle settles opaque or transparent only where a min/max pyramid over its
        // dilated UV footprint proves every point classifies that way, and anything else stays
        // unknown, where the classifier still runs. So the two assertions that matter are that
        // micromaps are produced at all — an empty set passes every "is it correct" check
        // vacuously — and that what they settled is internally consistent.
        let (_scratch, _assets, bytes, _hash) = cook_family_with_coverage("family-micromaps");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = index
            .section(&bytes, PlantCompiledSectionKind::RayTracing)
            .unwrap()
            .unwrap();
        let ray_tracing =
            saffron_geometry::decode_ray_tracing(section.as_ref()).expect("ray tracing section");
        assert!(
            !ray_tracing.opacity_micromaps.is_empty(),
            "a masked family with a coverage texture must derive at least one micromap"
        );
        for micromap in &ray_tracing.opacity_micromaps {
            let (opaque, transparent, unknown) = micromap.classes;
            assert!(
                opaque + transparent + unknown > 0,
                "a stored micromap classified no micro-triangles"
            );
            // Every usage row must account for triangles that exist, and the block a
            // non-negative index names must be present — a dangling index is the failure the
            // build turns into device loss rather than a validation message.
            let counted: u32 = micromap.usage.iter().map(|&(count, ..)| count).sum();
            assert!(counted as usize <= micromap.indices.len());
            for &block in &micromap.indices {
                if block >= 0 {
                    assert!(
                        (block as usize) < micromap.blocks.len(),
                        "index {block} names no block"
                    );
                }
            }
        }
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

    /// Splitting one source's faces across two materials must not move its geometry.
    ///
    /// A material names a surface, not a place. Two OBJs carrying byte-identical vertices must cook
    /// to byte-identical positions whether their faces sit in one `usemtl` run or two — otherwise a
    /// family that gains a second material silently relocates, which reads downstream as plants
    /// missing from the view rather than as anything to do with materials.
    #[test]
    fn a_second_material_run_does_not_move_the_cooked_geometry() {
        let single = cook_two_box_obj("obj-one-material", false);
        let split = cook_two_box_obj("obj-two-materials", true);
        assert_eq!(
            single.len(),
            split.len(),
            "the same geometry cooked to a different vertex count"
        );
        for (index, (one, two)) in single.iter().zip(&split).enumerate() {
            assert_eq!(
                one, two,
                "vertex {index} moved when the faces gained a second material"
            );
        }
    }

    /// Cooks one two-box OBJ — the boxes in one `usemtl` run or in two — and returns the published
    /// family's vertex positions as bit patterns, so a comparison is exact rather than approximate.
    fn cook_two_box_obj(tag: &str, split: bool) -> Vec<[u32; 3]> {
        let scratch = Scratch::new(tag);
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(
            source_dir.join("oak.mtl"),
            "newmtl Bark\nKd 0.6 0.4 0.2\nnewmtl Leaf\nKd 0.2 0.5 0.2\n",
        )
        .expect("material source");
        // Identical geometry in both cases; only the second run's `usemtl` differs.
        let second_material = if split { "usemtl Leaf\n" } else { "" };
        std::fs::write(
            source_dir.join("oak.obj"),
            format!(
                concat!(
                    "mtllib oak.mtl\n",
                    "o Trunk\nv -0.5 0 0\nv 0.5 0 0\nv 0 1 0\n",
                    "vt 0 0\nvt 1 0\nvt 0.5 1\nvn 0 0 1\n",
                    "usemtl Bark\nf 1/1/1 2/2/1 3/3/1\n",
                    "o Canopy\nv -0.5 1 0\nv 0.5 1 0\nv 0 2 0\n",
                    "{}f 4/1/1 5/2/1 6/3/1\n",
                ),
                second_material
            ),
        )
        .expect("geometry source");

        let material = Uuid(8_010);
        let source_uri = "sources/oak.obj".to_owned();
        let mut targets = vec![
            PlantManualSemanticTarget {
                id: 30,
                source: 10,
                selector: PlantSourceSelector::Whole,
                destination: PlantSemanticDestination::Part(40),
            },
            PlantManualSemanticTarget {
                id: 31,
                source: 11,
                selector: PlantSourceSelector::Element {
                    id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
                    path: "materials/Bark".to_owned(),
                },
                destination: PlantSemanticDestination::MaterialSlot(0),
            },
        ];
        let mut family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: Vec::new(),
            }),
        );
        if split {
            targets.push(PlantManualSemanticTarget {
                id: 33,
                source: 11,
                selector: PlantSourceSelector::Element {
                    id: u128::from(sub_id_for("oak", "material", "Leaf", 1).value()),
                    path: "materials/Leaf".to_owned(),
                },
                destination: PlantSemanticDestination::MaterialSlot(1),
            });
            family.material_slots.push(Uuid(8_011));
        }
        if let PlantFamilySource::Imported(recipe) = &mut family.source {
            recipe.semantic_targets = targets;
        }

        let id = crate::save_plant_family_asset(&mut assets, family, "Oak", "plants")
            .expect("register family");
        let family = crate::load_plant_family_asset(&assets, id).expect("reload family");
        let outcome = recook_plant_family(&mut assets, &family, &recook_options()).expect("recook");
        match outcome {
            PlantRecookOutcome::Published(published) => {
                let bytes = std::fs::read(&published.publication.path).expect("artifact");
                let decoded = crate::plant_render::decode_plant_render_sections(&bytes)
                    .expect("render-decode");
                decoded
                    .mesh
                    .vertices
                    .iter()
                    .map(|vertex| vertex.position.to_array().map(f32::to_bits))
                    .collect()
            }
            PlantRecookOutcome::Rejected(validation) => panic!(
                "rejected: {:#?} {:#?}",
                validation.compile.conflicts, validation.compile.diagnostics
            ),
        }
    }

    fn recook_options() -> PlantRecookOptions {
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

    /// A multi-part OBJ family binds each part through a submesh selector, not an element one.
    ///
    /// The OBJ importer collapses every `o` block into ONE node carrying ONE mesh, so a file with
    /// two objects still yields a single element and there is no second element identity to name.
    /// What survives the merge is the material run: faces are grouped into submeshes in first-seen
    /// `usemtl` order, which is what `PlantSourceSelector::Submesh` addresses.
    #[test]
    fn obj_submesh_selectors_bind_each_material_run_to_its_own_part() {
        let scratch = Scratch::new("obj-submesh");
        let mut assets = scratch.assets();
        let source_dir = assets.root.join("sources");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::write(
            source_dir.join("oak.mtl"),
            "newmtl Bark\nKd 0.6 0.4 0.2\nnewmtl Leaf\nKd 0.2 0.5 0.2\n",
        )
        .expect("material source");
        // Two objects with distinct `usemtl` runs. OBJ vertex indices are file-global and
        // one-based, so the canopy references the second triple.
        std::fs::write(
            source_dir.join("oak.obj"),
            concat!(
                "mtllib oak.mtl\n",
                "vt 0 0\n",
                "vt 1 0\n",
                "vt 0.5 1\n",
                "vn 0 0 1\n",
                "o Trunk\n",
                "v -0.5 0 0\n",
                "v 0.5 0 0\n",
                "v 0 1 0\n",
                "usemtl Bark\n",
                "f 1/1/1 2/2/1 3/3/1\n",
                "o Canopy\n",
                "v -0.5 1 0\n",
                "v 0.5 1 0\n",
                "v 0 2 0\n",
                "usemtl Leaf\n",
                "f 4/1/1 5/2/1 6/3/1\n",
            ),
        )
        .expect("geometry source");

        // The node the importer produced is named for the file stem and is the only one, so its
        // identity is fully determined — no guessing at what a second object would have been called.
        let element = u128::from(sub_id_for("oak", "mesh", "oak", 0).value());
        let trunk = PlantSourceSelector::Submesh { element, index: 0 };
        let canopy = PlantSourceSelector::Submesh { element, index: 1 };
        let material = Uuid(8_002);
        let source_uri = "sources/oak.obj".to_owned();
        let mut family = base_family(
            material,
            PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
                sources: vec![
                    PlantSourceReference {
                        id: 10,
                        locator: PlantSourceLocator::File(source_uri.clone()),
                        role: PlantSourceRole::Geometry,
                        selector: PlantSourceSelector::Whole,
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                    PlantSourceReference {
                        id: 11,
                        locator: PlantSourceLocator::File(source_uri),
                        role: PlantSourceRole::Material,
                        selector: PlantSourceSelector::Whole,
                        // Geometry and material read one file, so they must agree on its bytes
                        // and on how to read them: one address carrying two contents is an error.
                        content_hash: [1; 32],
                        settings: PlantImportSettings::default(),
                        provenance: provenance(),
                    },
                ],
                semantic_targets: vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: trunk,
                        destination: PlantSemanticDestination::Part(40),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 10,
                        selector: canopy,
                        destination: PlantSemanticDestination::Part(41),
                    },
                    PlantManualSemanticTarget {
                        id: 32,
                        source: 11,
                        selector: PlantSourceSelector::Element {
                            id: u128::from(sub_id_for("oak", "material", "Bark", 0).value()),
                            path: "materials/Bark".to_owned(),
                        },
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                    // Every imported material needs exactly one slot target: a second material run
                    // with nowhere to land fails the whole source, not just its own binding.
                    PlantManualSemanticTarget {
                        id: 33,
                        source: 11,
                        selector: PlantSourceSelector::Element {
                            id: u128::from(sub_id_for("oak", "material", "Leaf", 1).value()),
                            path: "materials/Leaf".to_owned(),
                        },
                        destination: PlantSemanticDestination::MaterialSlot(1),
                    },
                ],
            }),
        );
        family.material_slots.push(Uuid(8_003));
        family.parts.push(PlantPart {
            id: 41,
            parent: Some(40),
            semantic: PlantPartSemantic::Leaf,
            material_slot: 1,
            sources: vec![10],
        });

        let validation =
            validate_plant_family_sources(&mut assets, &family, PlantCompileLimits::default())
                .expect("validate OBJ source");
        assert!(
            validation.compile.publishable(),
            "{:?}",
            validation.compile.diagnostics
        );
        // Both material runs survived as submeshes of the one merged element — the premise the
        // canopy selector rests on, asserted rather than assumed.
        assert_eq!(validation.compile.statistics.materials, 2);
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
        probe_fixture("vegetation-phase3.json");
    }

    /// The canopy (two-submesh, two-slot) family is the multi-object shape — the checked-in
    /// repro for the phase-11 multi-object render loss — and it must stay cookable.
    #[test]
    fn seasonal_fixture_family_publishes_against_the_current_compiler() {
        probe_fixture("vegetation-canopy.json");
    }

    fn probe_fixture(file: &str) {
        let fixture_path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/e2e/fixtures"
        ))
        .join(file);
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

        let root = std::env::temp_dir().join(format!(
            "saffron-fixture-probe-{}-{file}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let mut assets = AssetServer::new(root.join("assets"));
        let full = assets.root.join(trunk_path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, &trunk).unwrap();
        if let (Some(mtl_hex), Some(mtl_path)) =
            (json["trunkMtlHex"].as_str(), json["trunkMtlPath"].as_str())
        {
            std::fs::write(assets.root.join(mtl_path), from_hex(mtl_hex)).unwrap();
        }
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
                let hierarchy = &decoded.hierarchy;
                eprintln!(
                    "PROBE {file}: prototypes={} uses={} combinations={:?} words={:?} roots={:?} nodes={}",
                    hierarchy.prototypes.len(),
                    hierarchy.micro_instances.len(),
                    hierarchy
                        .combinations
                        .iter()
                        .map(|combination| (combination.variation, combination.phenotype))
                        .collect::<Vec<_>>(),
                    hierarchy
                        .combinations
                        .iter()
                        .map(|combination| combination.active_words.clone())
                        .collect::<Vec<_>>(),
                    hierarchy.roots,
                    hierarchy.nodes.len(),
                );
                for (index, instance) in hierarchy.micro_instances.iter().enumerate() {
                    eprintln!(
                        "PROBE {file}: use[{index}] prototype={} part={:x} transform={:?}",
                        instance.prototype,
                        instance.part,
                        &instance.transform_bits[..12],
                    );
                }
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
