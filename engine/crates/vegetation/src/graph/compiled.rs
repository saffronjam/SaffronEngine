//! The compiled biome-graph IR consumed by planners and evaluators.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::DecisionScalar;

use crate::hash::sha256;
use crate::{
    BiomeAsset, BiomePaletteEntry, BiomeRole, CompanionRule, CompetitionRule, Result,
    SuccessionRule, SuitabilityBinding,
};

use super::*;

/// One immutable dependency and the exact content identity used for compilation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphDependencyFingerprint {
    pub source: GraphDependencySource,
    /// Exact canonical content hash.
    pub content_hash: [u8; 32],
}

/// Resolves biome modules and immutable source hashes without coupling the domain crate to asset I/O.
pub trait BiomeGraphResolver {
    /// Loads one module asset by stable catalog UUID.
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset>;
    /// Resolves the canonical content hash for one declared source.
    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]>;
    /// Lists canonical non-asset sources available to operators that address a complete source set.
    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        Vec::new()
    }
}

/// Stable path used by provenance and rejection diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphDebugSymbol {
    /// Module call path from the root.
    pub module_path: Vec<u128>,
    /// Node GUID local to the owning asset.
    pub node: u128,
    /// Human-readable stable label.
    pub label: String,
}

/// Stable fully qualified address of one compiled node, including nested module calls.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphNodeAddress {
    /// Module-call path from the root graph.
    pub module_path: Vec<u128>,
    /// Node GUID local to the owning graph.
    pub node: u128,
}

impl GraphNodeAddress {
    /// Canonical bytes used by stage, cache, and execution identities.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + self.module_path.len() * 16 + 16);
        bytes.extend_from_slice(&(self.module_path.len() as u64).to_be_bytes());
        for call in &self.module_path {
            bytes.extend_from_slice(&call.to_be_bytes());
        }
        bytes.extend_from_slice(&self.node.to_be_bytes());
        bytes
    }

    /// Root-biome-qualified compact execution identity used by candidate random streams.
    #[must_use]
    pub fn execution_identity(&self, biome: Uuid) -> u128 {
        let mut bytes = b"saffron-anima/vegetation-node-address/v1\0".to_vec();
        bytes.extend_from_slice(&biome.value().to_be_bytes());
        for call in &self.module_path {
            bytes.extend_from_slice(&call.to_be_bytes());
        }
        bytes.extend_from_slice(&self.node.to_be_bytes());
        u128::from_be_bytes(sha256(&bytes)[..16].try_into().unwrap())
    }
}

/// One pin on a fully qualified compiled node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedGraphPin {
    /// Fully qualified node owning the pin.
    pub node: GraphNodeAddress,
    pub pin: String,
}

impl QualifiedGraphPin {
    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = self.node.canonical_bytes();
        bytes.extend_from_slice(&(self.pin.len() as u64).to_be_bytes());
        bytes.extend_from_slice(self.pin.as_bytes());
        bytes
    }
}

/// Symbolic candidate-stream lineage carried through the compiled typed IR.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphValueLineage {
    /// One public module input whose caller supplies the concrete lineage.
    InterfaceInput(String),
    /// One candidate-generation or expansion stage in the fully qualified module path.
    CandidateOrigin {
        /// Module call path from the root.
        module_path: Vec<u128>,
        /// Node GUID local to the owning graph.
        node: u128,
    },
}

/// One validated node ready for execution.
#[derive(Clone, Debug)]
pub struct CompiledGraphNode {
    /// Authored node definition after strict current-version validation.
    pub definition: GraphNodeDefinition,
    /// Canonical definition hash reused by execution plans and cache identities.
    pub definition_hash: [u8; 32],
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
    pub parameter_schema: Vec<GraphParameterDescriptor>,
    /// Execution capabilities.
    pub capabilities: ExecutionCapabilities,
    /// Propagated output authority per pin.
    pub output_authority: BTreeMap<String, GraphAuthority>,
    /// Candidate-stream lineage carried by candidate-indexed output pins.
    pub output_lineage: BTreeMap<String, GraphValueLineage>,
    /// Conservative estimate for each output pin.
    pub output_estimates: BTreeMap<String, GraphEstimate>,
    /// Planning estimate after upstream propagation.
    pub estimate: GraphEstimate,
    /// Exact immutable dependency fingerprints read by this node.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Stable diagnostic symbol.
    pub debug_symbol: GraphDebugSymbol,
    /// Recursively compiled ordinary `.sbiome` module.
    pub module: Option<Box<CompiledGraphUnit>>,
}

impl CompiledGraphNode {
    /// Stable fully qualified address of this node.
    #[must_use]
    pub fn address(&self) -> GraphNodeAddress {
        GraphNodeAddress {
            module_path: self.debug_symbol.module_path.clone(),
            node: self.definition.guid,
        }
    }
}

/// One validated root or module graph in canonical topological order.
#[derive(Clone, Debug)]
pub struct CompiledGraphUnit {
    /// Owning biome asset.
    pub biome: Uuid,
    pub role: BiomeRole,
    pub inputs: Vec<GraphInterfaceInput>,
    pub outputs: Vec<GraphInterfaceOutput>,
    /// Propagated authority for each public output.
    pub output_authority: BTreeMap<String, GraphAuthority>,
    /// Conservative estimate for each public output.
    pub output_estimates: BTreeMap<String, GraphEstimate>,
    /// Candidate-stream lineage carried by candidate-indexed public outputs.
    pub output_lineage: BTreeMap<String, GraphValueLineage>,
    /// Nodes in canonical topological order.
    pub nodes: Vec<CompiledGraphNode>,
    /// Canonically sorted edges.
    pub edges: Vec<GraphEdge>,
    pub document_hash: [u8; 32],
    /// Direct and transitive asset dependencies with canonical hashes.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Plant palette available to species-input nodes.
    pub palette: Vec<BiomePaletteEntry>,
    /// Field suitability rules available to field/suitability nodes.
    pub suitability: Vec<SuitabilityBinding>,
    /// Pairwise spacing/priority rules.
    pub competition: Vec<CompetitionRule>,
    /// Recursive child/companion rules.
    pub companions: Vec<CompanionRule>,
    /// Read-only succession inputs.
    pub succession: Vec<SuccessionRule>,
    /// Whether missing canonical fields are a compile/evaluation error.
    pub require_authoritative_fields: bool,
    /// Maximum composed finite influence for demanded outputs.
    pub maximum_influence_radius: DecisionScalar,
    pub estimate: GraphEstimate,
    pub(super) output_halo_by_level: BTreeMap<String, [DecisionScalar; 63]>,
}

/// The single compiled IR used by preview, offline cooking, and runtime evaluation.
#[derive(Clone, Debug)]
pub struct CompiledBiomeGraph {
    pub root: CompiledGraphUnit,
    /// Exact root asset.
    pub biome: Uuid,
    pub identity: [u8; 32],
    /// Safety limits used during validation.
    pub limits: GraphSafetyLimits,
    pub(super) demand_plan: CompiledDemandPlan,
    pub(super) spatial_plan: CompiledSpatialPlan,
    pub(super) required_halo_by_level: [DecisionScalar; 63],
}

impl CompiledBiomeGraph {
    /// Maximum finite halo radius required by partitioned stages visible at `output_level`.
    #[must_use]
    pub fn required_halo(&self, output_level: u8) -> DecisionScalar {
        self.required_halo_by_level
            .get(usize::from(output_level))
            .copied()
            .unwrap_or(DecisionScalar::from_bits(0))
    }

    /// Canonically sorted direct and transitive immutable dependency fingerprints.
    #[must_use]
    pub fn dependencies(&self) -> &[GraphDependencyFingerprint] {
        &self.root.dependencies
    }

    /// Compiler-owned spatial schedule over the canonical graph IR.
    #[must_use]
    pub fn spatial_plan(&self) -> &CompiledSpatialPlan {
        &self.spatial_plan
    }

    /// Compiler-owned pin-level demand shared by planners and evaluators.
    #[must_use]
    pub(crate) const fn demand_plan(&self) -> &CompiledDemandPlan {
        &self.demand_plan
    }
}
