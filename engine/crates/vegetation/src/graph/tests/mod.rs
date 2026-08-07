//! Graph unit tests and the fixtures they share.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::hash::sha256;
use crate::{
    BIOME_ASSET_VERSION, BiomeAsset, BiomeGraphPolicy, BiomeModuleReference, BiomeParameter,
    BiomeParameterType, BiomeRole, Error, Result,
};

use super::*;

mod documents;
mod identity;
mod modules;
mod planning;
mod validation;

struct NoModules;

impl BiomeGraphResolver for NoModules {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        Err(graph_document(
            "resolver",
            &format!("unexpected module {}", id.value()),
        ))
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        Err(graph_document(
            "resolver",
            &format!("unexpected dependency {source:?}"),
        ))
    }
}

struct HashedDependency(u8);

impl BiomeGraphResolver for HashedDependency {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        Err(graph_document(
            "resolver",
            &format!("unexpected module {}", id.value()),
        ))
    }

    fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
        Ok([self.0; 32])
    }
}

struct ModuleResolver {
    module: BiomeAsset,
}

impl BiomeGraphResolver for ModuleResolver {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        if id == self.module.id {
            Ok(self.module.clone())
        } else {
            Err(graph_document("resolver", "unknown module"))
        }
    }

    fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
        Ok([9; 32])
    }
}

struct SelectiveModuleResolver {
    module: BiomeAsset,
    source: GraphDependencySource,
    content_hash: [u8; 32],
}

impl BiomeGraphResolver for SelectiveModuleResolver {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        if id == self.module.id {
            Ok(self.module.clone())
        } else {
            Err(graph_document("resolver", "unknown module"))
        }
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        Ok(if source == self.source {
            self.content_hash
        } else {
            [9; 32]
        })
    }
}

struct ModuleSetResolver {
    modules: Vec<BiomeAsset>,
}

impl BiomeGraphResolver for ModuleSetResolver {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        self.modules
            .iter()
            .find(|module| module.id == id)
            .cloned()
            .ok_or_else(|| graph_document("resolver", "unknown module"))
    }

    fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
        Ok([9; 32])
    }
}

fn node(guid: u128, operator: GraphOperator) -> GraphNodeDefinition {
    GraphNodeDefinition {
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
    }
}

fn biome(document: BiomeGraphDocument) -> BiomeAsset {
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: Uuid(7),
        name: "Test".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: Vec::new(),
        density: DecisionScalar::from_bits(0),
        clustering: UnitInterval::ZERO,
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces: vec![("placement".to_owned(), 11)],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: DecisionScalar::from_bits(65_536),
            require_authoritative_fields: true,
        },
        graph: document.to_json(),
    }
}

fn simple_document() -> BiomeGraphDocument {
    let region = node(1, GraphOperator::RegionInput);
    let mut scatter = node(2, GraphOperator::StratifiedCoverage);
    scatter.seed_namespaces.insert("sampling".to_owned(), 11);
    scatter
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(16));
    let species = node(3, GraphOperator::SpeciesInput);
    let mut output = node(4, GraphOperator::MacroOutput);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 11);
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, scatter, region],
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

fn module_call_node(guid: u128, call_guid: u128) -> GraphNodeDefinition {
    let mut call = node(guid, GraphOperator::ModuleCall);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(call_guid));
    call
}

fn module_asset(id: u64, child: Option<(u64, u128)>, maximum_recursion: u16) -> BiomeAsset {
    let mut document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: Vec::new(),
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let module_reference = child.map(|(child_id, call_guid)| {
        document.nodes.push(module_call_node(call_guid, call_guid));
        BiomeModuleReference {
            biome: Uuid(child_id),
            call_guid,
            bindings: Vec::new(),
        }
    });
    let mut asset = biome(document);
    asset.id = Uuid(id);
    asset.role = BiomeRole::Module;
    asset.name = format!("Module {id}");
    asset.policy.maximum_recursion = maximum_recursion;
    if let Some(module_reference) = module_reference {
        asset.modules.push(module_reference);
    }
    asset
}

fn root_with_module(module: u64, call_guid: u128) -> BiomeAsset {
    let mut document = simple_document();
    document.nodes.push(module_call_node(call_guid, call_guid));
    let mut root = biome(document);
    root.modules.push(BiomeModuleReference {
        biome: Uuid(module),
        call_guid,
        bindings: Vec::new(),
    });
    root
}
