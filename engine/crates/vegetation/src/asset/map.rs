use std::collections::BTreeSet;

use saffron_core::Uuid;
use saffron_spatial::{FieldChannel, WorldBounds, WorldCellKey};

use crate::{
    BrushGestureMetadata, Error, PlantId, PlantPoint, PlantStateOverride, PlantTransformOverride,
    ProvenanceTable, Result, VegetationLayer,
};

use super::biome::LocalBiomeInstance;

/// Current `.svegmap` manifest version.
pub const VEGETATION_MAP_VERSION: u32 = 2;
/// Current sparse authored map-chunk version.
pub const VEGETATION_MAP_CHUNK_VERSION: u32 = 3;

/// Stable sparse chunk address policy for a vegetation map package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationMapChunkLayout {
    /// World-cell level owning authored chunks.
    pub level: u8,
    /// Canonical chunk schema identity.
    pub schema_hash: [u8; 32],
}

/// Address class for one immutable authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VegetationMapChunkKind {
    /// Quantized field and blocker samples.
    Field,
    /// Explicit anchors, pins, state, transforms, and their provenance.
    AnchorOverride,
    /// One local biome-graph instance.
    GraphInstance,
    /// One ordered layer definition.
    LayerMetadata,
    /// Optional non-authoritative editor gesture metadata.
    EditorMetadata,
}

/// Spatial address of one immutable authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VegetationMapTileKey {
    /// Map-global metadata.
    Global,
    /// One sparse authored world tile.
    Cell(WorldCellKey),
}

/// Stable logical address resolved through the `.svegmap` root inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationMapChunkKey {
    /// Stable layer or instance identity.
    pub layer: u128,
    /// Global or spatial tile address.
    pub tile: VegetationMapTileKey,
    /// Typed payload vocabulary.
    pub kind: VegetationMapChunkKind,
}

/// Root reference to one immutable content-addressed authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationMapChunkReference {
    /// Logical address replaced by a newer transaction.
    pub key: VegetationMapChunkKey,
    /// SHA-256 of the exact canonical object bytes.
    pub content_hash: [u8; 32],
    /// Exact canonical byte length.
    pub byte_length: u64,
    /// Monotonic authored object revision.
    pub revision: u64,
}

impl VegetationMapChunkReference {
    /// Canonical inventory order.
    #[must_use]
    pub fn order_key(&self) -> VegetationMapChunkKey {
        self.key
    }
}

/// One logical `.svegmap` catalog root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapAsset {
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    pub name: String,
    /// Exact world coverage.
    pub bounds: WorldBounds,
    /// Sparse authored chunk policy. Chunk inventory is intentionally external.
    pub chunk_layout: VegetationMapChunkLayout,
    /// Monotonic committed root generation.
    pub generation: u64,
    /// Canonically ordered logical-address to immutable-object references.
    pub inventory: Vec<VegetationMapChunkReference>,
}

/// One quantized authored field tile inside a sparse map chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredFieldTile {
    pub channel: FieldChannel,
    /// Stable layer owning the tile.
    pub layer: u128,
    /// Tile dimensions.
    pub dimensions: [u32; 3],
    /// Quantization step in Q15.16 bits.
    pub quantum_bits: i32,
    /// Canonical packed signed values.
    pub values: Vec<i32>,
}

/// One explicit authored point anchor stored in a map chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplicitPlantAnchor {
    /// Explicit-namespace identity.
    pub id: PlantId,
    /// Stable authored vegetation layer owning this anchor.
    pub layer: u128,
    /// Plant-family asset.
    pub family: Uuid,
    /// Complete point-row data, projected into canonical columns during cooking.
    pub point: PlantPoint,
}

/// Quantized authored truth for one layer and spatial tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapFieldChunk {
    /// Quantized scalar/vector/species fields.
    pub fields: Vec<AuthoredFieldTile>,
    /// Signed blocker tile/category data.
    pub blockers: Vec<AuthoredFieldTile>,
}

/// Anchors, pins, authored overrides, and provenance for one layer and spatial tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapAnchorChunk {
    /// Explicit authored plants/anchors.
    pub explicit_plants: Vec<ExplicitPlantAnchor>,
    pub pins: Vec<PlantId>,
    pub transform_overrides: Vec<PlantTransformOverride>,
    pub state_overrides: Vec<PlantStateOverride>,
    /// Chunk-local compact provenance.
    pub provenance: ProvenanceTable,
}

/// One typed immutable authored map-object payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationMapChunkPayload {
    /// Quantized field and blocker samples.
    Field(VegetationMapFieldChunk),
    /// Explicit anchors, pins, overrides, and provenance.
    AnchorOverride(VegetationMapAnchorChunk),
    /// One local root-biome graph instance.
    GraphInstance(LocalBiomeInstance),
    /// One ordered layer definition.
    LayerMetadata(VegetationLayer),
    /// Optional non-authoritative editor gesture metadata.
    EditorMetadata(Vec<BrushGestureMetadata>),
}

impl VegetationMapChunkPayload {
    /// Address kind required by this payload.
    #[must_use]
    pub fn kind(&self) -> VegetationMapChunkKind {
        match self {
            Self::Field(_) => VegetationMapChunkKind::Field,
            Self::AnchorOverride(_) => VegetationMapChunkKind::AnchorOverride,
            Self::GraphInstance(_) => VegetationMapChunkKind::GraphInstance,
            Self::LayerMetadata(_) => VegetationMapChunkKind::LayerMetadata,
            Self::EditorMetadata(_) => VegetationMapChunkKind::EditorMetadata,
        }
    }
}

/// One immutable content-addressed authored `.svegmap` object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapChunk {
    /// Chunk format version.
    pub version: u32,
    /// Owning map.
    pub map: Uuid,
    /// Exact logical object address.
    pub key: VegetationMapChunkKey,
    /// Monotonic authored revision.
    pub revision: u64,
    pub payload: VegetationMapChunkPayload,
}

impl VegetationMapChunk {
    /// Builds the exact root reference for these canonical object bytes.
    ///
    /// # Errors
    ///
    /// Propagates encoding, and [`Error::NumericOverflow`] when the object does not fit a `u64`.
    pub fn reference(&self) -> Result<VegetationMapChunkReference> {
        let bytes = crate::write_vegetation_map_chunk(self)?;
        Ok(VegetationMapChunkReference {
            key: self.key,
            content_hash: crate::vegetation_content_hash(&bytes),
            byte_length: u64::try_from(bytes.len()).map_err(|_| Error::NumericOverflow)?,
            revision: self.revision,
        })
    }
}

/// Fully resolved logical map used by authoring and evaluation callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapSnapshot {
    /// Atomically visible root generation.
    pub root: VegetationMapAsset,
    /// Ordered layer algebra reconstructed from layer-metadata objects.
    pub layers: Vec<VegetationLayer>,
    /// Local biome instances reconstructed from graph-instance objects.
    pub biome_instances: Vec<LocalBiomeInstance>,
    /// Optional editor-only gesture records.
    pub brush_history: Vec<BrushGestureMetadata>,
    /// Canonically addressed immutable object set.
    pub chunks: Vec<VegetationMapChunk>,
}

impl std::ops::Deref for VegetationMapSnapshot {
    type Target = VegetationMapAsset;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

/// Resolved spatial authored truth assembled from typed objects at one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapTileSnapshot {
    /// Owning map.
    pub map: Uuid,
    /// Exact canonical tile.
    pub cell: WorldCellKey,
    /// Quantized scalar/vector/species fields.
    pub fields: Vec<AuthoredFieldTile>,
    /// Signed blocker tile/category data.
    pub blockers: Vec<AuthoredFieldTile>,
    /// Explicit authored plants/anchors.
    pub explicit_plants: Vec<ExplicitPlantAnchor>,
    pub pins: Vec<PlantId>,
    pub transform_overrides: Vec<PlantTransformOverride>,
    pub state_overrides: Vec<PlantStateOverride>,
    /// Tile-local compact provenance.
    pub provenance: ProvenanceTable,
}

/// Pins/overrides invalidated by an identity-affecting seed/topology edit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentityConflictReport {
    /// Pins whose procedural identities no longer exist.
    pub invalidated_pins: Vec<PlantId>,
    /// Transform/state overrides whose targets no longer exist.
    pub invalidated_overrides: Vec<PlantId>,
}

/// Computes an explicit conflict report between the old/new accepted identity sets.
#[must_use]
pub fn identity_conflicts(
    old_ids: &[PlantId],
    new_ids: &[PlantId],
    pins: &[PlantId],
    overrides: &[PlantId],
) -> IdentityConflictReport {
    let old: BTreeSet<_> = old_ids.iter().copied().collect();
    let new: BTreeSet<_> = new_ids.iter().copied().collect();
    let disappeared: BTreeSet<_> = old.difference(&new).copied().collect();
    let mut report = IdentityConflictReport {
        invalidated_pins: pins
            .iter()
            .copied()
            .filter(|id| disappeared.contains(id))
            .collect(),
        invalidated_overrides: overrides
            .iter()
            .copied()
            .filter(|id| disappeared.contains(id))
            .collect(),
    };
    report.invalidated_pins.sort();
    report.invalidated_pins.dedup();
    report.invalidated_overrides.sort();
    report.invalidated_overrides.dedup();
    report
}

/// Validates one map root without reading its immutable authored objects.
///
/// # Errors
///
/// [`Error::FormatVersion`] on a noncurrent root, and [`Error::InvalidFormat`] when the inventory is
/// out of canonical order or an address does not match its payload kind.
pub fn validate_vegetation_map(asset: &VegetationMapAsset) -> Result<()> {
    if asset.version != VEGETATION_MAP_VERSION {
        return Err(Error::FormatVersion {
            format: ".svegmap",
            found: asset.version,
            expected: VEGETATION_MAP_VERSION,
        });
    }
    if asset.id.value() == 0
        || asset.name.is_empty()
        || asset.chunk_layout.level > 62
        || asset.chunk_layout.schema_hash != crate::vegetation_map_chunk_schema_hash()
    {
        return Err(field("identity/chunkLayout"));
    }
    let mut previous = None;
    for reference in &asset.inventory {
        if reference.key.layer == 0
            || reference.byte_length == 0
            || previous.is_some_and(|key| key >= reference.key)
        {
            return Err(field("inventory"));
        }
        match (reference.key.kind, reference.key.tile) {
            (
                VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride,
                VegetationMapTileKey::Cell(cell),
            ) if cell.level() == asset.chunk_layout.level => {}
            (
                VegetationMapChunkKind::GraphInstance
                | VegetationMapChunkKind::LayerMetadata
                | VegetationMapChunkKind::EditorMetadata,
                VegetationMapTileKey::Global,
            ) => {}
            _ => return Err(field("inventory.key")),
        }
        previous = Some(reference.key);
    }
    Ok(())
}

fn field(name: &str) -> Error {
    Error::InvalidFormat {
        format: ".svegmap",
        field: name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProceduralPlantIdentity;
    use crate::identity::derive_procedural_plant_id;

    #[test]
    fn topology_changes_report_affected_pins_and_overrides() {
        let id = |candidate| {
            derive_procedural_plant_id(ProceduralPlantIdentity {
                map: Uuid(1),
                layer_guid: 2,
                node_address: 3,
                node_semantic_revision: 4,
                candidate,
                ancestor: 0,
                seed_namespace: 5,
                owner: WorldCellKey::base(0, 0, 0),
                family: Uuid(6),
            })
        };
        let report = identity_conflicts(&[id(1), id(2)], &[id(2), id(3)], &[id(1)], &[id(1)]);
        assert_eq!(report.invalidated_pins, vec![id(1)]);
        assert_eq!(report.invalidated_overrides, vec![id(1)]);
    }
}
