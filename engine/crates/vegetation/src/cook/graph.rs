//! The canonical dependency DAG for one complete vegetation-world generation.

use std::collections::{BTreeMap, BTreeSet};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{Error, Result};

use super::{
    ContentHash, CookDependency, CookDependencyAddress, CookNodeAddress, CookPlatformProfile,
    CookVersionSet, CookWorkActual, CookWorkEstimate, canonical_dependencies,
};

const COOK_GRAPH_MAGIC: &[u8; 8] = b"SVCGPH04";

/// One immutable output and every exact input that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookNodeRecord {
    /// Stable logical output address.
    pub address: CookNodeAddress,
    /// Hash of canonical address, versions, platform, and dependencies before execution.
    pub cook_key: ContentHash,
    /// Exact validated output artifact identity.
    pub output_hash: ContentHash,
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

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
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

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
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
    pub versions: CookVersionSet,
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
