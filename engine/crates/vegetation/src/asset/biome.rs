use std::collections::BTreeSet;

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldBounds};

use crate::{Error, Result};

/// Current `.sbiome` document version.
pub const BIOME_ASSET_VERSION: u32 = 1;

/// Root biome or reusable typed module role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BiomeRole {
    /// Root world/community graph.
    #[default]
    Root,
    /// Reusable typed subgraph module.
    Module,
}

/// Typed biome module parameter kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiomeParameterType {
    /// Q15.16 scalar.
    Scalar,
    /// Q15.16 vector.
    Vector,
    /// Closed normalized scalar.
    Unit,
    /// Plant-family asset reference.
    Plant,
    /// Shared field-channel reference.
    Field,
    Boolean,
}

/// One declared module/root parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeParameter {
    /// Stable parameter identity.
    pub id: u128,
    /// Human-readable parameter name.
    pub name: String,
    pub parameter_type: BiomeParameterType,
    /// Canonical typed default document.
    pub default_value: Value,
}

/// One family entry in a biome palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomePaletteEntry {
    /// Plant-family asset.
    pub plant: Uuid,
    /// Base community weight.
    pub weight: UnitInterval,
    pub seed_namespace: u128,
}

/// Field-to-suitability binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuitabilityBinding {
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Suitable inclusive minimum.
    pub minimum: DecisionScalar,
    /// Suitable inclusive maximum.
    pub maximum: DecisionScalar,
    /// Edge softness.
    pub falloff: DecisionScalar,
    /// Stable rule/node identity.
    pub node_guid: u128,
}

/// Pairwise competition rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompetitionRule {
    /// First family.
    pub first: Uuid,
    /// Second family.
    pub second: Uuid,
    /// Required spacing in Q15.16 metres.
    pub spacing: DecisionScalar,
    /// Relative priority when exclusion overlaps.
    pub priority: i32,
}

/// Companion/child relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionRule {
    /// Parent/host family.
    pub parent: Uuid,
    /// Companion/child family.
    pub child: Uuid,
    pub minimum_distance: DecisionScalar,
    pub maximum_distance: DecisionScalar,
    /// Spawn probability.
    pub probability: UnitInterval,
}

/// Ecological succession rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuccessionRule {
    /// Source family/lifecycle community.
    pub from: Uuid,
    /// Successor family.
    pub to: Uuid,
    /// Earliest ecology tick.
    pub minimum_tick: u64,
    /// Canonical chance at an eligible tick.
    pub probability: UnitInterval,
}

/// One ordinary `.sbiome` module reference.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeModuleReference {
    /// Referenced `.sbiome` asset.
    pub biome: Uuid,
    /// Stable call-site node GUID.
    pub call_guid: u128,
    /// Typed parameter bindings by parameter GUID.
    pub bindings: Vec<(u128, Value)>,
}

/// Graph-level evaluator policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomeGraphPolicy {
    /// Maximum descendant module-call edges relative to this asset's entry depth.
    pub maximum_recursion: u16,
    /// Maximum finite influence radius in Q15.16 metres.
    pub maximum_influence_radius: DecisionScalar,
    /// Reject unavailable authoritative fields instead of substituting defaults.
    pub require_authoritative_fields: bool,
}

/// One `.sbiome` root/module asset.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeAsset {
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    pub name: String,
    /// Root or reusable module.
    pub role: BiomeRole,
    /// Typed public parameter interface.
    pub parameters: Vec<BiomeParameter>,
    pub palette: Vec<BiomePaletteEntry>,
    /// Base density in points per square metre, Q15.16.
    pub density: DecisionScalar,
    /// Clustering strength.
    pub clustering: UnitInterval,
    pub suitability: Vec<SuitabilityBinding>,
    /// Pairwise spacing/competition.
    pub competition: Vec<CompetitionRule>,
    pub companions: Vec<CompanionRule>,
    pub succession: Vec<SuccessionRule>,
    /// Stable seed namespaces declared by this graph.
    pub seed_namespaces: Vec<(String, u128)>,
    /// Ordinary `.sbiome` module references.
    pub modules: Vec<BiomeModuleReference>,
    /// Bounded/cycle-checked graph policy.
    pub policy: BiomeGraphPolicy,
    /// Canonical typed graph document with stable node GUIDs.
    pub graph: Value,
}

/// One local biome instance and its typed parameter bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBiomeInstance {
    /// Stable layer/instance identity.
    pub id: u128,
    /// Referenced root biome.
    pub biome: Uuid,
    /// Exact affected bounds.
    pub bounds: WorldBounds,
    pub bindings: Vec<(u128, Value)>,
    pub revision: u64,
}

/// Validates one biome asset's interface and bounded module policy.
///
/// # Errors
///
/// [`Error::FormatVersion`] on a noncurrent document, and [`Error::InvalidFormat`] naming the
/// section that failed.
pub fn validate_biome(asset: &BiomeAsset) -> Result<()> {
    if asset.version != BIOME_ASSET_VERSION {
        return Err(Error::FormatVersion {
            format: ".sbiome",
            found: asset.version,
            expected: BIOME_ASSET_VERSION,
        });
    }
    if asset.id.value() == 0
        || asset.name.is_empty()
        || asset.policy.maximum_influence_radius.bits() < 0
    {
        return Err(field("identity/policy"));
    }
    let parameter_ids: BTreeSet<_> = asset
        .parameters
        .iter()
        .map(|parameter| parameter.id)
        .collect();
    if parameter_ids.len() != asset.parameters.len()
        || parameter_ids.contains(&0)
        || asset
            .parameters
            .iter()
            .any(|parameter| parameter.name.is_empty())
    {
        return Err(field("parameters.id"));
    }
    let call_ids: BTreeSet<_> = asset
        .modules
        .iter()
        .map(|module| module.call_guid)
        .collect();
    let seed_names: BTreeSet<_> = asset
        .seed_namespaces
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let seed_ids: BTreeSet<_> = asset.seed_namespaces.iter().map(|(_, id)| *id).collect();
    let palette_ids: BTreeSet<_> = asset
        .palette
        .iter()
        .map(|entry| entry.plant.value())
        .collect();
    let competition_keys: BTreeSet<_> = asset
        .competition
        .iter()
        .map(|rule| (rule.first.value(), rule.second.value()))
        .collect();
    let companion_keys: BTreeSet<_> = asset
        .companions
        .iter()
        .map(|rule| (rule.parent.value(), rule.child.value()))
        .collect();
    let succession_keys: BTreeSet<_> = asset
        .succession
        .iter()
        .map(|rule| (rule.from.value(), rule.to.value(), rule.minimum_tick))
        .collect();
    let suitability_ids: BTreeSet<_> = asset
        .suitability
        .iter()
        .map(|binding| binding.node_guid)
        .collect();
    if asset.density.bits() < 0
        || call_ids.len() != asset.modules.len()
        || asset.modules.iter().any(|module| module.biome == asset.id)
        || seed_names.len() != asset.seed_namespaces.len()
        || seed_ids.len() != asset.seed_namespaces.len()
        || seed_names.contains("")
        || seed_ids.contains(&0)
        || palette_ids.len() != asset.palette.len()
        || asset.palette.iter().any(|entry| {
            entry.plant.value() == 0
                || entry.seed_namespace == 0
                || !seed_ids.contains(&entry.seed_namespace)
        })
        || competition_keys.len() != asset.competition.len()
        || asset.competition.iter().any(|rule| {
            rule.first.value() > rule.second.value()
                || !palette_ids.contains(&rule.first.value())
                || !palette_ids.contains(&rule.second.value())
                || rule.spacing.bits() < 0
        })
        || companion_keys.len() != asset.companions.len()
        || asset.companions.iter().any(|rule| {
            !palette_ids.contains(&rule.parent.value())
                || !palette_ids.contains(&rule.child.value())
                || rule.minimum_distance.bits() < 0
                || rule.maximum_distance < rule.minimum_distance
        })
        || succession_keys.len() != asset.succession.len()
        || asset.succession.iter().any(|rule| {
            !palette_ids.contains(&rule.from.value()) || !palette_ids.contains(&rule.to.value())
        })
        || suitability_ids.len() != asset.suitability.len()
        || suitability_ids.contains(&0)
        || asset
            .suitability
            .iter()
            .any(|binding| binding.minimum > binding.maximum || binding.falloff.bits() < 0)
    {
        return Err(field("graph/palette/seedNamespaces"));
    }
    Ok(())
}

fn field(name: &str) -> Error {
    Error::InvalidFormat {
        format: ".sbiome",
        field: name.to_owned(),
    }
}
