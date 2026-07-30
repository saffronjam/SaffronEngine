//! Unit tests for the one plant-family compile path.

use saffron_geometry::glam::{Mat4, Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, Vertex, compute_tangents};
use saffron_spatial::{DecisionScalar, UnitInterval};

use super::*;
use crate::{
    AlphaClassification, CoverageSource, ImportedPlantFamilyRecipe, InteractionPolicy,
    MaterialSurface, MechanicalResponse, PLANT_ASSET_VERSION, PhenotypeRole, PlantDimensions,
    PlantFamilySource, PlantImportSettings, PlantManualSemanticTarget, PlantPart,
    PlantPartSemantic, PlantPhenotype, PlantPivot, PlantSemanticDestination, PlantSourceLocator,
    PlantSourceReference, PlantSourceRole, PlantSourceSelector, PlantVariation, SourceAxis,
    SourceHandedness, SourceProvenance,
};

fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).unwrap()
}

fn selector(id: u128, path: &str) -> PlantSourceSelector {
    PlantSourceSelector::Element {
        id,
        path: path.to_owned(),
    }
}

fn provenance() -> SourceProvenance {
    SourceProvenance {
        source: "fixture".to_owned(),
        source_uri: "file:///oak.glb".to_owned(),
        license_id: "CC0-1.0".to_owned(),
        license_uri: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
        author: "Fixture".to_owned(),
        attribution: "Oak fixture".to_owned(),
        requires_attribution: false,
    }
}

fn asset() -> PlantFamilyAsset {
    let source = PlantSourceReference {
        id: 10,
        locator: PlantSourceLocator::Asset(Uuid(1_100)),
        role: PlantSourceRole::Geometry,
        selector: selector(20, "oak/leaves"),
        content_hash: [1; 32],
        settings: PlantImportSettings {
            pivot: PlantPivot::SourceOrigin,
            ..PlantImportSettings::default()
        },
        provenance: provenance(),
    };
    PlantFamilyAsset {
        role: crate::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: crate::MAX_PLANT_MODULE_RECURSION,
        version: PLANT_ASSET_VERSION,
        id: Uuid(2_000),
        name: "Oak".to_owned(),
        tags: vec![crate::PlantTagId::new(17).unwrap()],
        source: PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
            sources: vec![source],
            semantic_targets: vec![PlantManualSemanticTarget {
                id: 30,
                source: 10,
                selector: selector(20, "oak/leaves"),
                destination: PlantSemanticDestination::Part(40),
            }],
        }),
        parts: vec![PlantPart {
            id: 40,
            parent: None,
            semantic: PlantPartSemantic::Leaf,
            material_slot: 0,
            sources: vec![10],
        }],
        dimensions: PlantDimensions {
            height: fixed(2),
            trunk_radius: DecisionScalar::from_bits(0),
            crown_radius: [fixed(2); 2],
            root_radius: [fixed(1); 2],
            local_bounds_min: [fixed(-2), fixed(-1), fixed(-2)],
            local_bounds_max: [fixed(2), fixed(2), fixed(2)],
        },
        material_slots: vec![Uuid(3_000)],
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
            sources: vec![10],
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
        ecology: crate::PlantEcologyDeclaration::default(),
    }
}

fn triangle() -> Mesh {
    let mut mesh = Mesh {
        vertices: vec![
            Vertex {
                position: Vec3::new(-1.0, 0.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(1.0, 0.0, 0.0),
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

fn snapshot() -> PlantSourceSnapshot {
    PlantSourceSnapshot {
        source: 10,
        content_hash: [1; 32],
        meshes: vec![PlantSourceMeshSnapshot {
            selector: selector(20, "oak/leaves"),
            transform: Mat4::IDENTITY,
            mesh: triangle(),
            skin: Vec::new(),
            material_slots: vec![Uuid(3_000)],
        }],
        materials: vec![PlantSourceMaterialSnapshot {
            selector: selector(21, "oak/leaf-material"),
            material: Uuid(3_000),
            content_hash: [3; 32],
            surface: MaterialSurface::Standard,
            alpha_classification: AlphaClassification::Opaque,
            coverage_source: CoverageSource::ModeledGeometry,
        }],
        joints: Vec::new(),
        semantic_elements: Vec::new(),
    }
}

#[test]
fn compile_is_byte_identical_under_snapshot_order_changes() {
    let asset = asset();
    let first = compile_plant_family(
        &asset,
        &[snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    let second = compile_plant_family(
        &asset,
        &[snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(first.publishable());
    assert_eq!(first.family_hash, second.family_hash);
    assert_eq!(first.family, second.family);
    assert_eq!(
        first.family.unwrap().tags,
        vec![crate::PlantTagId::new(17).unwrap()]
    );
}

#[test]
fn family_tags_participate_in_the_normalized_hash() {
    let original = asset();
    let original_hash = compile_plant_family(
        &original,
        &[snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap()
    .family_hash;
    let mut changed = original;
    changed.tags = vec![crate::PlantTagId::new(19).unwrap()];
    let changed_hash = compile_plant_family(
        &changed,
        &[snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap()
    .family_hash;
    assert_ne!(original_hash, changed_hash);
}

#[test]
fn left_handed_source_flips_winding_and_tangent_handedness() {
    let mut asset = asset();
    let PlantFamilySource::Imported(recipe) = &mut asset.source else {
        unreachable!();
    };
    recipe.sources[0].settings.handedness = SourceHandedness::Left;
    recipe.sources[0].settings.forward_axis = SourceAxis::PositiveZ;
    let output = compile_plant_family(
        &asset,
        &[snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable(), "{:?}", output.diagnostics);
    let mesh = &output.family.unwrap().meshes[0];
    assert_eq!(mesh.indices, vec![0, 2, 1]);
    assert!(mesh.vertices[0].tangent_snorm[3] < 0);
}

#[test]
fn source_vertex_offsets_are_canonicalized_and_unreferenced_vertices_are_removed() {
    let asset = asset();
    let mut source = snapshot();
    let referenced = source.meshes[0].mesh.vertices.clone();
    source.meshes[0].mesh.vertices = vec![Vertex::default(); 3];
    source.meshes[0].mesh.vertices.extend(referenced);
    source.meshes[0].mesh.submeshes[0].vertex_offset = 3;
    let output = compile_plant_family(
        &asset,
        &[source],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable(), "{:?}", output.diagnostics);
    assert_eq!(output.family.unwrap().meshes[0].vertices.len(), 3);
}

#[test]
fn disappeared_manual_target_blocks_publication_without_dropping_target() {
    let asset = asset();
    let mut source = snapshot();
    source.meshes[0].selector = selector(99, "oak/renamed-leaves");
    let output = compile_plant_family(
        &asset,
        &[source],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(!output.publishable());
    assert_eq!(output.conflicts.conflicts.len(), 1);
    assert_eq!(output.conflicts.conflicts[0].target, 30);
    assert_eq!(
        output.conflicts.conflicts[0].destination,
        PlantSemanticDestination::Part(40)
    );
}

#[test]
fn content_change_is_reported_and_accepted_by_the_same_compile_path() {
    let asset = asset();
    let mut source = snapshot();
    source.content_hash = [9; 32];
    let output = compile_plant_family(
        &asset,
        &[source],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable());
    assert_eq!(output.source_updates.len(), 1);
    assert_eq!(output.source_updates[0].current, [9; 32]);
}

#[test]
fn coverage_material_rejects_zero_uv_area() {
    let asset = asset();
    let mut source = snapshot();
    for vertex in &mut source.meshes[0].mesh.vertices {
        vertex.uv0 = Vec2::ZERO;
    }
    compute_tangents(&mut source.meshes[0].mesh);
    source.materials[0].alpha_classification = AlphaClassification::Masked;
    source.materials[0].coverage_source = CoverageSource::AlbedoAlpha;
    let output = compile_plant_family(
        &asset,
        &[source],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(!output.publishable());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == PlantCompileDiagnosticCode::MissingCoverageUv })
    );
}

#[test]
fn a_graft_normalizes_through_the_imported_source_path() {
    let mut asset = asset();
    let mut graph = crate::BotanicalGraphDocument::sapling(0x5a11);
    let grown = crate::grow(
        &graph,
        0,
        &crate::NoBotanicalModules,
        &crate::BotanicalBudget::COOK,
    )
    .unwrap()
    .assembly;
    let leaf = grown.elements[0];
    let hero = PlantSourceReference {
        id: 77,
        locator: PlantSourceLocator::Asset(Uuid(1_100)),
        role: PlantSourceRole::Geometry,
        selector: selector(20, "oak/leaves"),
        content_hash: [1; 32],
        settings: PlantImportSettings {
            pivot: PlantPivot::SourceOrigin,
            ..PlantImportSettings::default()
        },
        provenance: provenance(),
    };
    graph.edits = vec![crate::BotanicalManualEdit {
        target: leaf.id,
        action: crate::BotanicalEditAction::Graft {
            source: hero.id,
            selector: selector(20, "oak/leaves"),
        },
    }];
    asset.source = PlantFamilySource::Native {
        graph: graph.clone(),
        grafts: vec![hero.clone()],
    };
    asset.parts[0].sources.clear();
    asset.variations[0].sources = vec![native_variation_source_id(0)];

    let mut native = snapshot();
    native.source = native_plant_source_id(asset.id);
    native.content_hash = native_botanical_graph_content_hash(&graph);
    native.meshes.clear();
    native.joints.clear();
    let mut grafted = snapshot();
    grafted.source = hero.id;
    grafted.content_hash = hero.content_hash;

    let output = compile_plant_family(
        &asset,
        &[native, grafted],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable(), "{:?}", output.diagnostics);
    assert_eq!(output.statistics.grafts, 1);
    let family = output.family.unwrap();
    assert_eq!(family.meshes.len(), 2);
    let graft_mesh = family
        .meshes
        .iter()
        .find(|mesh| mesh.source == hero.id)
        .expect("the graft reached the family");
    assert!(!graft_mesh.vertices.is_empty());
    assert!(
        graft_mesh
            .skin
            .iter()
            .all(|skin| skin.weights[0] == UnitInterval::ONE.bits()),
        "a graft is rigid on the limb it stands on"
    );
    let near = graft_mesh.vertices.iter().any(|vertex| {
        (0..3)
            .all(|lane| (vertex.position_bits[lane] - leaf.position[lane].bits()).abs() < (4 << 16))
    });
    assert!(near, "the graft stands on its frame");
    assert_eq!(family.sources.len(), 2);

    let mut lonely = snapshot();
    lonely.source = native_plant_source_id(asset.id);
    lonely.content_hash = native_botanical_graph_content_hash(&graph);
    lonely.meshes.clear();
    lonely.joints.clear();
    let missing = compile_plant_family(
        &asset,
        &[lonely],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(!missing.publishable());
    assert!(
        missing
            .diagnostics
            .iter()
            .any(|entry| entry.code == PlantCompileDiagnosticCode::MissingSource)
    );
}

#[test]
fn native_source_uses_the_shared_normalized_family_contract() {
    let mut asset = asset();
    let graph = crate::BotanicalGraphDocument::sapling(0x5a11);
    asset.source = PlantFamilySource::Native {
        graph: graph.clone(),
        grafts: Vec::new(),
    };
    asset.parts[0].sources.clear();
    asset.variations[0].sources = vec![native_variation_source_id(0)];
    let mut source = snapshot();
    source.source = native_plant_source_id(asset.id);
    source.content_hash = native_botanical_graph_content_hash(&graph);
    source.meshes.clear();
    source.joints.clear();
    let output = compile_plant_family(
        &asset,
        &[source],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable(), "{:?}", output.diagnostics);
    let family = output.family.unwrap();
    assert_eq!(family.meshes.len(), 1);
    assert!(!family.meshes[0].vertices.is_empty());
    assert_eq!(family.meshes[0].skin.len(), family.meshes[0].vertices.len());
    assert!(!family.joints.is_empty());
    assert!(family.dimensions.height.bits() > 0);
    assert_eq!(family.materials.len(), 1);
    assert_eq!(family.sources[0].0, native_plant_source_id(asset.id));
    assert!(output.statistics.vertices > 0 && output.statistics.joints > 0);
}

fn two_submesh_mesh() -> Mesh {
    let quad = |base: f32| {
        [
            Vertex {
                position: Vec3::new(-1.0, base, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(1.0, base, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(0.0, base + 1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.5, 1.0),
                ..Vertex::default()
            },
        ]
    };
    let mut mesh = Mesh {
        vertices: quad(0.0).into_iter().chain(quad(1.0)).collect(),
        indices: vec![0, 1, 2, 3, 4, 5],
        submeshes: vec![
            Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            },
            Submesh {
                first_index: 3,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 1,
            },
        ],
    };
    compute_tangents(&mut mesh);
    mesh
}

fn two_part_asset(partitioned: bool) -> PlantFamilyAsset {
    let mut asset = asset();
    asset.parts = vec![
        PlantPart {
            id: 40,
            parent: None,
            semantic: PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: vec![10],
        },
        PlantPart {
            id: 41,
            parent: Some(40),
            semantic: PlantPartSemantic::Leaf,
            material_slot: 1,
            sources: vec![10],
        },
    ];
    asset.material_slots = vec![Uuid(3_000), Uuid(3_001)];
    let PlantFamilySource::Imported(recipe) = &mut asset.source else {
        unreachable!("the fixture asset is an imported recipe");
    };
    recipe.semantic_targets = if partitioned {
        vec![
            PlantManualSemanticTarget {
                id: 30,
                source: 10,
                selector: PlantSourceSelector::Submesh {
                    element: 20,
                    index: 0,
                },
                destination: PlantSemanticDestination::Part(40),
            },
            PlantManualSemanticTarget {
                id: 31,
                source: 10,
                selector: PlantSourceSelector::Submesh {
                    element: 20,
                    index: 1,
                },
                destination: PlantSemanticDestination::Part(41),
            },
        ]
    } else {
        vec![
            PlantManualSemanticTarget {
                id: 30,
                source: 10,
                selector: selector(20, "oak/leaves"),
                destination: PlantSemanticDestination::Part(40),
            },
            PlantManualSemanticTarget {
                id: 31,
                source: 10,
                selector: selector(20, "oak/leaves"),
                destination: PlantSemanticDestination::Part(41),
            },
        ]
    };
    asset
}

fn two_submesh_snapshot() -> PlantSourceSnapshot {
    let mut source = snapshot();
    source.meshes[0].mesh = two_submesh_mesh();
    source.meshes[0].material_slots = vec![Uuid(3_000), Uuid(3_001)];
    source.materials.push(PlantSourceMaterialSnapshot {
        selector: selector(22, "oak/bark-material"),
        material: Uuid(3_001),
        content_hash: [4; 32],
        surface: MaterialSurface::Standard,
        alpha_classification: AlphaClassification::Opaque,
        coverage_source: CoverageSource::ModeledGeometry,
    });
    source
}

#[test]
fn part_submesh_targets_partition_the_source_into_per_part_rows() {
    let asset = two_part_asset(true);
    let output = compile_plant_family(
        &asset,
        &[two_submesh_snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(output.publishable(), "{:?}", output.diagnostics);
    let family = output.family.unwrap();
    let selectors = family
        .meshes
        .iter()
        .map(|mesh| mesh.selector.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        selectors,
        vec![
            PlantSourceSelector::Submesh {
                element: 20,
                index: 0
            },
            PlantSourceSelector::Submesh {
                element: 20,
                index: 1
            },
        ]
    );
    assert!(
        family
            .meshes
            .iter()
            .all(|mesh| mesh.submeshes.len() == 1 && mesh.vertices.len() == 3)
    );
    let hierarchy = crate::plant_hierarchy_input(&asset, &family, &[]).unwrap();
    assert_eq!(hierarchy.meshes.len(), 2);
    let placements = hierarchy
        .micro_instances
        .iter()
        .map(|instance| (instance.prototype, instance.part))
        .collect::<Vec<_>>();
    assert_eq!(placements, vec![(0, 40), (1, 41)]);
}

#[test]
fn several_parts_on_an_unpartitioned_source_are_refused() {
    let output = compile_plant_family(
        &two_part_asset(false),
        &[two_submesh_snapshot()],
        PlantCompileLimits::default(),
        &crate::NoBotanicalModules,
    )
    .unwrap();
    assert!(!output.publishable());
    assert!(output.diagnostics.iter().any(|entry| {
        entry.code == PlantCompileDiagnosticCode::InvalidGeometry
            && entry.path == "source.imported.semanticTargets"
    }));
}
