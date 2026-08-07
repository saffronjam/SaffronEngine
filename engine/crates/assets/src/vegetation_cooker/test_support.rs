//! Shared fixtures for the vegetation-cooker tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, Vertex, compute_tangents, save_mesh_to_buffer};
use saffron_scene::{AssetEntry, AssetType};
use saffron_spatial::{DecisionScalar, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION, BiomeGraphDocument,
    ContentHash, GraphAuthority, GraphCancellationToken, GraphDependencySource, GraphDomain,
    GraphEdge, GraphInterfaceOutput, GraphNodeDefinition, GraphOperator, GraphParameterValue,
    GraphSink, LocalBiomeInstance, NodeSpatialPolicy, PlantFamilySource, PlantImportSettings,
    PlantSourceLocator, PlantSourceReference, PlantSourceRole, PlantSourceSelector,
    SourceProvenance,
};

use super::{
    VegetationCookEvent, VegetationCookOutput, VegetationCookRequest, clone_surface_providers,
    commit_staged_vegetation_cook, portable_vegetation_platform_profile, stage_vegetation_cook,
};
use crate::vegetation::test_support::{
    biome_fixture, chunk_fixture, commit_chunks, fixed, graph_instance_chunk, layer_chunk,
    layer_fixture, map_fixture, plant_fixture,
};
use crate::vegetation::{save_biome_asset, save_plant_family_asset, save_vegetation_map_asset};
use crate::{AssetServer, CookProjectView, Error, MaterialAsset, Result, save_material_asset};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The layer the authored field chunks and the placement graph both name.
pub(crate) const AUTHORED_LAYER: u128 = 32;

/// The local biome instance the populated map carries.
const BIOME_INSTANCE: u128 = 91;

pub(crate) struct Scratch(PathBuf);

pub(crate) fn run_cook(
    assets: &mut AssetServer,
    request: VegetationCookRequest,
    cancellation: &GraphCancellationToken,
    emit: impl FnMut(VegetationCookEvent),
) -> Result<VegetationCookOutput> {
    let surfaces = clone_surface_providers(&request.surface_providers);
    let staged = stage_vegetation_cook(
        CookProjectView::capture(assets),
        request,
        cancellation,
        emit,
    )?;
    commit_staged_vegetation_cook(assets, &surfaces, staged, cancellation)
}

impl Scratch {
    pub(crate) fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "saffron-vegetation-cooker-{tag}-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A populated authored map: one plant family, one biome whose graph places macro points and reads
/// [`AUTHORED_LAYER`], one local instance covering every cell, and one bounded field chunk per cell.
pub(crate) struct PopulatedMap {
    pub(crate) map: Uuid,
    pub(crate) plant: Uuid,
    pub(crate) biome: Uuid,
}

/// Saves the populated map spanning `cells`.
///
/// `halo_metres` is the placement node's influence radius, which becomes the compiled graph's
/// declared halo at level zero: every cell's read bounds — and so its dependency region — grow by
/// it, which is what decides whether a neighbour's authored chunk reaches this cell's cook key.
pub(crate) fn save_populated_map(
    assets: &mut AssetServer,
    cells: &[WorldCellKey],
    halo_metres: i32,
) -> Result<PopulatedMap> {
    let bounds = cells
        .iter()
        .map(|cell| cell.bounds())
        .reduce(WorldBounds::union)
        .expect("map fixture cells");
    // The family compiles only against a catalogued material, and a family that fails to compile
    // publishes no artifact and cooks no plants.
    let material = save_material_asset(assets, &MaterialAsset::default(), "Oak material", "")?;
    let mut plant_asset = plant_fixture(Uuid(1), "Oak");
    plant_asset.material_slots = vec![material];
    let plant = save_plant_family_asset(assets, plant_asset, "Oak", "")?;
    let mut biome_asset = biome_fixture(Uuid(2), "Forest", plant);
    biome_asset.graph = placement_graph(halo_metres).to_json();
    let biome = save_biome_asset(assets, biome_asset, "Forest", "")?;
    let mut map_asset = map_fixture(Uuid(3), "World");
    map_asset.bounds = bounds;
    let map = save_vegetation_map_asset(assets, map_asset, "World", "")?;
    let mut layer = layer_fixture(bounds);
    layer.bounds = bounds;
    let mut chunks = vec![
        layer_chunk(map, layer),
        graph_instance_chunk(
            map,
            LocalBiomeInstance {
                id: BIOME_INSTANCE,
                biome,
                bounds,
                bindings: Vec::new(),
                revision: 1,
            },
        ),
    ];
    chunks.extend(cells.iter().map(|cell| chunk_fixture(map, *cell, 1, 0)));
    commit_chunks(assets, map, chunks);
    Ok(PopulatedMap { map, plant, biome })
}

/// Gives `family` one graft over an external mesh file whose declared observation is stale — the
/// shape every freshly imported family has until a cook reads the file and accepts what it saw.
pub(crate) fn declare_unobserved_graft(assets: &mut AssetServer, family: Uuid) -> Result<()> {
    let relative = "meshes/hero.smesh";
    let path = assets.root.join(relative);
    std::fs::create_dir_all(path.parent().expect("source parent"))
        .map_err(|error| Error::Io(error.to_string()))?;
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
    std::fs::write(&path, save_mesh_to_buffer(&mesh, &[], None)?)
        .map_err(|error| Error::Io(error.to_string()))?;
    let hero = Uuid(9_001);
    assets.catalog.put(AssetEntry {
        id: hero,
        name: "hero".to_owned(),
        asset_type: AssetType::Mesh,
        path: relative.to_owned(),
        ..AssetEntry::default()
    });
    let mut asset = crate::load_plant_family_asset(assets, family)?;
    let PlantFamilySource::Native { grafts, .. } = &mut asset.source else {
        panic!("the fixture family is native");
    };
    grafts.push(PlantSourceReference {
        id: 77,
        locator: PlantSourceLocator::Asset(hero),
        role: PlantSourceRole::Geometry,
        selector: PlantSourceSelector::Element {
            id: u128::from(hero.value()),
            path: "hero".to_owned(),
        },
        content_hash: [0xAB; 32],
        settings: PlantImportSettings::default(),
        provenance: SourceProvenance::default(),
    });
    crate::update_plant_family_asset(assets, family, &asset)
}

/// Overwrites the bounded field chunk `cell` owns with a new value, which re-keys that one chunk.
pub(crate) fn edit_field_chunk(
    assets: &mut AssetServer,
    map: Uuid,
    cell: WorldCellKey,
    revision: u64,
    value: i32,
) {
    commit_chunks(assets, map, vec![chunk_fixture(map, cell, revision, value)]);
}

/// Every published cell's artifact identity, which is the hash of its bytes.
pub(crate) fn cell_artifact_hashes(
    output: &VegetationCookOutput,
) -> BTreeMap<WorldCellKey, ContentHash> {
    output
        .manifest
        .cells
        .iter()
        .map(|cell| (cell.cell, cell.artifact_hash))
        .collect()
}

pub(crate) fn cook_request(
    world: Uuid,
    map: Uuid,
    expected_manifest: Option<ContentHash>,
    cells: Vec<WorldCellKey>,
    workers: u16,
) -> VegetationCookRequest {
    VegetationCookRequest {
        world,
        map,
        expected_manifest,
        cells,
        ecology_tick: 0,
        workers,
        platform: portable_vegetation_platform_profile(Some("test-portable")),
        surface_providers: Vec::new(),
    }
}

fn placement_graph(halo_metres: i32) -> BiomeGraphDocument {
    let node = |guid, operator| GraphNodeDefinition {
        guid,
        version: BIOME_NODE_VERSION,
        semantic_revision: 1,
        operator,
        authority: GraphAuthority::Authoritative,
        spatial: NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(0),
        },
        dependencies: Vec::new(),
        seed_namespaces: BTreeMap::new(),
        parameters: BTreeMap::new(),
    };
    let mut region = node(1, GraphOperator::RegionInput);
    region
        .dependencies
        .push(GraphDependencySource::MapLayer(AUTHORED_LAYER));
    let mut coverage = node(2, GraphOperator::StratifiedCoverage);
    coverage.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: fixed(halo_metres),
    };
    coverage.seed_namespaces.insert("sampling".to_owned(), 23);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    let species = node(3, GraphOperator::SpeciesInput);
    let mut output = node(4, GraphOperator::MacroOutput);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 23);
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 400,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, coverage, region],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "species".to_owned(),
                to_node: 4,
                to_pin: "species".to_owned(),
            },
        ],
    }
}
