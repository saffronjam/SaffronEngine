//! The authored plant family each fixture recipe compiles from its source files.

use saffron_spatial::{DecisionScalar, UnitInterval};
use saffron_vegetation::{
    ImportedPlantFamilyRecipe, InteractionPolicy, MechanicalResponse, PLANT_ASSET_VERSION,
    PhenotypeResponse, PhenotypeRole, PlantCollisionProxy, PlantCollisionShape, PlantDimensions,
    PlantFamilyAsset, PlantFamilySource, PlantImportSettings, PlantManualSemanticTarget,
    PlantNavigationProxy, PlantPart, PlantPartSemantic, PlantPhenotype, PlantSemanticDestination,
    PlantSourceLocator, PlantSourceReference, PlantSourceRole, PlantSourceSelector, PlantTagId,
    PlantVariation, SourceProvenance, vegetation_content_hash,
};

use super::source::PlantContent;
use super::{CANOPY_MATERIAL, DEFAULT_MATERIAL, Recipe, fixed};

fn provenance() -> SourceProvenance {
    SourceProvenance {
        source: "fixture".to_owned(),
        source_uri: "generated://e2e-birch-trunk".to_owned(),
        license_id: "CC0-1.0".to_owned(),
        license_uri: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
        author: "Fixture".to_owned(),
        attribution: "E2E birch trunk".to_owned(),
        requires_attribution: false,
    }
}

/// The imported element selector for one of the content's materials. The id derives from the
/// source file's stem exactly as the model importer bakes sub-assets.
fn material_selector(content: PlantContent, stem: &str, slot: usize) -> PlantSourceSelector {
    let name = content.material_names()[slot];
    PlantSourceSelector::Element {
        id: u128::from(saffron_geometry::sub_id_for(stem, "material", name, slot as u32).value()),
        path: format!("materials/{name}"),
    }
}

/// The imported mesh element the two-part contents split into submeshes. Both importers name the
/// single mesh-bearing node after the source file.
fn mesh_element(stem: &str) -> u128 {
    u128::from(saffron_geometry::sub_id_for(stem, "mesh", stem, 0).value())
}

pub(super) fn plant(recipe: &Recipe, primary_source: &[u8]) -> PlantFamilyAsset {
    let content = recipe.content;
    let height = recipe.trunk_height;
    let stem = recipe.stem;
    let source_uri = content.primary_path(stem);
    let content_hash = vegetation_content_hash(primary_source);
    let mut variations = vec![PlantVariation {
        id: 0,
        name: "Default".to_owned(),
        sources: vec![10, 11],
        active_parts: Vec::new(),
    }];
    let mut phenotypes = vec![PlantPhenotype {
        id: 0,
        role: PhenotypeRole::Healthy,
        response: PhenotypeResponse::default(),
        variation: 0,
        material_remap: Vec::new(),
        active_parts: Vec::new(),
    }];
    if recipe.seasonal {
        variations.push(PlantVariation {
            id: 1,
            name: "Autumn".to_owned(),
            sources: vec![10, 11],
            active_parts: Vec::new(),
        });
        // The senescent phenotype drops the crown — only the trunk part stays active — so a
        // phenotype flip is a visible geometry change, not a copy of the healthy one.
        phenotypes.push(PlantPhenotype {
            id: 1,
            role: PhenotypeRole::Senescent,
            response: PhenotypeResponse::default(),
            variation: 1,
            material_remap: Vec::new(),
            active_parts: if content.two_parts() {
                vec![1]
            } else {
                Vec::new()
            },
        });
    }
    PlantFamilyAsset {
        role: saffron_vegetation::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
        version: PLANT_ASSET_VERSION,
        id: recipe.plant_id,
        name: "E2E silver birch".to_owned(),
        tags: vec![PlantTagId::new(1).expect("nonzero fixture tag")],
        source: PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
            sources: vec![
                PlantSourceReference {
                    id: 10,
                    locator: PlantSourceLocator::File(source_uri.clone()),
                    role: PlantSourceRole::Geometry,
                    selector: PlantSourceSelector::Whole,
                    content_hash,
                    settings: PlantImportSettings::default(),
                    provenance: provenance(),
                },
                PlantSourceReference {
                    id: 11,
                    locator: PlantSourceLocator::File(source_uri),
                    role: PlantSourceRole::Material,
                    selector: if content.two_parts() {
                        PlantSourceSelector::Whole
                    } else {
                        material_selector(content, stem, 0)
                    },
                    content_hash,
                    settings: PlantImportSettings::default(),
                    provenance: provenance(),
                },
            ],
            semantic_targets: if content.two_parts() {
                // Two material runs → two submeshes of the one merged element: the trunk part
                // takes submesh 0, the leaf part submesh 1, each material run its own slot.
                let element = mesh_element(stem);
                vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: PlantSourceSelector::Submesh { element, index: 0 },
                        destination: PlantSemanticDestination::Part(1),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 10,
                        selector: PlantSourceSelector::Submesh { element, index: 1 },
                        destination: PlantSemanticDestination::Part(2),
                    },
                    PlantManualSemanticTarget {
                        id: 32,
                        source: 11,
                        selector: material_selector(content, stem, 0),
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                    PlantManualSemanticTarget {
                        id: 33,
                        source: 11,
                        selector: material_selector(content, stem, 1),
                        destination: PlantSemanticDestination::MaterialSlot(1),
                    },
                ]
            } else {
                vec![
                    PlantManualSemanticTarget {
                        id: 30,
                        source: 10,
                        selector: PlantSourceSelector::Whole,
                        destination: PlantSemanticDestination::Part(1),
                    },
                    PlantManualSemanticTarget {
                        id: 31,
                        source: 11,
                        selector: material_selector(content, stem, 0),
                        destination: PlantSemanticDestination::MaterialSlot(0),
                    },
                ]
            },
        }),
        parts: if content.two_parts() {
            vec![
                PlantPart {
                    id: 1,
                    parent: None,
                    semantic: PlantPartSemantic::Trunk,
                    material_slot: 0,
                    sources: vec![10],
                },
                PlantPart {
                    id: 2,
                    parent: Some(1),
                    semantic: PlantPartSemantic::Leaf,
                    material_slot: 1,
                    sources: vec![10],
                },
            ]
        } else {
            vec![PlantPart {
                id: 1,
                parent: None,
                semantic: PlantPartSemantic::Trunk,
                material_slot: 0,
                sources: vec![10],
            }]
        },
        dimensions: PlantDimensions {
            height: fixed(height),
            trunk_radius: fixed(1),
            crown_radius: [fixed(2); 2],
            root_radius: [fixed(2); 2],
            local_bounds_min: [fixed(-2), fixed(0), fixed(-2)],
            local_bounds_max: [
                fixed(2),
                // The crown rises above the trunk, and the authored conservative bounds must
                // contain it or the cook rejects at dimensions.localBounds.
                fixed(height + content.crown_extent()),
                fixed(2),
            ],
        },
        material_slots: if content.two_parts() {
            vec![DEFAULT_MATERIAL, CANOPY_MATERIAL]
        } else {
            vec![DEFAULT_MATERIAL]
        },
        spines: Vec::new(),
        mechanics: MechanicalResponse {
            stiffness: fixed(1),
            damping: UnitInterval::from_bits(32_768),
            drag: fixed(1),
            flutter: DecisionScalar::from_bits(16_384),
            bend_limit: UnitInterval::from_bits(32_768),
            damage_threshold: fixed(2),
            break_threshold: fixed(4),
        },
        variations,
        phenotypes,
        // A trunk capsule and a square footprint, so the physics and navigation facets have real
        // proxies to derive from (a family with none contributes neither a body nor an obstacle).
        collision_proxies: vec![PlantCollisionProxy {
            id: 0x0c01,
            shape: PlantCollisionShape::Capsule,
            part: 1,
            center: [fixed(0), fixed(height / 2), fixed(0)],
            dimensions: [fixed(1), fixed(height / 2), fixed(1)],
            breakable: true,
        }],
        navigation_proxies: vec![PlantNavigationProxy {
            id: 0x0f01,
            footprint: vec![
                [fixed(-1), fixed(-1)],
                [fixed(1), fixed(-1)],
                [fixed(1), fixed(1)],
                [fixed(-1), fixed(1)],
            ],
            height: fixed(height),
            cost: UnitInterval::ONE,
        }],
        interaction_policy: InteractionPolicy::Structural,
        habitat: None,
        ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
    }
}
