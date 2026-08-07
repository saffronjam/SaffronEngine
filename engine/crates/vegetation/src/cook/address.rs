//! Cook node and dependency addresses, and their canonical encodings.

use saffron_core::Uuid;
use saffron_spatial::{
    FieldChannel, SurfaceProviderId, SurfaceRevision, WorldBounds, WorldCellKey,
};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{Error, Result, VegetationMapChunkKey, VegetationMapChunkKind, VegetationMapTileKey};

use super::ContentHash;

/// Stable output address in the one vegetation cook graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CookNodeAddress {
    /// One normalized compiled plant family.
    Plant { family: Uuid },
    /// One compiler-owned hierarchy/global stage tile.
    GlobalStage {
        map: Uuid,
        biome_instance: u128,
        stage: ContentHash,
        owner: WorldCellKey,
    },
    /// One complete immutable vegetation cell.
    Cell { map: Uuid, cell: WorldCellKey },
}

impl PartialOrd for CookNodeAddress {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CookNodeAddress {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.canonical_bytes().cmp(&other.canonical_bytes())
    }
}

impl CookNodeAddress {
    pub(crate) fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Plant { family } => family.value() != 0,
            Self::GlobalStage {
                map,
                biome_instance,
                stage,
                ..
            } => map.value() != 0 && *biome_instance != 0 && !stage.is_zero(),
            Self::Cell { map, .. } => map.value() != 0,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "nodeAddress".to_owned(),
            })
        }
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) {
        match self {
            Self::Plant { family } => {
                writer.u8(0);
                writer.uuid(*family);
            }
            Self::GlobalStage {
                map,
                biome_instance,
                stage,
                owner,
            } => {
                writer.u8(1);
                writer.uuid(*map);
                writer.u128(*biome_instance);
                writer.bytes(&stage.bytes());
                writer.cell(*owner);
            }
            Self::Cell { map, cell } => {
                writer.u8(2);
                writer.uuid(*map);
                writer.cell(*cell);
            }
        }
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        match reader.u8()? {
            0 => Ok(Self::Plant {
                family: reader.uuid()?,
            }),
            1 => Ok(Self::GlobalStage {
                map: reader.uuid()?,
                biome_instance: reader.u128()?,
                stage: ContentHash::new(reader.array()?),
                owner: reader.cell()?,
            }),
            2 => Ok(Self::Cell {
                map: reader.uuid()?,
                cell: reader.cell()?,
            }),
            _ => Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "nodeAddress".to_owned(),
            }),
        }
    }

    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let mut writer = BinaryWriter::new();
        self.encode(&mut writer);
        writer.finish()
    }
}

/// Exact immutable input address read by a cook node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CookDependencyAddress {
    /// Catalog source asset bytes.
    SourceAsset { asset: Uuid },
    /// Canonical project-relative or file-URI source bytes.
    SourceFile { uri: String },
    /// Material and alpha-coverage schema used by a plant family.
    MaterialCoverage { material: Uuid },
    /// Canonical compiled biome IR for one map instance.
    BiomeIr { map: Uuid, instance: u128 },
    /// Authored map manifest root.
    MapManifest { map: Uuid },
    /// One exact immutable object resolved through the authored map inventory.
    MapObject {
        map: Uuid,
        key: VegetationMapChunkKey,
    },
    /// Complete immutable surface-provider snapshot.
    SurfaceProvider {
        provider: SurfaceProviderId,
        revision: SurfaceRevision,
    },
    /// One exact canonical surface/field tile.
    SurfaceTile {
        provider: SurfaceProviderId,
        revision: SurfaceRevision,
        channel: Option<FieldChannel>,
        bounds: WorldBounds,
    },
    /// Compiler/schema/numeric/platform vocabulary entry.
    Contract { namespace: String },
    /// Output from another node in the same cook graph.
    Node(CookNodeAddress),
}

impl CookDependencyAddress {
    pub(super) fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::SourceAsset { asset } => asset.value() != 0,
            Self::SourceFile { uri } => !uri.trim().is_empty(),
            Self::MaterialCoverage { material } => material.value() != 0,
            Self::BiomeIr { map, instance } => map.value() != 0 && *instance != 0,
            Self::MapManifest { map } => map.value() != 0,
            Self::MapObject { map, key } => {
                map.value() != 0
                    && key.layer != 0
                    && matches!(
                        (key.kind, key.tile),
                        (
                            VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride,
                            VegetationMapTileKey::Cell(_)
                        ) | (
                            VegetationMapChunkKind::GraphInstance
                                | VegetationMapChunkKind::LayerMetadata
                                | VegetationMapChunkKind::EditorMetadata,
                            VegetationMapTileKey::Global
                        )
                    )
            }
            Self::SurfaceProvider { provider, revision } => provider.0 != 0 && revision.0 != 0,
            Self::SurfaceTile {
                provider, revision, ..
            } => provider.0 != 0 && revision.0 != 0,
            Self::Contract { namespace } => !namespace.is_empty(),
            Self::Node(node) => return node.validate(),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "dependencyAddress".to_owned(),
            })
        }
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        match self {
            Self::SourceAsset { asset } => {
                writer.u8(0);
                writer.uuid(*asset);
            }
            Self::SourceFile { uri } => {
                writer.u8(1);
                writer.string(uri)?;
            }
            Self::MaterialCoverage { material } => {
                writer.u8(2);
                writer.uuid(*material);
            }
            Self::BiomeIr { map, instance } => {
                writer.u8(3);
                writer.uuid(*map);
                writer.u128(*instance);
            }
            Self::MapManifest { map } => {
                writer.u8(4);
                writer.uuid(*map);
            }
            Self::MapObject { map, key } => {
                writer.u8(5);
                writer.uuid(*map);
                encode_map_object_key(writer, *key);
            }
            Self::SurfaceProvider { provider, revision } => {
                writer.u8(6);
                writer.u64(provider.0);
                writer.u64(revision.0);
            }
            Self::SurfaceTile {
                provider,
                revision,
                channel,
                bounds,
            } => {
                writer.u8(7);
                writer.u64(provider.0);
                writer.u64(revision.0);
                match channel {
                    Some(channel) => {
                        writer.bool(true);
                        encode_field_channel(writer, *channel);
                    }
                    None => writer.bool(false),
                }
                writer.bounds(*bounds);
            }
            Self::Contract { namespace } => {
                writer.u8(8);
                writer.string(namespace)?;
            }
            Self::Node(node) => {
                writer.u8(9);
                node.encode(writer);
            }
        }
        Ok(())
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        match reader.u8()? {
            0 => Ok(Self::SourceAsset {
                asset: reader.uuid()?,
            }),
            1 => Ok(Self::SourceFile {
                uri: reader.string()?,
            }),
            2 => Ok(Self::MaterialCoverage {
                material: reader.uuid()?,
            }),
            3 => Ok(Self::BiomeIr {
                map: reader.uuid()?,
                instance: reader.u128()?,
            }),
            4 => Ok(Self::MapManifest {
                map: reader.uuid()?,
            }),
            5 => Ok(Self::MapObject {
                map: reader.uuid()?,
                key: decode_map_object_key(reader)?,
            }),
            6 => Ok(Self::SurfaceProvider {
                provider: SurfaceProviderId(reader.u64()?),
                revision: SurfaceRevision(reader.u64()?),
            }),
            7 => Ok(Self::SurfaceTile {
                provider: SurfaceProviderId(reader.u64()?),
                revision: SurfaceRevision(reader.u64()?),
                channel: if reader.bool()? {
                    Some(decode_field_channel(reader)?)
                } else {
                    None
                },
                bounds: reader.bounds()?,
            }),
            8 => Ok(Self::Contract {
                namespace: reader.string()?,
            }),
            9 => Ok(Self::Node(CookNodeAddress::decode(reader)?)),
            _ => Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "dependencyAddress".to_owned(),
            }),
        }
    }

    /// Returns the canonical identity bytes used to order and deduplicate dependencies.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut writer = BinaryWriter::new();
        self.encode(&mut writer)?;
        Ok(writer.finish())
    }
}

fn encode_map_object_key(writer: &mut BinaryWriter, key: VegetationMapChunkKey) {
    writer.u128(key.layer);
    match key.tile {
        VegetationMapTileKey::Global => writer.u8(0),
        VegetationMapTileKey::Cell(cell) => {
            writer.u8(1);
            writer.cell(cell);
        }
    }
    writer.u8(match key.kind {
        VegetationMapChunkKind::Field => 0,
        VegetationMapChunkKind::AnchorOverride => 1,
        VegetationMapChunkKind::GraphInstance => 2,
        VegetationMapChunkKind::LayerMetadata => 3,
        VegetationMapChunkKind::EditorMetadata => 4,
    });
}

fn decode_map_object_key(reader: &mut BinaryReader<'_>) -> Result<VegetationMapChunkKey> {
    let layer = reader.u128()?;
    let tile = match reader.u8()? {
        0 => VegetationMapTileKey::Global,
        1 => VegetationMapTileKey::Cell(reader.cell()?),
        _ => {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "dependencyAddress.mapObject.tile".to_owned(),
            });
        }
    };
    let kind = match reader.u8()? {
        0 => VegetationMapChunkKind::Field,
        1 => VegetationMapChunkKind::AnchorOverride,
        2 => VegetationMapChunkKind::GraphInstance,
        3 => VegetationMapChunkKind::LayerMetadata,
        4 => VegetationMapChunkKind::EditorMetadata,
        _ => {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "dependencyAddress.mapObject.kind".to_owned(),
            });
        }
    };
    Ok(VegetationMapChunkKey { layer, tile, kind })
}

fn encode_field_channel(writer: &mut BinaryWriter, channel: FieldChannel) {
    let (tag, user) = channel.canonical_code();
    writer.u8(tag);
    writer.u64(user);
}

fn decode_field_channel(reader: &mut BinaryReader<'_>) -> Result<FieldChannel> {
    match (reader.u8()?, reader.u64()?) {
        (0, 0) => Ok(FieldChannel::Altitude),
        (1, 0) => Ok(FieldChannel::Slope),
        (2, 0) => Ok(FieldChannel::Curvature),
        (3, 0) => Ok(FieldChannel::Concavity),
        (4, 0) => Ok(FieldChannel::Drainage),
        (5, 0) => Ok(FieldChannel::Moisture),
        (6, 0) => Ok(FieldChannel::Temperature),
        (7, 0) => Ok(FieldChannel::Precipitation),
        (8, 0) => Ok(FieldChannel::Sunlight),
        (9, 0) => Ok(FieldChannel::Exposure),
        (10, 0) => Ok(FieldChannel::WaterDistance),
        (11, 0) => Ok(FieldChannel::WaterDepth),
        (12, 0) => Ok(FieldChannel::SignedBlocker),
        (13, 0) => Ok(FieldChannel::SplineDistance),
        (14, value) => Ok(FieldChannel::User(value)),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation cook",
            field: "fieldChannel".to_owned(),
        }),
    }
}
