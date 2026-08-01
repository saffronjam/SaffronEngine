//! The authored biome, its placement graph, and the map chunks each recipe cooks from.

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, UnitInterval, WorldCellKey};
use saffron_vegetation::{
    BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION,
    BiomeAsset, BiomeGraphDocument, BiomeGraphPolicy, BiomePaletteEntry, BiomeRole, GraphAuthority,
    GraphDependencySource, GraphDomain, GraphEdge, GraphInterfaceOutput, GraphNodeDefinition,
    GraphOperator, GraphParameterValue, GraphSink, InclusionOperator, LayerCoordinateSpace,
    LocalBiomeInstance, NodeSpatialPolicy, VEGETATION_MAP_CHUNK_VERSION, VegetationLayer,
    VegetationLayerOperator, VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
    VegetationMapChunkPayload, VegetationMapTileKey, VolumeLayer,
};

use super::{Recipe, fixed};

pub(super) fn biome(recipe: &Recipe) -> BiomeAsset {
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
    // The understory scatters on its own node, so the cosmetic field's density is free to move
    // without re-keying a single macro plant.
    let mut understory = node(
        12,
        GraphOperator::StratifiedCoverage,
        GraphAuthority::Authoritative,
    );
    understory.seed_namespaces.insert("sampling".to_owned(), 11);
    understory.parameters.insert(
        "count".to_owned(),
        GraphParameterValue::U32(recipe.micro_coverage_count),
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
            understory,
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
            edge(1, "regions", 12, "regions"),
            edge(2, "candidates", 3, "candidates"),
            edge(3, "field", 4, "field"),
            edge(4, "field", 5, "field"),
            edge(2, "candidates", 6, "candidates"),
            edge(5, "field", 6, "weights"),
            edge(6, "candidates", 8, "candidates"),
            edge(7, "species", 8, "species"),
            edge(12, "candidates", 11, "candidates"),
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
pub(super) fn union_bounds(cells: &[(i64, i64, i64)]) -> saffron_spatial::WorldBounds {
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

pub(super) fn map_chunks(recipe: &Recipe) -> Vec<VegetationMapChunk> {
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
