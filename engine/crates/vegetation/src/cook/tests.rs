use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, WorldCellKey};

use crate::binary::BinaryReader;
use crate::{Error, VegetationMapChunkKey, VegetationMapChunkKind, VegetationMapTileKey};

use super::*;

fn dependency(address: CookDependencyAddress, hash: u8) -> CookDependency {
    CookDependency {
        address,
        content_hash: ContentHash::new([hash; 32]),
        bounds: None,
        halo: DecisionScalar::from_bits(0),
        ancestor_level: None,
    }
}

fn graph(nodes: Vec<CookNodeRecord>) -> CookGraph {
    let mut graph = CookGraph {
        versions: CookVersionSet {
            schema: 1,
            compiler: 2,
            evaluator: 3,
            numeric: 4,
            simulation: 5,
        },
        platform: CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["canonical-fixed".to_owned(), "thin-sheet".to_owned()],
        },
        nodes,
    };
    for node in &mut graph.nodes {
        node.cook_key = node
            .calculate_cook_key(graph.versions, &graph.platform)
            .unwrap();
    }
    graph
}

fn node(address: CookNodeAddress, dependencies: Vec<CookDependency>) -> CookNodeRecord {
    CookNodeRecord {
        address,
        cook_key: ContentHash::default(),
        output_hash: ContentHash::new([8; 32]),
        dependencies,
        estimate: CookWorkEstimate::default(),
        actual: CookWorkActual::default(),
    }
}

#[test]
fn graph_bytes_ignore_input_and_dependency_order() {
    let first = node(
        CookNodeAddress::Cell {
            map: Uuid(10),
            cell: WorldCellKey::base(-1, 2, 0),
        },
        vec![
            dependency(CookDependencyAddress::SourceAsset { asset: Uuid(20) }, 1),
            dependency(CookDependencyAddress::MapManifest { map: Uuid(10) }, 2),
        ],
    );
    let mut reversed = first.clone();
    reversed.dependencies.reverse();
    let a = graph(vec![first]);
    let b = graph(vec![reversed]);
    assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
    assert_eq!(
        CookGraph::from_canonical_bytes(&a.canonical_bytes().unwrap()).unwrap(),
        a
    );
}

#[test]
fn measured_work_does_not_change_graph_identity() {
    let mut first = node(
        CookNodeAddress::Plant { family: Uuid(20) },
        vec![dependency(
            CookDependencyAddress::SourceAsset { asset: Uuid(20) },
            1,
        )],
    );
    let mut second = first.clone();
    first.actual.elapsed_micros = 10;
    second.actual.elapsed_micros = 99;
    second.actual.cache_hit = true;
    assert_eq!(
        graph(vec![first]).canonical_bytes().unwrap(),
        graph(vec![second]).canonical_bytes().unwrap()
    );
}

#[test]
fn map_object_and_source_file_addresses_round_trip_exactly() {
    let cell = WorldCellKey::base(-7, 0, 11);
    let addresses = [
        CookDependencyAddress::SourceFile {
            uri: "project://plants/oak.glb".to_owned(),
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 21,
                tile: VegetationMapTileKey::Cell(cell),
                kind: VegetationMapChunkKind::Field,
            },
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 22,
                tile: VegetationMapTileKey::Cell(cell),
                kind: VegetationMapChunkKind::AnchorOverride,
            },
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 23,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::GraphInstance,
            },
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 24,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::LayerMetadata,
            },
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 25,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::EditorMetadata,
            },
        },
    ];

    for address in addresses {
        let bytes = address.canonical_bytes().unwrap();
        let mut reader = BinaryReader::new(&bytes, "vegetation cook");
        let decoded = CookDependencyAddress::decode(&mut reader).unwrap();
        reader.complete().unwrap();
        assert_eq!(decoded, address);
    }
}

#[test]
fn map_object_address_rejects_kind_tile_mismatches() {
    let invalid = [
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 21,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::Field,
            },
        },
        CookDependencyAddress::MapObject {
            map: Uuid(10),
            key: VegetationMapChunkKey {
                layer: 21,
                tile: VegetationMapTileKey::Cell(WorldCellKey::base(0, 0, 0)),
                kind: VegetationMapChunkKind::LayerMetadata,
            },
        },
    ];

    for address in invalid {
        assert!(matches!(
            address.canonical_bytes(),
            Err(Error::ArtifactFormat { field, .. }) if field == "dependencyAddress"
        ));
    }
}

#[test]
fn graph_rejects_a_node_dependency_with_the_wrong_output_hash() {
    let plant = CookNodeAddress::Plant { family: Uuid(20) };
    let graph = graph(vec![
        node(plant.clone(), Vec::new()),
        node(
            CookNodeAddress::Cell {
                map: Uuid(10),
                cell: WorldCellKey::base(0, 0, 0),
            },
            vec![dependency(CookDependencyAddress::Node(plant), 7)],
        ),
    ]);
    assert!(matches!(
        graph.canonical_bytes(),
        Err(Error::ArtifactFormat { field, .. }) if field == "dependencies.nodeContentHash"
    ));
}

#[test]
fn content_hash_requires_canonical_lowercase_hex() {
    let hash = ContentHash::new([0xab; 32]);
    assert_eq!(hash.to_string().parse::<ContentHash>().unwrap(), hash);
    assert!(
        hash.to_string()
            .to_uppercase()
            .parse::<ContentHash>()
            .is_err()
    );
}

#[test]
fn graph_rejects_a_cook_key_not_derived_from_exact_inputs() {
    let mut graph = graph(vec![node(
        CookNodeAddress::Plant { family: Uuid(20) },
        vec![dependency(
            CookDependencyAddress::SourceAsset { asset: Uuid(20) },
            1,
        )],
    )]);
    graph.nodes[0].cook_key = ContentHash::new([99; 32]);
    assert!(matches!(
        graph.canonical_bytes(),
        Err(Error::ArtifactFormat { field, .. }) if field == "nodes.cookKey"
    ));
}
