//! Canonical content identities and dependency records for vegetation cooking.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::str::FromStr;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, FieldChannel, MAX_HIERARCHY_LEVEL, SurfaceProviderId, SurfaceRevision,
    WorldBounds, WorldCellKey,
};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    Error, Result, VegetationMapChunkKey, VegetationMapChunkKind, VegetationMapTileKey,
    vegetation_content_hash,
};

const COOK_GRAPH_MAGIC: &[u8; 8] = b"SVCGPH04";

/// Exact SHA-256 identity of canonical vegetation content.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wraps an exact SHA-256 digest.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Hashes canonical bytes with the vegetation SHA-256 implementation.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(vegetation_content_hash(bytes))
    }

    /// Returns the exact digest bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Whether this is the reserved missing identity.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl From<[u8; 32]> for ContentHash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

impl From<ContentHash> for [u8; 32] {
    fn from(value: ContentHash) -> Self {
        value.0
    }
}

impl FromStr for ContentHash {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            });
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let text = std::str::from_utf8(pair).map_err(|_| Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            })?;
            bytes[index] = u8::from_str_radix(text, 16).map_err(|_| Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            })?;
        }
        let hash = Self(bytes);
        if hash.to_string() != value {
            return Err(Error::ArtifactFormat {
                format: "content hash",
                field: "canonicalHex".to_owned(),
            });
        }
        Ok(hash)
    }
}

/// Semantic versions that participate in every cook identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CookVersionSet {
    /// Cook-graph and artifact schema version.
    pub schema: u32,
    /// Source-normalization/compiler semantic version.
    pub compiler: u32,
    /// Biome evaluator semantic version.
    pub evaluator: u32,
    /// Authoritative numeric contract version.
    pub numeric: u32,
    /// Persistent simulation/save compatibility version.
    pub simulation: u32,
}

impl CookVersionSet {
    /// Returns the exact semantic contract set produced by this build.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            schema: 2,
            compiler: 2,
            evaluator: 4,
            numeric: 1,
            simulation: 1,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if [
            self.schema,
            self.compiler,
            self.evaluator,
            self.numeric,
            self.simulation,
        ]
        .contains(&0)
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "versions".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) {
        writer.u32(self.schema);
        writer.u32(self.compiler);
        writer.u32(self.evaluator);
        writer.u32(self.numeric);
        writer.u32(self.simulation);
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let value = Self {
            schema: reader.u32()?,
            compiler: reader.u32()?,
            evaluator: reader.u32()?,
            numeric: reader.u32()?,
            simulation: reader.u32()?,
        };
        value.validate()?;
        Ok(value)
    }
}

/// Complete platform profile that can affect derived artifact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookPlatformProfile {
    /// Rust target triple.
    pub target: String,
    /// Logical content profile, such as `portable-vulkan`.
    pub content_profile: String,
    /// Exact compiler/toolchain identity.
    pub toolchain: String,
    /// Canonical feature vocabulary selected for the artifact.
    pub features: Vec<String>,
}

impl CookPlatformProfile {
    /// Validates and returns the canonical profile identity.
    pub fn identity(&self) -> Result<ContentHash> {
        let mut writer = BinaryWriter::new();
        self.encode(&mut writer)?;
        Ok(ContentHash::of(&writer.finish()))
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        if self.target.is_empty() || self.content_profile.is_empty() || self.toolchain.is_empty() {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "platformProfile".to_owned(),
            });
        }
        let mut features = self.features.clone();
        features.sort_unstable();
        features.dedup();
        if features.iter().any(String::is_empty) {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "platformProfile.features".to_owned(),
            });
        }
        writer.string(&self.target)?;
        writer.string(&self.content_profile)?;
        writer.string(&self.toolchain)?;
        writer.length(features.len())?;
        for feature in features {
            writer.string(&feature)?;
        }
        Ok(())
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let target = reader.string()?;
        let content_profile = reader.string()?;
        let toolchain = reader.string()?;
        let count = reader.count(8)?;
        let mut features = Vec::with_capacity(count);
        for _ in 0..count {
            features.push(reader.string()?);
        }
        let profile = Self {
            target,
            content_profile,
            toolchain,
            features,
        };
        profile.identity()?;
        Ok(profile)
    }
}

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
    fn validate(&self) -> Result<()> {
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

    fn canonical_bytes(&self) -> Vec<u8> {
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
    fn validate(&self) -> Result<()> {
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

/// Exact content and spatial support of one immutable cook dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookDependency {
    /// Stable source address.
    pub address: CookDependencyAddress,
    /// Exact canonical source identity.
    pub content_hash: ContentHash,
    /// Exact source coverage when spatially bounded.
    pub bounds: Option<WorldBounds>,
    /// Composed finite support read beyond the output bounds.
    pub halo: DecisionScalar,
    /// Ancestor level required by propagating/global work.
    pub ancestor_level: Option<u8>,
}

impl CookDependency {
    pub(crate) fn validate(&self) -> Result<()> {
        self.address.validate()?;
        if self.content_hash.is_zero()
            || self.halo.bits() < 0
            || self
                .ancestor_level
                .is_some_and(|level| level > MAX_HIERARCHY_LEVEL)
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook graph",
                field: "dependencies.contentOrSupport".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        self.address.encode(writer)?;
        writer.bytes(&self.content_hash.bytes());
        match self.bounds {
            Some(bounds) => {
                writer.bool(true);
                writer.bounds(bounds);
            }
            None => writer.bool(false),
        }
        writer.i32(self.halo.bits());
        match self.ancestor_level {
            Some(level) => {
                writer.bool(true);
                writer.u8(level);
            }
            None => writer.bool(false),
        }
        Ok(())
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        Ok(Self {
            address: CookDependencyAddress::decode(reader)?,
            content_hash: ContentHash::new(reader.array()?),
            bounds: if reader.bool()? {
                Some(reader.bounds()?)
            } else {
                None
            },
            halo: DecisionScalar::from_bits(reader.i32()?),
            ancestor_level: if reader.bool()? {
                Some(reader.u8()?)
            } else {
                None
            },
        })
    }
}

/// Predicted bounded work for one cook node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CookWorkEstimate {
    /// Stable abstract work units.
    pub work_units: u64,
    /// Peak resident bytes admitted before execution.
    pub peak_memory_bytes: u64,
    /// Canonical input bytes read.
    pub input_bytes: u64,
    /// Canonical output bytes expected.
    pub output_bytes: u64,
}

impl CookWorkEstimate {
    pub(crate) fn encode(&self, writer: &mut BinaryWriter) {
        writer.u64(self.work_units);
        writer.u64(self.peak_memory_bytes);
        writer.u64(self.input_bytes);
        writer.u64(self.output_bytes);
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        Ok(Self {
            work_units: reader.u64()?,
            peak_memory_bytes: reader.u64()?,
            input_bytes: reader.u64()?,
            output_bytes: reader.u64()?,
        })
    }
}

/// Measured execution and cache result for one cook node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CookWorkActual {
    /// Wall-clock execution duration.
    pub elapsed_micros: u64,
    /// Measured peak resident memory.
    pub peak_memory_bytes: u64,
    /// Actual canonical input bytes read.
    pub input_bytes: u64,
    /// Actual canonical output bytes published.
    pub output_bytes: u64,
    /// Typed rejection total produced by this node.
    pub rejection_count: u64,
    /// Whether the node was satisfied by an already validated artifact.
    pub cache_hit: bool,
}

/// One immutable output and every exact input that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookNodeRecord {
    /// Stable logical output address.
    pub address: CookNodeAddress,
    /// Hash of canonical address, versions, platform, and dependencies before execution.
    pub cook_key: ContentHash,
    /// Exact validated output artifact identity.
    pub output_hash: ContentHash,
    /// Canonical exact dependencies.
    pub dependencies: Vec<CookDependency>,
    /// Preflight work prediction.
    pub estimate: CookWorkEstimate,
    /// Measured execution/cache statistics for the live job; excluded from canonical bytes.
    pub actual: CookWorkActual,
}

impl CookNodeRecord {
    /// Computes the exact pre-execution identity of this node's address and immutable inputs.
    pub fn calculate_cook_key(
        &self,
        versions: CookVersionSet,
        platform: &CookPlatformProfile,
    ) -> Result<ContentHash> {
        versions.validate()?;
        platform.identity()?;
        self.address.validate()?;
        let dependencies = canonical_dependencies(&self.dependencies)?;
        let mut writer = BinaryWriter::new();
        writer.bytes(b"saffron-anima/vegetation-cook-node/v2\0");
        versions.encode(&mut writer);
        platform.encode(&mut writer)?;
        self.address.encode(&mut writer);
        writer.length(dependencies.len())?;
        for (_, dependency) in dependencies {
            dependency.encode(&mut writer)?;
        }
        Ok(ContentHash::of(&writer.finish()))
    }

    fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        self.address.encode(writer);
        writer.bytes(&self.cook_key.bytes());
        writer.bytes(&self.output_hash.bytes());
        let dependencies = canonical_dependencies(&self.dependencies)?;
        writer.length(dependencies.len())?;
        for (_, dependency) in dependencies {
            dependency.encode(writer)?;
        }
        self.estimate.encode(writer);
        Ok(())
    }

    fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let address = CookNodeAddress::decode(reader)?;
        let cook_key = ContentHash::new(reader.array()?);
        let output_hash = ContentHash::new(reader.array()?);
        let count = reader.count(44)?;
        let mut dependencies = Vec::with_capacity(count);
        for _ in 0..count {
            dependencies.push(CookDependency::decode(reader)?);
        }
        Ok(Self {
            address,
            cook_key,
            output_hash,
            dependencies,
            estimate: CookWorkEstimate::decode(reader)?,
            actual: CookWorkActual::default(),
        })
    }
}

/// Canonical dependency DAG for one complete vegetation-world generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookGraph {
    /// Exact contract versions.
    pub versions: CookVersionSet,
    /// Complete platform profile.
    pub platform: CookPlatformProfile,
    /// Every output node in canonical logical-address order.
    pub nodes: Vec<CookNodeRecord>,
}

impl CookGraph {
    /// Writes a schedule-independent canonical graph record.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.versions.validate()?;
        self.platform.identity()?;
        let mut writer = BinaryWriter::new();
        writer.bytes(COOK_GRAPH_MAGIC);
        self.versions.encode(&mut writer);
        self.platform.encode(&mut writer)?;
        let mut nodes = self.nodes.clone();
        nodes.sort_by_cached_key(|node| node.address.canonical_bytes());
        for pair in nodes.windows(2) {
            if pair[0].address == pair[1].address {
                return Err(Error::ArtifactFormat {
                    format: "vegetation cook graph",
                    field: "nodes.duplicateAddress".to_owned(),
                });
            }
        }
        let addresses = nodes
            .iter()
            .map(|node| node.address.clone())
            .collect::<BTreeSet<_>>();
        validate_node_dependencies(&nodes, &addresses)?;
        for node in &nodes {
            if node.cook_key != node.calculate_cook_key(self.versions, &self.platform)? {
                return Err(Error::ArtifactFormat {
                    format: "vegetation cook graph",
                    field: "nodes.cookKey".to_owned(),
                });
            }
        }
        writer.length(nodes.len())?;
        for node in nodes {
            node.encode(&mut writer)?;
        }
        Ok(writer.finish())
    }

    /// Strictly reads a canonical cook graph and rejects reordered or malformed records.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation cook graph");
        reader.expect(COOK_GRAPH_MAGIC, "magic")?;
        let versions = CookVersionSet::decode(&mut reader)?;
        let platform = CookPlatformProfile::decode(&mut reader)?;
        let count = reader.count(109)?;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            nodes.push(CookNodeRecord::decode(&mut reader)?);
        }
        reader.complete()?;
        let graph = Self {
            versions,
            platform,
            nodes,
        };
        if graph.canonical_bytes()? != bytes {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook graph",
                field: "nonCanonicalOrdering".to_owned(),
            });
        }
        Ok(graph)
    }

    /// Complete content identity of the canonical DAG and all recorded outcomes.
    pub fn identity(&self) -> Result<ContentHash> {
        Ok(ContentHash::of(&self.canonical_bytes()?))
    }

    /// Returns every node transitively invalidated by exact changed dependency addresses.
    pub fn invalidated_nodes(
        &self,
        changed: &[CookDependencyAddress],
    ) -> Result<Vec<CookNodeAddress>> {
        self.canonical_bytes()?;
        let mut reverse = BTreeMap::<Vec<u8>, Vec<CookNodeAddress>>::new();
        for node in &self.nodes {
            for dependency in &node.dependencies {
                reverse
                    .entry(dependency.address.canonical_bytes()?)
                    .or_default()
                    .push(node.address.clone());
            }
        }
        let mut pending = VecDeque::new();
        for address in changed {
            pending.push_back(address.canonical_bytes()?);
        }
        let mut invalidated = BTreeSet::new();
        while let Some(address) = pending.pop_front() {
            for node in reverse.get(&address).into_iter().flatten() {
                if invalidated.insert(node.clone()) {
                    pending.push_back(CookDependencyAddress::Node(node.clone()).canonical_bytes()?);
                }
            }
        }
        Ok(invalidated.into_iter().collect())
    }
}

fn validate_node_dependencies(
    nodes: &[CookNodeRecord],
    addresses: &BTreeSet<CookNodeAddress>,
) -> Result<()> {
    let outputs = nodes
        .iter()
        .map(|node| (node.address.clone(), node.output_hash))
        .collect::<BTreeMap<_, _>>();
    let mut inbound = BTreeMap::<CookNodeAddress, BTreeSet<CookNodeAddress>>::new();
    for node in nodes {
        node.address.validate()?;
        if node.cook_key.is_zero() || node.output_hash.is_zero() {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook graph",
                field: "nodes.contentIdentity".to_owned(),
            });
        }
        for dependency in &node.dependencies {
            dependency.validate()?;
            if let CookDependencyAddress::Node(parent) = &dependency.address {
                if !addresses.contains(parent) || parent == &node.address {
                    return Err(Error::ArtifactFormat {
                        format: "vegetation cook graph",
                        field: "dependencies.node".to_owned(),
                    });
                }
                if outputs.get(parent) != Some(&dependency.content_hash) {
                    return Err(Error::ArtifactFormat {
                        format: "vegetation cook graph",
                        field: "dependencies.nodeContentHash".to_owned(),
                    });
                }
                inbound
                    .entry(node.address.clone())
                    .or_default()
                    .insert(parent.clone());
            }
        }
    }
    let mut complete = BTreeSet::new();
    let mut active = BTreeSet::new();
    for address in addresses {
        visit_node(address, &inbound, &mut active, &mut complete)?;
    }
    Ok(())
}

fn canonical_dependencies(
    dependencies: &[CookDependency],
) -> Result<Vec<(Vec<u8>, CookDependency)>> {
    let mut dependencies = dependencies
        .iter()
        .cloned()
        .map(|dependency| {
            dependency.validate()?;
            Ok((dependency.address.canonical_bytes()?, dependency))
        })
        .collect::<Result<Vec<_>>>()?;
    dependencies.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if dependencies.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(Error::ArtifactFormat {
            format: "vegetation cook graph",
            field: "dependencies.duplicateAddress".to_owned(),
        });
    }
    Ok(dependencies)
}

fn visit_node(
    node: &CookNodeAddress,
    inbound: &BTreeMap<CookNodeAddress, BTreeSet<CookNodeAddress>>,
    active: &mut BTreeSet<CookNodeAddress>,
    complete: &mut BTreeSet<CookNodeAddress>,
) -> Result<()> {
    if complete.contains(node) {
        return Ok(());
    }
    if !active.insert(node.clone()) {
        return Err(Error::ArtifactFormat {
            format: "vegetation cook graph",
            field: "dependencies.cycle".to_owned(),
        });
    }
    for dependency in inbound.get(node).into_iter().flatten() {
        visit_node(dependency, inbound, active, complete)?;
    }
    active.remove(node);
    complete.insert(node.clone());
    Ok(())
}

pub(crate) fn encode_field_channel(writer: &mut BinaryWriter, channel: FieldChannel) {
    let (tag, user) = channel.canonical_code();
    writer.u8(tag);
    writer.u64(user);
}

pub(crate) fn decode_field_channel(reader: &mut BinaryReader<'_>) -> Result<FieldChannel> {
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

#[cfg(test)]
mod tests {
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
    fn invalidation_follows_exact_node_edges() {
        let plant = CookNodeAddress::Plant { family: Uuid(20) };
        let cell = CookNodeAddress::Cell {
            map: Uuid(10),
            cell: WorldCellKey::base(-4, 3, 1),
        };
        let graph = graph(vec![
            node(
                plant.clone(),
                vec![dependency(
                    CookDependencyAddress::SourceAsset { asset: Uuid(20) },
                    1,
                )],
            ),
            node(
                cell.clone(),
                vec![dependency(CookDependencyAddress::Node(plant.clone()), 8)],
            ),
        ]);
        assert_eq!(
            graph
                .invalidated_nodes(&[CookDependencyAddress::SourceAsset { asset: Uuid(20) }])
                .unwrap(),
            vec![plant, cell]
        );
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
}
