//! The distributed cook work-item manifest: the wire contract under which one planned
//! generation's cell cooks execute as independently claimable items.
//!
//! The plan phase evaluates the map and publishes one [`CookWorkManifest`] whose items are the
//! generation's cells in phase-J order (coarsest level first, then cell order). An item's
//! `blocked_by` lists the items it may not complete before — its containing candidate-ancestor
//! cells — and every listed index is strictly less than the item's own index, so acyclicity
//! holds by construction. A claimant recomputes the item's [`cook_work_own_input_key`] from the
//! payload it reads, composes the full cook key only after its ancestors' completions publish
//! their output hashes, and records a [`CookWorkCompletion`] the single committer assembles in
//! item order.
//!
//! This module owns only the value contracts; claiming, leases, payload publication, and the
//! completion markers are filesystem behavior and live in `saffron-assets`.

use saffron_core::Uuid;

use crate::binary::{BinaryReader, BinaryWriter};
use crate::cook::ContentHash;
use crate::cook::{
    CookDependency, CookNodeAddress, CookNodeRecord, CookPlatformProfile, CookVersionSet,
    CookWorkActual, CookWorkEstimate, canonical_dependencies,
};
use crate::error::{Error, Result};
use crate::manifest::{VegetationManifestCell, decode_cell, encode_cell};

const COOK_WORK_MANIFEST_MAGIC: &[u8; 8] = b"SVCWRK01";
const COOK_WORK_PAYLOAD_MAGIC: &[u8; 8] = b"SVCWPL01";
const COOK_WORK_COMPLETION_MAGIC: &[u8; 8] = b"SVCWCP01";

/// One claimable unit of cell cook work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookWorkItem {
    /// The cell node this item publishes.
    pub address: CookNodeAddress,
    /// Identity of the item's own (ancestor-independent) inputs, under its own domain string
    /// so it can never be mistaken for a [`CookNodeRecord`] cook key. A claimant recomputes it
    /// from the payload and refuses a mismatch.
    pub own_input_key: ContentHash,
    /// Content identity of the item's published work payload — the canonical own-input record
    /// ([`CookWorkPayload`]) the claimant reads.
    pub payload: ContentHash,
    /// Indices of the items whose completions this item's key composition waits on — its
    /// containing candidate-ancestor cells. Every index is strictly less than this item's own.
    pub blocked_by: Vec<u32>,
    /// Preflight work prediction, for claimant scheduling.
    pub estimate: CookWorkEstimate,
}

impl CookWorkItem {
    fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        self.address.encode(writer);
        writer.bytes(&self.own_input_key.bytes());
        writer.bytes(&self.payload.bytes());
        writer.length(self.blocked_by.len())?;
        for &blocker in &self.blocked_by {
            writer.u32(blocker);
        }
        self.estimate.encode(writer);
        Ok(())
    }

    fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let address = CookNodeAddress::decode(reader)?;
        let own_input_key = ContentHash::new(reader.array()?);
        let payload = ContentHash::new(reader.array()?);
        let count = reader.count(8)?;
        let mut blocked_by = Vec::with_capacity(count);
        for _ in 0..count {
            blocked_by.push(reader.u32()?);
        }
        Ok(Self {
            address,
            own_input_key,
            payload,
            blocked_by,
            estimate: CookWorkEstimate::decode(reader)?,
        })
    }
}

/// The complete claimable plan for one generation's cell cooks.
///
/// A full-generation plan always: closure validation forbids a cell referencing a prior
/// generation's ancestor, so the item set is every cell the generation cooks, in phase-J
/// order. Plants and global stages are plan-phase work and are not items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookWorkManifest {
    /// Exact contract versions the items cook under.
    pub versions: CookVersionSet,
    /// Complete platform profile the items cook for.
    pub platform: CookPlatformProfile,
    /// The world the generation belongs to.
    pub world: Uuid,
    /// The authored map the generation cooks.
    pub map: Uuid,
    /// The immutable ecology snapshot tick.
    pub ecology_tick: u64,
    /// The generation this plan supersedes, absent for a first cook.
    pub expected_manifest: Option<ContentHash>,
    /// Exact content identity shared by every surface-derived tile in the plan. A claimant
    /// refuses a payload whose tiles were quantized under a different provider set.
    pub surface_provider_set_hash: ContentHash,
    /// The claimable items, in phase-J order.
    pub items: Vec<CookWorkItem>,
}

impl CookWorkManifest {
    /// Rejects a plan whose items are not a well-formed wavefront: a `blocked_by` at or past
    /// its own item, a duplicate address, or a non-cell address.
    pub fn validate(&self) -> Result<()> {
        self.versions.validate()?;
        self.platform.identity()?;
        let mut addresses = std::collections::BTreeSet::new();
        for (index, item) in self.items.iter().enumerate() {
            item.address.validate()?;
            if !matches!(item.address, CookNodeAddress::Cell { .. }) {
                return Err(Error::ArtifactFormat {
                    format: "vegetation cook work manifest",
                    field: "items.address".to_owned(),
                });
            }
            if !addresses.insert(item.address.canonical_bytes()) {
                return Err(Error::ArtifactFormat {
                    format: "vegetation cook work manifest",
                    field: "items.duplicateAddress".to_owned(),
                });
            }
            for &blocker in &item.blocked_by {
                if usize::try_from(blocker).map_err(|_| Error::NumericOverflow)? >= index {
                    return Err(Error::ArtifactFormat {
                        format: "vegetation cook work manifest",
                        field: "items.blockedBy".to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Writes the canonical plan record.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut writer = BinaryWriter::new();
        writer.bytes(COOK_WORK_MANIFEST_MAGIC);
        self.versions.encode(&mut writer);
        self.platform.encode(&mut writer)?;
        writer.uuid(self.world);
        writer.uuid(self.map);
        writer.u64(self.ecology_tick);
        writer.bool(self.expected_manifest.is_some());
        if let Some(expected) = self.expected_manifest {
            writer.bytes(&expected.bytes());
        }
        writer.bytes(&self.surface_provider_set_hash.bytes());
        writer.length(self.items.len())?;
        for item in &self.items {
            item.encode(&mut writer)?;
        }
        Ok(writer.finish())
    }

    /// Strictly reads a canonical plan and rejects a malformed wavefront.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation cook work manifest");
        reader.expect(COOK_WORK_MANIFEST_MAGIC, "magic")?;
        let versions = CookVersionSet::decode(&mut reader)?;
        let platform = CookPlatformProfile::decode(&mut reader)?;
        let world = reader.uuid()?;
        let map = reader.uuid()?;
        let ecology_tick = reader.u64()?;
        let expected_manifest = if reader.bool()? {
            Some(ContentHash::new(reader.array()?))
        } else {
            None
        };
        let surface_provider_set_hash = ContentHash::new(reader.array()?);
        let count = reader.count(69)?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(CookWorkItem::decode(&mut reader)?);
        }
        reader.complete()?;
        let manifest = Self {
            versions,
            platform,
            world,
            map,
            ecology_tick,
            expected_manifest,
            surface_provider_set_hash,
            items,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Complete content identity of the plan; claims and completions key under it.
    pub fn identity(&self) -> Result<ContentHash> {
        Ok(ContentHash::of(&self.canonical_bytes()?))
    }
}

/// One item's published own-input record: the canonical ancestor-independent dependency half a
/// claimant composes its cook key from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookWorkPayload {
    /// The cell node the payload belongs to.
    pub address: CookNodeAddress,
    /// The item's own (ancestor-independent) dependency half, canonical.
    pub own_dependencies: Vec<CookDependency>,
}

impl CookWorkPayload {
    /// Writes the canonical payload record.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.address.validate()?;
        let dependencies = canonical_dependencies(&self.own_dependencies)?;
        let mut writer = BinaryWriter::new();
        writer.bytes(COOK_WORK_PAYLOAD_MAGIC);
        self.address.encode(&mut writer);
        writer.length(dependencies.len())?;
        for (_, dependency) in dependencies {
            dependency.encode(&mut writer)?;
        }
        Ok(writer.finish())
    }

    /// Strictly reads a canonical payload record.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation cook work payload");
        reader.expect(COOK_WORK_PAYLOAD_MAGIC, "magic")?;
        let address = CookNodeAddress::decode(&mut reader)?;
        let count = reader.count(44)?;
        let mut own_dependencies = Vec::with_capacity(count);
        for _ in 0..count {
            own_dependencies.push(CookDependency::decode(&mut reader)?);
        }
        reader.complete()?;
        Ok(Self {
            address,
            own_dependencies,
        })
    }
}

/// The identity of one item's own (ancestor-independent) inputs.
///
/// Hashes under its own domain string so an own-input key can never be mistaken for a
/// [`CookNodeRecord`] cook key, which covers the composed full dependency set.
pub fn cook_work_own_input_key(
    versions: CookVersionSet,
    platform: &CookPlatformProfile,
    address: &CookNodeAddress,
    own_dependencies: &[CookDependency],
) -> Result<ContentHash> {
    versions.validate()?;
    platform.identity()?;
    address.validate()?;
    let dependencies = canonical_dependencies(own_dependencies)?;
    let mut writer = BinaryWriter::new();
    writer.bytes(b"saffron-anima/vegetation-cook-work-own-input/v1\0");
    versions.encode(&mut writer);
    platform.encode(&mut writer)?;
    address.encode(&mut writer);
    writer.length(dependencies.len())?;
    for (_, dependency) in dependencies {
        dependency.encode(&mut writer)?;
    }
    Ok(ContentHash::of(&writer.finish()))
}

/// One completed item: everything the committer needs to assemble the generation's cook graph
/// and manifest without re-reading the item's payload or artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookWorkCompletion {
    /// The completed item's index in the plan.
    pub item: u32,
    /// The published node record (composed dependencies, cook key, output hash, estimate).
    pub node: CookNodeRecord,
    /// Measured execution/cache statistics; carried apart from the node record because the
    /// node's canonical encoding excludes them.
    pub actual: CookWorkActual,
    /// The manifest cell row the committer appends.
    pub manifest_cell: VegetationManifestCell,
}

impl CookWorkCompletion {
    /// Writes the canonical completion record.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = BinaryWriter::new();
        writer.bytes(COOK_WORK_COMPLETION_MAGIC);
        writer.u32(self.item);
        self.node.encode(&mut writer)?;
        writer.u64(self.actual.elapsed_micros);
        writer.u64(self.actual.peak_memory_bytes);
        writer.u64(self.actual.input_bytes);
        writer.u64(self.actual.output_bytes);
        writer.u64(self.actual.rejection_count);
        writer.bool(self.actual.cache_hit);
        encode_cell(&mut writer, &self.manifest_cell)?;
        Ok(writer.finish())
    }

    /// Strictly reads a canonical completion record.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation cook work completion");
        reader.expect(COOK_WORK_COMPLETION_MAGIC, "magic")?;
        let item = reader.u32()?;
        let mut node = CookNodeRecord::decode(&mut reader)?;
        let actual = CookWorkActual {
            elapsed_micros: reader.u64()?,
            peak_memory_bytes: reader.u64()?,
            input_bytes: reader.u64()?,
            output_bytes: reader.u64()?,
            rejection_count: reader.u64()?,
            cache_hit: reader.bool()?,
        };
        node.actual = actual;
        let mut manifest_cell = decode_cell(&mut reader)?;
        // The cell's canonical encoding excludes measured actuals (like the node record's);
        // the completion's own copy restores them, so the committer reports faithful work.
        manifest_cell.actual = actual;
        reader.complete()?;
        Ok(Self {
            item,
            node,
            actual,
            manifest_cell,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cook::CookDependencyAddress;
    use saffron_spatial::WorldCellKey;

    fn test_platform() -> CookPlatformProfile {
        CookPlatformProfile {
            target: "x86_64-unknown-linux-gnu".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "test".to_owned(),
            features: Vec::new(),
        }
    }

    fn cell_address(index: u64) -> CookNodeAddress {
        CookNodeAddress::Cell {
            map: Uuid(7),
            cell: WorldCellKey::new(index as i64, 0, 0, 2).expect("cell key"),
        }
    }

    fn test_item(index: u64, blocked_by: Vec<u32>) -> CookWorkItem {
        CookWorkItem {
            address: cell_address(index),
            own_input_key: ContentHash::of(&index.to_be_bytes()),
            payload: ContentHash::of(b"payload"),
            blocked_by,
            estimate: CookWorkEstimate {
                work_units: 1,
                peak_memory_bytes: 2,
                input_bytes: 3,
                output_bytes: 4,
            },
        }
    }

    fn test_manifest(items: Vec<CookWorkItem>) -> CookWorkManifest {
        CookWorkManifest {
            versions: CookVersionSet::current(),
            platform: test_platform(),
            world: Uuid(1),
            map: Uuid(7),
            ecology_tick: 12,
            expected_manifest: Some(ContentHash::of(b"previous")),
            surface_provider_set_hash: ContentHash::of(b"surfaces"),
            items,
        }
    }

    #[test]
    fn manifest_round_trips_canonically() {
        let manifest = test_manifest(vec![
            test_item(0, Vec::new()),
            test_item(1, vec![0]),
            test_item(2, vec![0, 1]),
        ]);
        let bytes = manifest.canonical_bytes().expect("encode");
        let decoded = CookWorkManifest::from_canonical_bytes(&bytes).expect("decode");
        assert_eq!(decoded, manifest);
        assert_eq!(
            decoded.identity().expect("identity"),
            ContentHash::of(&bytes)
        );
    }

    #[test]
    fn a_forward_blocker_is_rejected() {
        let manifest = test_manifest(vec![test_item(0, vec![0])]);
        assert!(manifest.canonical_bytes().is_err());
        let manifest = test_manifest(vec![test_item(0, Vec::new()), test_item(1, vec![1])]);
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn a_duplicate_item_address_is_rejected() {
        let manifest = test_manifest(vec![test_item(0, Vec::new()), test_item(0, Vec::new())]);
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn own_input_key_differs_from_the_node_cook_key_on_identical_inputs() {
        let versions = CookVersionSet::current();
        let platform = test_platform();
        let dependencies = vec![CookDependency {
            address: CookDependencyAddress::Contract {
                namespace: "test".to_owned(),
            },
            content_hash: ContentHash::of(b"dep"),
            bounds: None,
            halo: saffron_spatial::DecisionScalar::from_bits(0),
            ancestor_level: None,
        }];
        let own = cook_work_own_input_key(versions, &platform, &cell_address(0), &dependencies)
            .expect("own key");
        let node = CookNodeRecord {
            address: cell_address(0),
            cook_key: ContentHash::default(),
            output_hash: ContentHash::default(),
            dependencies,
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
        };
        let cook_key = node
            .calculate_cook_key(versions, &platform)
            .expect("cook key");
        assert_ne!(own, cook_key, "the two domains must never collide");
    }

    #[test]
    fn payload_round_trips_and_matches_its_own_input_key() {
        let payload = CookWorkPayload {
            address: cell_address(3),
            own_dependencies: vec![CookDependency {
                address: CookDependencyAddress::Contract {
                    namespace: "test".to_owned(),
                },
                content_hash: ContentHash::of(b"dep"),
                bounds: None,
                halo: saffron_spatial::DecisionScalar::from_bits(0),
                ancestor_level: None,
            }],
        };
        let bytes = payload.canonical_bytes().expect("encode");
        let decoded = CookWorkPayload::from_canonical_bytes(&bytes).expect("decode");
        assert_eq!(decoded, payload);
        let direct = cook_work_own_input_key(
            CookVersionSet::current(),
            &test_platform(),
            &payload.address,
            &payload.own_dependencies,
        )
        .expect("key");
        let recomputed = cook_work_own_input_key(
            CookVersionSet::current(),
            &test_platform(),
            &decoded.address,
            &decoded.own_dependencies,
        )
        .expect("key");
        assert_eq!(direct, recomputed);
    }
}
