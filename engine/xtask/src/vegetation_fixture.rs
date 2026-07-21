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
    GraphOperator, GraphParameterValue, GraphSink, InclusionOperator, InteractionPolicy,
    LayerCoordinateSpace, LocalBiomeInstance, MechanicalResponse, NativeBotanicalGraph,
    NodeSpatialPolicy, PLANT_ASSET_VERSION, PhenotypeRole, PlantDimensions, PlantFamilyAsset,
    PlantFamilySource, PlantPart, PlantPartSemantic, PlantPhenotype, PlantTagId, PlantVariation,
    VEGETATION_MAP_CHUNK_VERSION, VEGETATION_MAP_VERSION, VegetationLayer, VegetationLayerOperator,
    VegetationMapAsset, VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
    VegetationMapChunkLayout, VegetationMapChunkPayload, VegetationMapChunkReference,
    VegetationMapTileKey, VolumeLayer, vegetation_content_hash, vegetation_map_chunk_schema_hash,
    write_biome_asset, write_plant_asset, write_vegetation_map_asset, write_vegetation_map_chunk,
};
use serde::Serialize;

const PLANT: Uuid = Uuid(7_300_001);
const BIOME: Uuid = Uuid(7_300_002);
const MAP: Uuid = Uuid(7_300_003);
const DEFAULT_MATERIAL: Uuid = Uuid(1);
const AUTHORED_LAYER: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
const BIOME_INSTANCE: u128 = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0001;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    format_version: u32,
    plant_hex: String,
    biome_hex: String,
    map_hex: String,
    map_objects: Vec<MapObject>,
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

pub fn write(path: &Path) -> Result<()> {
    let plant = write_plant_asset(&plant())?;
    let biome = write_biome_asset(&biome())?;
    let chunks = map_chunks();
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
        id: MAP,
        name: "E2E vegetation world".to_owned(),
        bounds: WorldCellKey::base(0, 0, 0).bounds(),
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
        format_version: 2,
        plant_hex: hex(&plant),
        biome_hex: hex(&biome),
        map_hex: hex(&map),
        map_objects: encoded_chunks
            .into_iter()
            .map(|(reference, bytes)| MapObject {
                content_hash: hex(&reference.content_hash),
                hex: hex(&bytes),
            })
            .collect(),
        plant: PLANT.value().to_string(),
        biome: BIOME.value().to_string(),
        map: MAP.value().to_string(),
        authored_layer: format!("{AUTHORED_LAYER:032x}"),
        biome_instance: format!("{BIOME_INSTANCE:032x}"),
        expected_plant: "13a5f40885613ffa491fd721727fbb08".to_owned(),
        expected_accepted: "2".to_owned(),
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

fn plant() -> PlantFamilyAsset {
    PlantFamilyAsset {
        version: PLANT_ASSET_VERSION,
        id: PLANT,
        name: "E2E silver birch".to_owned(),
        tags: vec![PlantTagId::new(1).expect("nonzero fixture tag")],
        source: PlantFamilySource::Native(NativeBotanicalGraph {
            schema_hash: [1; 32],
            graph: serde_json::json!({ "nodes": [], "version": 1 }),
        }),
        parts: vec![PlantPart {
            id: 1,
            parent: None,
            semantic: PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: Vec::new(),
        }],
        dimensions: PlantDimensions {
            height: fixed(8),
            trunk_radius: fixed(1),
            crown_radius: [fixed(2); 2],
            root_radius: [fixed(2); 2],
            local_bounds_min: [fixed(-2), fixed(0), fixed(-2)],
            local_bounds_max: [fixed(2), fixed(8), fixed(2)],
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
        variations: vec![PlantVariation {
            id: 0,
            name: "Default".to_owned(),
            sources: Vec::new(),
            active_parts: Vec::new(),
        }],
        phenotypes: vec![PlantPhenotype {
            id: 0,
            role: PhenotypeRole::Healthy,
            variation: 0,
            material_remap: Vec::new(),
            active_parts: Vec::new(),
        }],
        collision_proxies: Vec::new(),
        navigation_proxies: Vec::new(),
        interaction_policy: InteractionPolicy::Structural,
        habitat: None,
    }
}

fn biome() -> BiomeAsset {
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: BIOME,
        name: "E2E resident vegetation".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![BiomePaletteEntry {
            plant: PLANT,
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
        ],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: fixed(64),
            require_authoritative_fields: true,
        },
        graph: graph().to_json(),
    }
}

fn graph() -> BiomeGraphDocument {
    let mut coverage = node(
        2,
        GraphOperator::StratifiedCoverage,
        GraphAuthority::Authoritative,
    );
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(2));
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
    let mut region = node(1, GraphOperator::RegionInput, GraphAuthority::Authoritative);
    region
        .dependencies
        .push(GraphDependencySource::MapLayer(AUTHORED_LAYER));
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 100,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 8,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
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

fn map_chunks() -> Vec<VegetationMapChunk> {
    let cell = WorldCellKey::base(0, 0, 0);
    let layer = VegetationLayer {
        id: AUTHORED_LAYER,
        name: "Authored grove".to_owned(),
        coordinate_space: LayerCoordinateSpace::World,
        bounds: cell.bounds(),
        operator: VegetationLayerOperator::Volume(VolumeLayer {
            bounds: cell.bounds(),
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
        id: BIOME_INSTANCE,
        biome: BIOME,
        bounds: cell.bounds(),
        bindings: Vec::new(),
        revision: 1,
    };
    vec![
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map: MAP,
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
            map: MAP,
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
