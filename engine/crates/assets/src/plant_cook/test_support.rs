//! Shared fixtures for the plant-cook tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, Vertex, compute_tangents, save_mesh_to_buffer};
use saffron_scene::{AssetEntry, AssetType};
use saffron_spatial::{DecisionScalar, UnitInterval};
use saffron_vegetation::{
    BotanicalGraphDocument, ContentHash, CookPlatformProfile, CookVersionSet,
    ImportedPlantFamilyRecipe, InteractionPolicy, MaterialSurface, MechanicalResponse,
    PLANT_ASSET_VERSION, PhenotypeRole, PlantCompileLimits, PlantDimensions, PlantFamilyAsset,
    PlantFamilySource, PlantImportSettings, PlantManualSemanticTarget, PlantPart,
    PlantPartSemantic, PlantPhenotype, PlantPivot, PlantSemanticDestination, PlantSourceLocator,
    PlantSourceReference, PlantSourceRole, PlantSourceSelector, PlantVariation, SourceProvenance,
};

use crate::plant_cook::{PlantRecookOptions, PlantRecookOutcome, recook_plant_family};
use crate::{
    AssetServer, MaterialAsset, load_plant_family_asset, save_material_asset,
    save_plant_family_asset,
};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct Scratch {
    project: PathBuf,
}

impl Scratch {
    pub(super) fn new(tag: &str) -> Self {
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

    pub(super) fn assets(&self) -> AssetServer {
        AssetServer::new(self.project.join("assets"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

pub(super) fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).expect("fixture scalar")
}

pub(super) fn triangle() -> Mesh {
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

pub(super) fn alpha_card() -> Mesh {
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

pub(super) fn provenance() -> SourceProvenance {
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

pub(super) fn base_family(material: Uuid, source: PlantFamilySource) -> PlantFamilyAsset {
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
            response: saffron_vegetation::PhenotypeResponse::default(),
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

pub(super) fn imported_family(material: Uuid, mesh: Uuid) -> PlantFamilyAsset {
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

pub(super) fn native_family(material: Uuid) -> PlantFamilyAsset {
    base_family(
        material,
        PlantFamilySource::Native {
            graph: BotanicalGraphDocument::sapling(0x5a11),
            grafts: Vec::new(),
        },
    )
}

pub(super) fn fixture_server(tag: &str) -> (Scratch, AssetServer, Uuid, Uuid) {
    fixture_server_with_material(tag, &MaterialAsset::default())
}

pub(super) fn fixture_server_with_material(
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

pub(super) fn options() -> PlantRecookOptions {
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

pub(super) fn save_family(assets: &mut AssetServer, family: PlantFamilyAsset) -> PlantFamilyAsset {
    let id = save_plant_family_asset(assets, family, "Oak", "plants").expect("save family");
    load_plant_family_asset(assets, id).expect("reload family")
}

/// Replaces the sapling's leaf placement with a call to `call_guid`, and binds it to `module`.
pub(super) fn family_calling(material: Uuid, module: Uuid, call_guid: u128) -> PlantFamilyAsset {
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
    graph.nodes[leaves].operator = saffron_vegetation::BotanicalOperator::ModuleCall { call_guid };
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

/// Extends the imported fixture with a second geometry source (a second mesh asset
/// mapped to its own part), so the compiled family carries two prototypes and the
/// runtime load builds a real assembly table.
pub(super) fn two_prototype_family(
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

/// Encodes a cut-out RGBA PNG: a diagonal split from opaque to transparent.
///
/// A UNIFORM texture is useless for micromap derivation and would look like a broken
/// derivation rather than a degenerate fixture — every micro-triangle would be uniformly
/// covered, which correctly emits the format's special index and no block at all. Only a
/// plane with real coverage variation produces triangles that straddle the cutoff.
pub(super) fn cutout_png(edge: u32) -> Vec<u8> {
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
pub(super) fn coverage_png(alpha: u8) -> Vec<u8> {
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

/// Builds a family whose one material carries a real coverage texture, and cooks it.
pub(super) fn cook_family_with_coverage(tag: &str) -> (Scratch, AssetServer, Vec<u8>, ContentHash) {
    let scratch = Scratch::new(tag);
    let mut assets = scratch.assets();
    let texture = Uuid(7_502);
    std::fs::create_dir_all(assets.root.join("textures")).expect("create textures");
    std::fs::write(assets.root.join("textures/leaf.png"), cutout_png(64)).expect("write texture");
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
    let material =
        save_material_asset(&mut assets, &material_asset, "Leaf", "plants").expect("save material");
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
