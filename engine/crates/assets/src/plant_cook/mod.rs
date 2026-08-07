//! Plant-source resolution and atomic `.splantc` publication.

mod atlas;
mod decode;
mod distance_field;
mod import_source;
mod materials;
mod publish;
mod sections;
mod sources;

#[cfg(test)]
mod test_support;

pub(crate) use decode::{
    PlantGeometryMesh, PlantPhenotypeRow, decode_family_atlas, decode_material_section,
    decode_mesh_section, decode_phenotype_section,
};
pub(crate) use materials::decode_plant_material_document;

use std::collections::BTreeMap;
use std::time::Instant;

use saffron_core::Uuid;
use saffron_geometry::VirtualHierarchyMaterial;
use saffron_spatial::DecisionScalar;
use saffron_vegetation::{
    ContentHash, CookDependency, CookDependencyAddress, CookNodeAddress, CookNodeRecord,
    CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate, PLANT_ASSET_VERSION,
    PLANT_COMPILED_ARTIFACT_VERSION, PLANT_SOURCE_COMPILER_VERSION, PlantCompileDiagnostic,
    PlantCompileLimits, PlantCompileOutput, PlantCompiledArtifactHeader, PlantFamilyAsset,
    PlantFamilySource, PlantSourceLocator, PlantSourceSnapshot, compile_plant_family,
    plant_asset_schema_hash, plant_compiled_artifact_schema_hash, plant_hierarchy_material,
    write_plant_asset, write_plant_compiled_artifact,
};

use crate::cook_reader::CookAssetAccess;
use crate::vegetation::update_plant_family_asset;
use crate::{AssetServer, Error, Result, VegetationArtifactPublication};

use self::materials::{ResolvedCoverageImages, ResolvedMaterialDocuments};
use self::publish::{build_plant_sections, validate_complete_plant_artifact};
use self::sources::resolve_plant_inputs;

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
    material_documents: ResolvedMaterialDocuments,
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

/// Measured input size of a dependency set: one 32-byte content hash per entry.
pub(crate) fn dependency_input_bytes(dependencies: &[CookDependency]) -> u64 {
    u64::try_from(dependencies.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(32)
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
        input_bytes: dependency_input_bytes(&validation.dependencies),
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

fn plant_dependencies(
    asset: &PlantFamilyAsset,
    snapshots: &[PlantSourceSnapshot],
    material_documents: &ResolvedMaterialDocuments,
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
    // move when the texture's pixels do. The cook reads those pixels — they drive the geometry-first
    // contour and the opacity micromap — so the texture belongs in the key that identifies the
    // artifact. Two materials sharing one texture name one dependency, through `insert_dependency`.
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
            "plant-compiled-artifact-schema-v4",
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

// The section writers encode these exact versions, so a bump upstream must revisit them here.
const _: () = assert!(PLANT_ASSET_VERSION == 7);
const _: () = assert!(PLANT_COMPILED_ARTIFACT_VERSION == 6);

#[cfg(test)]
mod tests {
    use super::test_support::{
        family_calling, fixture_server, imported_family, native_family, options, save_family,
    };
    use super::*;
    use crate::{MaterialAsset, load_plant_family_asset, save_material_asset};
    use saffron_vegetation::PlantReimportConflictReason;
    use saffron_vegetation::{
        PlantCompiledArtifactIndex, PlantCompiledSectionKind, PlantSourceSelector,
    };
    use std::path::Path;

    #[test]
    fn a_module_call_composes_the_referenced_family() {
        // A preset authored once, called from another family, reaches the compiled result.
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

    /// The checked-in vegetation E2E fixture families must stay cookable against the current
    /// compiler. Regenerate with `cargo run -p xtask -- gen-vegetation-e2e-fixture` when a
    /// format changes.
    #[test]
    fn fixture_family_publishes_against_the_current_compiler() {
        probe_fixture("vegetation-phase3.json");
    }

    /// The canopy (two-submesh, two-slot) family is the multi-object shape, and it must stay
    /// cookable.
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

        let root = std::env::temp_dir().join(format!(
            "saffron-fixture-probe-{}-{file}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let mut assets = AssetServer::new(root.join("assets"));
        for source in json["sources"].as_array().unwrap() {
            let full = assets.root.join(source["path"].as_str().unwrap());
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, from_hex(source["hex"].as_str().unwrap())).unwrap();
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
                assert!(
                    !decoded.hierarchy.nodes.is_empty(),
                    "the fixture family carries a hierarchy"
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
