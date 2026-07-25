//! Canonical authored vegetation package used by the real-host E2E acceptance test.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval, WorldCellKey};
use saffron_vegetation::{
    BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION,
    BiomeAsset, BiomeGraphDocument, BiomeGraphPolicy, BiomePaletteEntry, BiomeRole, GraphAuthority,
    GraphDependencySource, GraphDomain, GraphEdge, GraphInterfaceOutput, GraphNodeDefinition,
    GraphOperator, GraphParameterValue, GraphSink, ImportedPlantFamilyRecipe, InclusionOperator,
    InteractionPolicy, LayerCoordinateSpace, LocalBiomeInstance, MechanicalResponse,
    NodeSpatialPolicy, PLANT_ASSET_VERSION, PhenotypeRole, PlantCollisionProxy,
    PlantCollisionShape, PlantDimensions, PlantFamilyAsset, PlantFamilySource, PlantImportSettings,
    PlantManualSemanticTarget, PlantNavigationProxy, PlantPart, PlantPartSemantic, PlantPhenotype,
    PlantSemanticDestination, PlantSourceLocator, PlantSourceReference, PlantSourceRole,
    PlantSourceSelector, PlantTagId, PlantVariation, SourceProvenance,
    VEGETATION_MAP_CHUNK_VERSION, VEGETATION_MAP_VERSION, VegetationLayer, VegetationLayerOperator,
    VegetationMapAsset, VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
    VegetationMapChunkLayout, VegetationMapChunkPayload, VegetationMapChunkReference,
    VegetationMapTileKey, VolumeLayer, vegetation_content_hash, vegetation_map_chunk_schema_hash,
    write_biome_asset, write_plant_asset, write_vegetation_map_asset, write_vegetation_map_chunk,
};
use serde::Serialize;

const PLANT: Uuid = Uuid(7_300_001);
/// Project-relative trunk-geometry path the plant recipe references; the E2E writes the
/// fixture's OBJ bytes there before cooking.
const TRUNK_OBJ_PATH: &str = "models/e2e-birch.obj";
const BIOME: Uuid = Uuid(7_300_002);
const MAP: Uuid = Uuid(7_300_003);
const DEFAULT_MATERIAL: Uuid = Uuid(1);
const AUTHORED_LAYER: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
const BIOME_INSTANCE: u128 = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0001;

/// One authored-package recipe: the canonical phase-3 fixture plus the stress matrix
/// rows (density, mixed multi-cell woodland with negative cells, extreme scale, and a
/// rapid-traversal cell run). Content only — no vendor performance numbers.
struct Recipe {
    file: &'static str,
    stress: Option<&'static str>,
    plant_id: Uuid,
    biome_id: Uuid,
    map_id: Uuid,
    layer: u128,
    instance: u128,
    trunk_height: i32,
    coverage_count: u32,
    micro_dims: [u32; 3],
    cells: &'static [(i64, i64, i64)],
    seasonal: bool,
    expected_plant: &'static str,
    expected_accepted: &'static str,
}

const CANONICAL: Recipe = Recipe {
    file: "vegetation-phase3.json",
    stress: None,
    plant_id: PLANT,
    biome_id: BIOME,
    map_id: MAP,
    layer: AUTHORED_LAYER,
    instance: BIOME_INSTANCE,
    trunk_height: 8,
    coverage_count: 2,
    micro_dims: [8, 1, 8],
    cells: &[(0, 0, 0)],
    seasonal: false,
    expected_plant: "13a5f40885613ffa491fd721727fbb08",
    expected_accepted: "2",
};

const STRESS: &[Recipe] = &[
    Recipe {
        file: "vegetation-stress-meadow.json",
        stress: Some("meadow"),
        plant_id: Uuid(7_310_001),
        biome_id: Uuid(7_310_002),
        map_id: Uuid(7_310_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8891,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0011,
        trunk_height: 2,
        coverage_count: 4,
        micro_dims: [16, 1, 16],
        cells: &[(0, 0, 0)],
        seasonal: false,
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-woodland.json",
        stress: Some("woodland"),
        plant_id: Uuid(7_320_001),
        biome_id: Uuid(7_320_002),
        map_id: Uuid(7_320_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8892,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0012,
        trunk_height: 8,
        coverage_count: 6,
        micro_dims: [8, 1, 8],
        cells: &[(0, 0, 0), (-1, 0, 0), (0, 0, -1), (1, 0, 0)],
        seasonal: true,
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-scale.json",
        stress: Some("scale"),
        plant_id: Uuid(7_330_001),
        biome_id: Uuid(7_330_002),
        map_id: Uuid(7_330_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8893,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0013,
        trunk_height: 40,
        coverage_count: 2,
        micro_dims: [8, 1, 8],
        cells: &[(0, 0, 0)],
        seasonal: false,
        expected_plant: "",
        expected_accepted: "1",
    },
    Recipe {
        file: "vegetation-stress-traversal.json",
        stress: Some("traversal"),
        plant_id: Uuid(7_340_001),
        biome_id: Uuid(7_340_002),
        map_id: Uuid(7_340_003),
        layer: 0x1111_2222_3333_4444_5555_6666_7777_8894,
        instance: 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0014,
        trunk_height: 8,
        coverage_count: 3,
        micro_dims: [8, 1, 8],
        cells: &[(-1, 0, 0), (0, 0, 0), (1, 0, 0)],
        seasonal: false,
        expected_plant: "",
        expected_accepted: "1",
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    format_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stress: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cells: Vec<[i64; 3]>,
    plant_hex: String,
    biome_hex: String,
    map_hex: String,
    map_objects: Vec<MapObject>,
    trunk_obj_hex: String,
    trunk_obj_path: String,
    plant: String,
    biome: String,
    map: String,
    authored_layer: String,
    biome_instance: String,
    expected_plant: String,
    expected_accepted: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MapObject {
    content_hash: String,
    hex: String,
}

/// Writes the canonical fixture plus every stress-matrix fixture into `dir`.
pub fn write_all(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut written = Vec::new();
    for recipe in std::iter::once(&CANONICAL).chain(STRESS) {
        let path = dir.join(recipe.file);
        write_recipe(recipe, &path)?;
        written.push(path);
    }
    Ok(written)
}

fn write_recipe(recipe: &Recipe, path: &Path) -> Result<()> {
    let trunk = trunk_obj(recipe.trunk_height);
    let plant = write_plant_asset(&plant(recipe, &trunk))?;
    let biome = write_biome_asset(&biome(recipe))?;
    let chunks = map_chunks(recipe);
    let mut encoded_chunks = chunks
        .into_iter()
        .map(|chunk| {
            let bytes = write_vegetation_map_chunk(&chunk)?;
            let reference = VegetationMapChunkReference {
                key: chunk.key,
                content_hash: vegetation_content_hash(&bytes),
                byte_length: u64::try_from(bytes.len())?,
                revision: chunk.revision,
            };
            Ok((reference, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    encoded_chunks.sort_by_key(|(reference, _)| reference.order_key());
    let root = VegetationMapAsset {
        version: VEGETATION_MAP_VERSION,
        id: recipe.map_id,
        name: "E2E vegetation world".to_owned(),
        bounds: union_bounds(recipe.cells),
        chunk_layout: VegetationMapChunkLayout {
            level: 0,
            schema_hash: vegetation_map_chunk_schema_hash(),
        },
        generation: 1,
        inventory: encoded_chunks
            .iter()
            .map(|(reference, _)| *reference)
            .collect(),
    };
    let map = write_vegetation_map_asset(&root)?;
    let fixture = Fixture {
        format_version: 3,
        stress: recipe.stress.map(str::to_owned),
        cells: if recipe.stress.is_some() {
            recipe.cells.iter().map(|&(x, y, z)| [x, y, z]).collect()
        } else {
            Vec::new()
        },
        plant_hex: hex(&plant),
        biome_hex: hex(&biome),
        map_hex: hex(&map),
        trunk_obj_hex: hex(trunk.as_bytes()),
        trunk_obj_path: TRUNK_OBJ_PATH.to_owned(),
        map_objects: encoded_chunks
            .into_iter()
            .map(|(reference, bytes)| MapObject {
                content_hash: hex(&reference.content_hash),
                hex: hex(&bytes),
            })
            .collect(),
        plant: recipe.plant_id.value().to_string(),
        biome: recipe.biome_id.value().to_string(),
        map: recipe.map_id.value().to_string(),
        authored_layer: format!("{:032x}", recipe.layer),
        biome_instance: format!("{:032x}", recipe.instance),
        expected_plant: recipe.expected_plant.to_owned(),
        expected_accepted: recipe.expected_accepted.to_owned(),
    };
    let mut bytes = serde_json::to_vec_pretty(&fixture)?;
    bytes.push(b'\n');
    let mut file = AtomicWriteFile::options()
        .open(path)
        .with_context(|| format!("open vegetation E2E fixture {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write vegetation E2E fixture {}", path.display()))?;
    file.commit()
        .with_context(|| format!("publish vegetation E2E fixture {}", path.display()))
}

/// A watertight 1×8×1 m box trunk with per-face normals and UVs — the smallest
/// renderable geometry the plant compiler accepts.
fn trunk_obj(height: i32) -> String {
    let mut obj = String::from("o e2e-birch-trunk\n");
    let (x, y, z) = (0.5_f32, height as f32, 0.5_f32);
    let corners = [
        [-x, 0.0, -z],
        [x, 0.0, -z],
        [x, y, -z],
        [-x, y, -z],
        [-x, 0.0, z],
        [x, 0.0, z],
        [x, y, z],
        [-x, y, z],
    ];
    for corner in corners {
        let _ = writeln!(obj, "v {} {} {}", corner[0], corner[1], corner[2]);
    }
    for uv in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
        let _ = writeln!(obj, "vt {} {}", uv[0], uv[1]);
    }
    for normal in [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
    ] {
        let _ = writeln!(obj, "vn {} {} {}", normal[0], normal[1], normal[2]);
    }
    // Counter-clockwise quads seen from outside, split into triangles.
    let faces: [([usize; 4], usize); 6] = [
        ([1, 4, 3, 2], 1),
        ([5, 6, 7, 8], 2),
        ([1, 5, 8, 4], 3),
        ([2, 3, 7, 6], 4),
        ([1, 2, 6, 5], 5),
        ([4, 8, 7, 3], 6),
    ];
    for (quad, normal) in faces {
        let _ = writeln!(
            obj,
            "f {}/1/{normal} {}/2/{normal} {}/3/{normal}",
            quad[0], quad[1], quad[2]
        );
        let _ = writeln!(
            obj,
            "f {}/1/{normal} {}/3/{normal} {}/4/{normal}",
            quad[0], quad[2], quad[3]
        );
    }
    obj
}

/// The OBJ importer's default material element for the trunk file — the id derives
/// from the model key (the file stem) the same way the model importer bakes sub-assets.
fn trunk_material_selector() -> PlantSourceSelector {
    PlantSourceSelector::Element {
        id: u128::from(
            saffron_geometry::sub_id_for("e2e-birch", "material", "material_0", 0).value(),
        ),
        path: "materials/material_0".to_owned(),
    }
}

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

fn plant(recipe: &Recipe, trunk_obj: &str) -> PlantFamilyAsset {
    let height = recipe.trunk_height;
    let mut variations = vec![PlantVariation {
        id: 0,
        name: "Default".to_owned(),
        sources: vec![10, 11],
        active_parts: Vec::new(),
    }];
    let mut phenotypes = vec![PlantPhenotype {
        id: 0,
        role: PhenotypeRole::Healthy,
        season_window: None,
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
        phenotypes.push(PlantPhenotype {
            id: 1,
            role: PhenotypeRole::Senescent,
            season_window: None,
            variation: 1,
            material_remap: Vec::new(),
            active_parts: Vec::new(),
        });
    }
    PlantFamilyAsset {
        version: PLANT_ASSET_VERSION,
        id: recipe.plant_id,
        name: "E2E silver birch".to_owned(),
        tags: vec![PlantTagId::new(1).expect("nonzero fixture tag")],
        source: PlantFamilySource::Imported(ImportedPlantFamilyRecipe {
            sources: vec![
                PlantSourceReference {
                    id: 10,
                    locator: PlantSourceLocator::File(TRUNK_OBJ_PATH.to_owned()),
                    role: PlantSourceRole::Geometry,
                    selector: PlantSourceSelector::Whole,
                    content_hash: vegetation_content_hash(trunk_obj.as_bytes()),
                    settings: PlantImportSettings::default(),
                    provenance: provenance(),
                },
                PlantSourceReference {
                    id: 11,
                    locator: PlantSourceLocator::File(TRUNK_OBJ_PATH.to_owned()),
                    role: PlantSourceRole::Material,
                    selector: trunk_material_selector(),
                    content_hash: vegetation_content_hash(trunk_obj.as_bytes()),
                    settings: PlantImportSettings::default(),
                    provenance: provenance(),
                },
            ],
            semantic_targets: vec![
                PlantManualSemanticTarget {
                    id: 30,
                    source: 10,
                    selector: PlantSourceSelector::Whole,
                    destination: PlantSemanticDestination::Part(1),
                },
                PlantManualSemanticTarget {
                    id: 31,
                    source: 11,
                    selector: trunk_material_selector(),
                    destination: PlantSemanticDestination::MaterialSlot(0),
                },
            ],
        }),
        parts: vec![PlantPart {
            id: 1,
            parent: None,
            semantic: PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: vec![10],
        }],
        dimensions: PlantDimensions {
            height: fixed(height),
            trunk_radius: fixed(1),
            crown_radius: [fixed(2); 2],
            root_radius: [fixed(2); 2],
            local_bounds_min: [fixed(-2), fixed(0), fixed(-2)],
            local_bounds_max: [fixed(2), fixed(height), fixed(2)],
        },
        material_slots: vec![DEFAULT_MATERIAL],
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

fn biome(recipe: &Recipe) -> BiomeAsset {
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: recipe.biome_id,
        name: "E2E resident vegetation".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![BiomePaletteEntry {
            plant: recipe.plant_id,
            weight: UnitInterval::ONE,
            seed_namespace: 13,
        }],
        density: fixed(1),
        clustering: UnitInterval::ZERO,
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces: vec![
            ("sampling".to_owned(), 11),
            ("species-selection".to_owned(), 13),
            ("noise".to_owned(), 17),
            ("reconstruction".to_owned(), 23),
            ("community".to_owned(), 29),
        ],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: fixed(64),
            require_authoritative_fields: true,
        },
        graph: graph(recipe).to_json(),
    }
}

fn graph(recipe: &Recipe) -> BiomeGraphDocument {
    let mut coverage = node(
        2,
        GraphOperator::StratifiedCoverage,
        GraphAuthority::Authoritative,
    );
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage.parameters.insert(
        "count".to_owned(),
        GraphParameterValue::U32(recipe.coverage_count),
    );
    let mut noise = node(3, GraphOperator::Noise, GraphAuthority::EquivalentGpu);
    noise.seed_namespaces.insert("noise".to_owned(), 17);
    noise
        .parameters
        .insert("amplitude".to_owned(), GraphParameterValue::Fixed(fixed(1)));
    noise
        .parameters
        .insert("channel".to_owned(), GraphParameterValue::U32(3));
    noise.parameters.insert(
        "frequency".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
    );
    let mut curve = node(4, GraphOperator::Curve, GraphAuthority::EquivalentGpu);
    curve.parameters.insert(
        "curve".to_owned(),
        GraphParameterValue::Curve(vec![
            (UnitInterval::ZERO, DecisionScalar::from_bits(0)),
            (UnitInterval::ONE, fixed(1)),
        ]),
    );
    let mut clamp = node(5, GraphOperator::Clamp, GraphAuthority::EquivalentGpu);
    clamp
        .parameters
        .insert("maximum".to_owned(), GraphParameterValue::Fixed(fixed(1)));
    clamp.parameters.insert(
        "minimum".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    let mut importance = node(
        6,
        GraphOperator::FieldImportance,
        GraphAuthority::EquivalentGpu,
    );
    importance.parameters.insert(
        "threshold".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    let mut output = node(8, GraphOperator::MacroOutput, GraphAuthority::Authoritative);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let mut community_blend = node(
        11,
        GraphOperator::CommunityBlend,
        GraphAuthority::Authoritative,
    );
    community_blend
        .seed_namespaces
        .insert("community".to_owned(), 29);
    let mut micro = node(9, GraphOperator::MicroOutput, GraphAuthority::Authoritative);
    micro
        .seed_namespaces
        .insert("reconstruction".to_owned(), 23);
    micro.parameters.insert(
        "dimensions".to_owned(),
        GraphParameterValue::U32Vec3(recipe.micro_dims),
    );
    micro.parameters.insert(
        "attributeChannels".to_owned(),
        GraphParameterValue::GuidList(Vec::new()),
    );
    let mut region = node(1, GraphOperator::RegionInput, GraphAuthority::Authoritative);
    region
        .dependencies
        .push(GraphDependencySource::MapLayer(recipe.layer));
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![
            GraphInterfaceOutput {
                id: 100,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 8,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            },
            GraphInterfaceOutput {
                id: 101,
                name: "micro".to_owned(),
                domain: GraphDomain::MicroField,
                node: 9,
                pin: "micro".to_owned(),
                sink: Some(GraphSink::Micro),
            },
        ],
        nodes: vec![
            region,
            coverage,
            noise,
            curve,
            clamp,
            importance,
            node(
                7,
                GraphOperator::SpeciesInput,
                GraphAuthority::Authoritative,
            ),
            output,
            node(
                10,
                GraphOperator::CommunityInput,
                GraphAuthority::Authoritative,
            ),
            community_blend,
            micro,
        ],
        edges: vec![
            edge(1, "regions", 2, "regions"),
            edge(2, "candidates", 3, "candidates"),
            edge(3, "field", 4, "field"),
            edge(4, "field", 5, "field"),
            edge(2, "candidates", 6, "candidates"),
            edge(5, "field", 6, "weights"),
            edge(6, "candidates", 8, "candidates"),
            edge(7, "species", 8, "species"),
            edge(6, "candidates", 11, "candidates"),
            edge(10, "communities", 11, "communities"),
            edge(11, "candidates", 9, "candidates"),
        ],
    }
}

fn node(guid: u128, operator: GraphOperator, authority: GraphAuthority) -> GraphNodeDefinition {
    GraphNodeDefinition {
        guid,
        version: BIOME_NODE_VERSION,
        semantic_revision: 1,
        operator,
        authority,
        spatial: NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(0),
        },
        dependencies: Vec::new(),
        seed_namespaces: BTreeMap::new(),
        parameters: BTreeMap::new(),
    }
}

fn edge(from_node: u128, from_pin: &str, to_node: u128, to_pin: &str) -> GraphEdge {
    GraphEdge {
        from_node,
        from_pin: from_pin.to_owned(),
        to_node,
        to_pin: to_pin.to_owned(),
    }
}

/// The exact half-open tick union of the recipe's base cells.
fn union_bounds(cells: &[(i64, i64, i64)]) -> saffron_spatial::WorldBounds {
    let mut minimum = [i128::MAX; 3];
    let mut maximum = [i128::MIN; 3];
    for &(x, y, z) in cells {
        let bounds = WorldCellKey::base(x, y, z).bounds();
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(bounds.min_ticks()[axis]);
            maximum[axis] = maximum[axis].max(bounds.max_ticks_exclusive()[axis]);
        }
    }
    saffron_spatial::WorldBounds::new(minimum, maximum).expect("recipe cell union")
}

fn map_chunks(recipe: &Recipe) -> Vec<VegetationMapChunk> {
    let bounds = union_bounds(recipe.cells);
    let layer = VegetationLayer {
        id: recipe.layer,
        name: "Authored grove".to_owned(),
        coordinate_space: LayerCoordinateSpace::World,
        bounds,
        operator: VegetationLayerOperator::Volume(VolumeLayer {
            bounds,
            operation: InclusionOperator::Include,
            falloff: DecisionScalar::from_bits(0),
        }),
        dependencies: Vec::new(),
        order: 0,
        locked: false,
        muted: false,
        revision: 1,
    };
    let instance = LocalBiomeInstance {
        id: recipe.instance,
        biome: recipe.biome_id,
        bounds,
        bindings: Vec::new(),
        revision: 1,
    };
    vec![
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: recipe.map_id,
            key: VegetationMapChunkKey {
                layer: layer.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::LayerMetadata,
            },
            revision: layer.revision,
            payload: VegetationMapChunkPayload::LayerMetadata(layer),
        },
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: recipe.map_id,
            key: VegetationMapChunkKey {
                layer: instance.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::GraphInstance,
            },
            revision: instance.revision,
            payload: VegetationMapChunkPayload::GraphInstance(instance),
        },
    ]
}

fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).expect("representable fixture scalar")
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}
