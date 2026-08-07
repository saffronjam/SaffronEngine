use crate::{
    FieldChannelDto, Uuid, VegetationArtifactSectionCodecDto, VegetationCellSectionKindDto,
    VegetationCookNodeAddressDto, VegetationCookPlatformProfileDto, VegetationCookVersionSetDto,
    VegetationCookWorkActualDto, VegetationCookWorkEstimateDto, VegetationGuid, WorldBoundsDto,
    WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Typed payload vocabulary for one immutable authored vegetation-map object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationMapChunkKindDto {
    Field,
    AnchorOverride,
    GraphInstance,
    LayerMetadata,
    EditorMetadata,
}

/// Global or spatial tile address for one immutable authored vegetation-map object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationMapTileKeyDto {
    Global,
    Cell { cell: WorldCellDto },
}

/// Stable logical key resolved through a vegetation map's immutable object inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkKeyDto {
    pub layer: VegetationGuid,
    pub tile: VegetationMapTileKeyDto,
    pub kind: VegetationMapChunkKindDto,
}

/// Stable address of one exact input read by the vegetation cook graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationManifestDependencyAddressDto {
    SourceAsset {
        asset: Uuid,
    },
    SourceFile {
        uri: String,
    },
    MaterialCoverage {
        material: Uuid,
    },
    BiomeIr {
        map: Uuid,
        instance: VegetationGuid,
    },
    MapManifest {
        map: Uuid,
    },
    MapObject {
        map: Uuid,
        key: VegetationMapChunkKeyDto,
    },
    SurfaceProvider {
        provider: String,
        revision: String,
    },
    SurfaceTile {
        provider: String,
        revision: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<FieldChannelDto>,
        bounds: WorldBoundsDto,
    },
    Contract {
        namespace: String,
    },
    Node {
        node: VegetationCookNodeAddressDto,
    },
}

/// One exact immutable dependency in a vegetation base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestDependencyDto {
    pub address: VegetationManifestDependencyAddressDto,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<WorldBoundsDto>,
    pub halo_bits: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestor_level: Option<u8>,
}

/// One authoritative seed namespace bound into the world manifest identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSeedNamespaceDto {
    pub name: String,
    pub namespace: VegetationGuid,
}

/// Packed element shape of one canonical vegetation point column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationPointColumnTypeDto {
    Id128,
    WorldCell,
    Orientation,
    FixedVec3,
    WorldBounds,
    AssetUuid,
    U32,
    U64,
    OptionalId128,
    Unit,
    SurfaceProjection,
    OptionalSurfaceAttachment,
    WorldPosition,
}

/// One exact point column pinned into the immutable manifest identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestPointColumnDto {
    pub id: u32,
    pub name: String,
    pub element_type: VegetationPointColumnTypeDto,
}

/// One compiled plant family addressable by cells in the base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestPlantDto {
    pub family: Uuid,
    pub tags: Vec<String>,
    pub source_hash: String,
    pub artifact_hash: String,
    pub local_bounds_min_bits: [i32; 3],
    pub local_bounds_max_bits: [i32; 3],
    pub variation_count: u32,
    pub phenotype_count: u32,
}

/// Spatial reason that one immutable cell reads another cell artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationManifestCellDependencyRoleDto {
    Neighbour,
    Halo,
    Ancestor,
    GlobalStage,
}

/// One exact inter-cell dependency in the immutable manifest directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellDependencyDto {
    pub cell: WorldCellDto,
    pub content_hash: String,
    pub role: VegetationManifestCellDependencyRoleDto,
    pub halo_bits: i32,
}

/// Per-family accepted macro-point count for one cooked cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSpeciesCountDto {
    pub family: Uuid,
    pub macro_count: String,
    pub micro_count: String,
}

/// One independently resident section recorded in the manifest cell directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellSectionDto {
    pub kind: VegetationCellSectionKindDto,
    pub version: u32,
    pub codec: VegetationArtifactSectionCodecDto,
    pub alignment: u32,
    pub stored_size: String,
    pub decoded_size: String,
    pub content_hash: String,
}

/// One immutable cell entry in a complete vegetation base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellDto {
    pub cell: WorldCellDto,
    pub bounds: WorldBoundsDto,
    pub artifact_hash: String,
    pub payload_hash: String,
    pub dependencies: Vec<VegetationManifestCellDependencyDto>,
    pub species_counts: Vec<VegetationSpeciesCountDto>,
    pub macro_count: String,
    pub micro_count: String,
    pub resident_memory_bytes: String,
    pub stored_bytes: String,
    pub estimate: VegetationCookWorkEstimateDto,
    pub actual: VegetationCookWorkActualDto,
    pub sections: Vec<VegetationManifestCellSectionDto>,
}

/// Immutable identity binding every exact input, schema, seed, plant, and cooked base cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBaseManifestDto {
    pub version: u32,
    pub world: Uuid,
    pub map: Uuid,
    pub map_hash: String,
    pub versions: VegetationCookVersionSetDto,
    pub platform: VegetationCookPlatformProfileDto,
    pub cook_graph_hash: String,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    pub seed_namespaces: Vec<VegetationSeedNamespaceDto>,
    pub point_schema_hash: String,
    pub point_columns: Vec<VegetationManifestPointColumnDto>,
    pub plants: Vec<VegetationManifestPlantDto>,
    pub cells: Vec<VegetationManifestCellDto>,
    pub identity: String,
}
