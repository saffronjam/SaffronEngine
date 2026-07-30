//! Shared fixtures for the vegetation asset-I/O tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    AuthoredFieldTile, BIOME_ASSET_VERSION, BiomeAsset, BiomeGraphPolicy, BiomePaletteEntry,
    BiomeRole, BotanicalGraphDocument, FieldBlendOperator, FieldTileLayer, HabitatPreferences,
    InteractionPolicy, LocalBiomeInstance, MechanicalResponse, PLANT_ASSET_VERSION,
    PlantDimensions, PlantFamilyAsset, PlantFamilySource, PlantPart, PlantPartSemantic,
    VEGETATION_MAP_CHUNK_VERSION, VEGETATION_MAP_VERSION, VegetationLayer, VegetationLayerOperator,
    VegetationMapAsset, VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
    VegetationMapChunkLayout, VegetationMapChunkPayload, VegetationMapFieldChunk,
    VegetationMapTileKey, vegetation_map_chunk_schema_hash,
};
use serde_json::Value;

use crate::AssetServer;
use crate::vegetation::{
    VegetationMapTransaction, commit_vegetation_map_transaction, load_vegetation_map_root,
};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "saffron-vegetation-assets-{tag}-{}-{nanos}-{sequence}",
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

pub(crate) fn fixed(value: i32) -> DecisionScalar {
    DecisionScalar::from_integer(value).expect("representable fixture scalar")
}

pub(crate) fn plant_fixture(id: Uuid, name: &str) -> PlantFamilyAsset {
    PlantFamilyAsset {
        role: saffron_vegetation::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
        version: PLANT_ASSET_VERSION,
        id,
        name: name.to_owned(),
        tags: Vec::new(),
        source: PlantFamilySource::Native {
            graph: BotanicalGraphDocument::sapling(0x5a11),
            grafts: Vec::new(),
        },
        parts: vec![PlantPart {
            id: 12,
            parent: None,
            semantic: PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: Vec::new(),
        }],
        dimensions: PlantDimensions {
            height: fixed(8),
            trunk_radius: fixed(1),
            crown_radius: [fixed(3); 2],
            root_radius: [fixed(4); 2],
            local_bounds_min: [fixed(-4), fixed(0), fixed(-4)],
            local_bounds_max: [fixed(4), fixed(8), fixed(4)],
        },
        material_slots: vec![Uuid(13)],
        spines: Vec::new(),
        mechanics: MechanicalResponse {
            stiffness: fixed(2),
            damping: UnitInterval::from_bits(1),
            drag: fixed(1),
            flutter: DecisionScalar::from_bits(1),
            bend_limit: UnitInterval::from_bits(2),
            damage_threshold: fixed(3),
            break_threshold: fixed(4),
        },
        variations: vec![saffron_vegetation::PlantVariation {
            id: 0,
            name: "Default".to_owned(),
            sources: vec![saffron_vegetation::native_variation_source_id(0)],
            active_parts: Vec::new(),
        }],
        phenotypes: vec![saffron_vegetation::PlantPhenotype {
            id: 0,
            role: saffron_vegetation::PhenotypeRole::Healthy,
            season_window: None,
            variation: 0,
            material_remap: Vec::new(),
            active_parts: Vec::new(),
        }],
        collision_proxies: Vec::new(),
        navigation_proxies: Vec::new(),
        interaction_policy: InteractionPolicy::Structural,
        habitat: Some(HabitatPreferences {
            fields: vec![(FieldChannel::Moisture, fixed(0), fixed(1))],
            surface_tags: vec![14],
            shade_tolerance: UnitInterval::from_bits(32_768),
        }),
        ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
    }
}

pub(crate) fn biome_fixture(id: Uuid, name: &str, plant: Uuid) -> BiomeAsset {
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id,
        name: name.to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![BiomePaletteEntry {
            plant,
            weight: UnitInterval::ONE,
            seed_namespace: 23,
        }],
        density: fixed(1),
        clustering: UnitInterval::from_bits(24),
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces: vec![("canopy".to_owned(), 23)],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: fixed(64),
            require_authoritative_fields: true,
        },
        graph: Value::Object(Default::default()),
    }
}

pub(crate) fn map_fixture(id: Uuid, name: &str) -> VegetationMapAsset {
    let bounds = WorldBounds::new([0; 3], [1024; 3]).expect("fixture bounds");
    VegetationMapAsset {
        version: VEGETATION_MAP_VERSION,
        id,
        name: name.to_owned(),
        bounds,
        chunk_layout: VegetationMapChunkLayout {
            level: 0,
            schema_hash: vegetation_map_chunk_schema_hash(),
        },
        generation: 0,
        inventory: Vec::new(),
    }
}

pub(crate) fn layer_fixture(bounds: WorldBounds) -> VegetationLayer {
    VegetationLayer {
        id: 32,
        name: "Density".to_owned(),
        coordinate_space: saffron_vegetation::LayerCoordinateSpace::World,
        bounds,
        operator: VegetationLayerOperator::Density(FieldTileLayer {
            channel: FieldChannel::Moisture,
            tile_set: 33,
            blend: FieldBlendOperator::Multiply,
            weight: UnitInterval::ONE,
        }),
        dependencies: Vec::new(),
        order: 0,
        locked: false,
        muted: false,
        revision: 1,
    }
}

pub(crate) fn layer_chunk(map: Uuid, layer: VegetationLayer) -> VegetationMapChunk {
    VegetationMapChunk {
        version: VEGETATION_MAP_CHUNK_VERSION,
        map,
        key: VegetationMapChunkKey {
            layer: layer.id,
            tile: VegetationMapTileKey::Global,
            kind: VegetationMapChunkKind::LayerMetadata,
        },
        revision: layer.revision,
        payload: VegetationMapChunkPayload::LayerMetadata(layer),
    }
}

pub(crate) fn graph_instance_chunk(map: Uuid, instance: LocalBiomeInstance) -> VegetationMapChunk {
    VegetationMapChunk {
        version: VEGETATION_MAP_CHUNK_VERSION,
        map,
        key: VegetationMapChunkKey {
            layer: instance.id,
            tile: VegetationMapTileKey::Global,
            kind: VegetationMapChunkKind::GraphInstance,
        },
        revision: instance.revision,
        payload: VegetationMapChunkPayload::GraphInstance(instance),
    }
}

/// One bounded authored moisture tile on layer 32, owned by `cell`.
pub(crate) fn chunk_fixture(
    map: Uuid,
    cell: WorldCellKey,
    revision: u64,
    value: i32,
) -> VegetationMapChunk {
    VegetationMapChunk {
        version: VEGETATION_MAP_CHUNK_VERSION,
        map,
        key: VegetationMapChunkKey {
            layer: 32,
            tile: VegetationMapTileKey::Cell(cell),
            kind: VegetationMapChunkKind::Field,
        },
        revision,
        payload: VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
            fields: vec![AuthoredFieldTile {
                channel: FieldChannel::Moisture,
                layer: 32,
                dimensions: [1, 1, 1],
                quantum_bits: 1,
                values: vec![value],
            }],
            blockers: Vec::new(),
        }),
    }
}

pub(crate) fn commit_chunks(assets: &mut AssetServer, map: Uuid, upserts: Vec<VegetationMapChunk>) {
    let expected_generation = load_vegetation_map_root(assets, map).unwrap().generation;
    commit_vegetation_map_transaction(
        assets,
        map,
        VegetationMapTransaction {
            expected_generation,
            upserts,
            removals: Vec::new(),
        },
    )
    .unwrap();
}
