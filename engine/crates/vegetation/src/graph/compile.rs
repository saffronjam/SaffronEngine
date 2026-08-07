//! Root and module graph compilation into the single canonical IR.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_spatial::{FieldChannel, FieldDerivative};

use crate::hash::sha256;
use crate::{BiomeAsset, BiomeRole, Error, Result, validate_biome};

use super::*;

/// Compiler inputs that do not alter graph semantics.
pub struct GraphCompileOptions {
    /// Hard estimates and evaluator caps.
    pub limits: GraphSafetyLimits,
}

impl GraphCompileOptions {
    /// Canonical compilation with the default hard limits.
    #[must_use]
    pub fn canonical() -> Self {
        Self {
            limits: GraphSafetyLimits::default(),
        }
    }
}

/// Compiles one root biome and its ordinary `.sbiome` modules into the canonical IR.
pub fn compile_biome_graph(
    root: &BiomeAsset,
    root_bindings: &[(u128, Value)],
    resolver: &dyn BiomeGraphResolver,
    options: GraphCompileOptions,
) -> Result<CompiledBiomeGraph> {
    validate_biome(root)?;
    if root.role != BiomeRole::Root {
        return Err(graph_document(
            "biome.role",
            "root compilation requires a root biome",
        ));
    }
    let mut stack = Vec::new();
    let mut root_unit = compile_unit(
        root,
        root_bindings,
        resolver,
        &mut stack,
        &[],
        usize::from(options.limits.max_module_depth),
    )?;
    let root_seeds = root_unit
        .outputs
        .iter()
        .map(|output| {
            let source = root_unit
                .nodes
                .iter()
                .find(|node| node.definition.guid == output.node)
                .ok_or_else(|| graph_document("graph.outputs", "output source node is missing"))?;
            Ok(QualifiedGraphPin {
                node: source.address(),
                pin: output.pin.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut execution_demand =
        compile_demand_slice(&root_unit, root_seeds.iter().cloned(), &BTreeSet::new())?;
    execution_demand
        .units
        .entry(Vec::new())
        .or_default()
        .outputs
        .extend(root_unit.outputs.iter().map(|output| output.name.clone()));
    apply_demanded_estimates(&mut root_unit, &[], &execution_demand)?;
    root_unit.dependencies = execution_demand.dependencies.clone();
    let live_estimate = demanded_estimate(&execution_demand);
    enforce_limits(live_estimate, options.limits)?;
    enforce_demanded_halo_policies(&root_unit, &execution_demand)?;
    let required_halo_by_level = demanded_output_halo(&root_unit, &execution_demand, &root_seeds)?;
    let mut identity_bytes = b"saffron-anima/compiled-biome-execution/v2\0".to_vec();
    identity_bytes.extend_from_slice(&BIOME_GRAPH_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&BIOME_INTERFACE_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&BIOME_NODE_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&root.id.value().to_be_bytes());
    identity_bytes.extend_from_slice(&demand_unit_semantic_hash(
        &root_unit,
        &execution_demand,
        &[],
    )?);
    append_identity_collection(
        &mut identity_bytes,
        "dependencies",
        execution_demand.dependencies.len(),
    );
    for dependency in &execution_demand.dependencies {
        append_dependency_source(&mut identity_bytes, dependency.source);
        identity_bytes.extend_from_slice(&dependency.content_hash);
    }
    let identity = sha256(&identity_bytes);
    let spatial_plan = compile_spatial_plan(&root_unit, &execution_demand)?;
    let global_outputs = spatial_plan
        .global_stages()
        .iter()
        .flat_map(|stage| stage.output_pins.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut public_demand = compile_demand_slice(&root_unit, root_seeds, &global_outputs)?;
    public_demand
        .units
        .entry(Vec::new())
        .or_default()
        .outputs
        .extend(root_unit.outputs.iter().map(|output| output.name.clone()));
    let stages = spatial_plan
        .global_stages()
        .iter()
        .map(|stage| {
            Ok((
                stage.id,
                compile_demand_slice(
                    &root_unit,
                    stage.output_pins.iter().cloned(),
                    &stage.input_pins.iter().cloned().collect(),
                )?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(CompiledBiomeGraph {
        root: root_unit,
        biome: root.id,
        identity,
        limits: options.limits,
        demand_plan: CompiledDemandPlan {
            execution: execution_demand,
            public: public_demand,
            stages,
        },
        spatial_plan,
        required_halo_by_level,
    })
}

fn compile_unit(
    asset: &BiomeAsset,
    bindings: &[(u128, Value)],
    resolver: &dyn BiomeGraphResolver,
    stack: &mut Vec<Uuid>,
    module_path: &[u128],
    inherited_depth_limit: usize,
) -> Result<CompiledGraphUnit> {
    if stack.contains(&asset.id) {
        return Err(Error::GraphCycle {
            node: u128::from(asset.id.value()),
        });
    }
    let module_depth = module_path.len();
    if module_depth > inherited_depth_limit {
        return Err(Error::GraphLimit {
            resource: "module recursion",
            requested: u64::try_from(module_depth).map_err(|_| Error::NumericOverflow)?,
            limit: u64::try_from(inherited_depth_limit).map_err(|_| Error::NumericOverflow)?,
        });
    }
    let descendant_depth_limit = inherited_depth_limit.min(
        module_depth
            .checked_add(usize::from(asset.policy.maximum_recursion))
            .ok_or(Error::NumericOverflow)?,
    );
    stack.push(asset.id);
    validate_biome(asset)?;
    let mut document = BiomeGraphDocument::from_json(&asset.graph)?;
    resolve_parameter_bindings(&mut document, asset, bindings)?;
    validate_interface(&document, asset.role)?;
    let node_map: BTreeMap<_, _> = document
        .nodes
        .iter()
        .map(|node| (node.guid, node))
        .collect();
    if node_map.len() != document.nodes.len() || node_map.contains_key(&0) {
        return Err(graph_document(
            "graph.nodes.guid",
            "node GUIDs must be unique and non-zero",
        ));
    }
    for binding in &asset.suitability {
        if node_map
            .get(&binding.node_guid)
            .is_none_or(|node| node.operator != GraphOperator::Suitability)
        {
            return Err(graph_document(
                "biome.suitability",
                "every suitability binding must target one suitability node",
            ));
        }
    }
    if document.nodes.iter().any(|node| {
        node.operator == GraphOperator::Suitability
            && !asset
                .suitability
                .iter()
                .any(|binding| binding.node_guid == node.guid)
    }) {
        return Err(graph_document(
            "biome.suitability",
            "every suitability node requires exactly one suitability binding",
        ));
    }
    let mut module_by_call = BTreeMap::new();
    for module in &asset.modules {
        if module_by_call.insert(module.call_guid, module).is_some() || module.call_guid == 0 {
            return Err(graph_document(
                "biome.modules.callGuid",
                "module call GUIDs must be unique and non-zero",
            ));
        }
    }
    let mut module_units = BTreeMap::new();
    let mut used_module_calls = BTreeSet::new();
    let declared_seed_namespaces: BTreeSet<_> =
        asset.seed_namespaces.iter().map(|(_, id)| *id).collect();
    for node in &document.nodes {
        validate_node_definition(node)?;
        validate_spatial_contract(node, asset)?;
        if node
            .seed_namespaces
            .values()
            .any(|namespace| !declared_seed_namespaces.contains(namespace))
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
                "node uses a seed namespace not declared by its biome",
            ));
        }
        if node.operator == GraphOperator::ModuleCall {
            let call_guid = required_guid(node, "callGuid")?;
            if !used_module_calls.insert(call_guid) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.callGuid", node.guid),
                    "module call GUID is already used by another node",
                ));
            }
            let module_ref = module_by_call.get(&call_guid).ok_or_else(|| {
                graph_document(
                    &format!("graph.nodes.{:032x}.callGuid", node.guid),
                    "no matching biome module reference",
                )
            })?;
            let module_asset = resolver.resolve_biome(module_ref.biome)?;
            if module_asset.role != BiomeRole::Module {
                return Err(graph_document(
                    "biome.modules",
                    "module references must target module-role biome assets",
                ));
            }
            validate_parameter_bindings(&module_asset, &module_ref.bindings)?;
            let mut child_path = module_path.to_vec();
            child_path.push(call_guid);
            let unit = compile_unit(
                &module_asset,
                &module_ref.bindings,
                resolver,
                stack,
                &child_path,
                descendant_depth_limit,
            )?;
            module_units.insert(node.guid, Box::new(unit));
        }
    }
    if used_module_calls.len() != module_by_call.len() {
        return Err(graph_document(
            "biome.modules",
            "every module reference requires exactly one matching module-call node",
        ));
    }
    let signatures = node_signatures(&document.nodes, &document.inputs, &module_units)?;
    validate_edges(&document, &signatures)?;
    let order = topological_order(&document.nodes, &document.edges)?;
    let incoming = incoming_edges(&document.edges);
    let mut authority_by_pin: BTreeMap<(u128, String), GraphAuthority> = BTreeMap::new();
    let mut estimate_by_pin: BTreeMap<(u128, String), GraphEstimate> = BTreeMap::new();
    let mut lineage_by_pin: BTreeMap<(u128, String), GraphValueLineage> = BTreeMap::new();
    let mut compiled_nodes = Vec::with_capacity(order.len());
    for guid in order {
        let definition = node_map[&guid].clone();
        let (inputs, outputs) = signatures[&guid].clone();
        let mut authority = definition.authority;
        let mut upstream_estimate = GraphEstimate::default();
        let mut input_lineage = BTreeMap::new();
        for edge in incoming.get(&guid).into_iter().flatten() {
            let key = (edge.from_node, edge.from_pin.clone());
            authority = authority.join(authority_by_pin[&key]);
            upstream_estimate = merge_input_estimate(upstream_estimate, estimate_by_pin[&key])?;
            if let Some(lineage) = lineage_by_pin.get(&key) {
                input_lineage.insert(edge.to_pin.clone(), lineage.clone());
            }
        }
        if definition.authority == GraphAuthority::EquivalentGpu
            && !definition.operator.has_slang_executor()
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.authority", definition.guid),
                "equivalent-gpu authority requires a complete Slang implementation",
            ));
        }
        if definition.operator == GraphOperator::Transform
            && input_lineage.contains_key("offset")
            && definition.spatial.influence_radius().bits() <= 0
        {
            return Err(Error::GraphUnboundedInfluence {
                node: definition.guid,
            });
        }
        let base_estimate = estimate_node(&definition, upstream_estimate)?;
        let compiled_module = module_units.get(&guid).map(Box::as_ref);
        let dependencies =
            compile_node_dependencies(&definition, asset, compiled_module, resolver)?;
        let output_lineage = resolve_node_lineage(
            &definition,
            &outputs,
            &input_lineage,
            compiled_module,
            module_path,
        )?;
        let mut output_authority = BTreeMap::new();
        let mut output_estimates = BTreeMap::new();
        for output in &outputs {
            let output_authority_value = compiled_module
                .and_then(|module| module.output_authority.get(&output.name))
                .copied()
                .map_or(authority, |module_authority| {
                    authority.join(module_authority)
                });
            let output_estimate = compiled_module
                .and_then(|module| module.output_estimates.get(&output.name))
                .copied()
                .map_or(base_estimate, |module_estimate| {
                    max_estimate(base_estimate, module_estimate)
                });
            authority_by_pin.insert((guid, output.name.clone()), output_authority_value);
            estimate_by_pin.insert((guid, output.name.clone()), output_estimate);
            output_authority.insert(output.name.clone(), output_authority_value);
            output_estimates.insert(output.name.clone(), output_estimate);
            if let Some(lineage) = output_lineage.get(&output.name) {
                lineage_by_pin.insert((guid, output.name.clone()), lineage.clone());
            }
        }
        let estimate = output_estimates
            .values()
            .copied()
            .fold(base_estimate, max_estimate);
        let definition_hash = sha256(&definition.canonical_bytes());
        compiled_nodes.push(CompiledGraphNode {
            definition_hash,
            inputs,
            outputs,
            parameter_schema: definition.operator.parameter_schema(),
            capabilities: ExecutionCapabilities {
                reference_cpu: true,
                parallel_cpu: true,
                slang_compute: definition.operator.has_slang_executor(),
            },
            output_authority,
            output_lineage,
            output_estimates,
            estimate,
            dependencies,
            debug_symbol: GraphDebugSymbol {
                module_path: module_path.to_vec(),
                node: guid,
                label: format!("{}:{guid:032x}", definition.operator.as_wire()),
            },
            module: module_units.remove(&guid),
            definition,
        });
    }
    for output in &document.outputs {
        if let Some(sink) = output.sink
            && output.domain != sink.expected_domain()
        {
            return Err(graph_document(
                &format!("graph.outputs.{}.domain", output.name),
                &format!(
                    "{} sink requires {} domain",
                    sink.as_wire(),
                    sink.expected_domain().as_wire()
                ),
            ));
        }
        let source = authority_by_pin
            .get(&(output.node, output.pin.clone()))
            .copied()
            .ok_or_else(|| graph_document("graph.outputs", "output source pin does not exist"))?;
        if output.sink.is_some_and(GraphSink::requires_authority) && !source.can_feed_authority() {
            return Err(Error::GraphAuthority {
                node: output.node,
                reason: format!(
                    "cosmetic value reaches {} output '{}'",
                    output.sink.unwrap().as_wire(),
                    output.name
                ),
            });
        }
    }
    let output_authority = document
        .outputs
        .iter()
        .map(|output| {
            Ok((
                output.name.clone(),
                *authority_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .ok_or_else(|| {
                        graph_document("graph.outputs", "output authority is missing")
                    })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let output_estimates = document
        .outputs
        .iter()
        .map(|output| {
            Ok((
                output.name.clone(),
                *estimate_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .ok_or_else(|| graph_document("graph.outputs", "output estimate is missing"))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let output_lineage = document
        .outputs
        .iter()
        .filter(|output| domain_has_candidate_lineage(output.domain))
        .map(|output| {
            Ok((
                output.name.clone(),
                lineage_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .cloned()
                    .ok_or_else(|| {
                        graph_document("graph.outputs", "candidate lineage is missing")
                    })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut edges = document.edges.clone();
    edges.sort_by(|left, right| {
        (left.to_node, &left.to_pin, left.from_node, &left.from_pin).cmp(&(
            right.to_node,
            &right.to_pin,
            right.from_node,
            &right.from_pin,
        ))
    });
    let mut dependencies = BTreeMap::new();
    for palette in &asset.palette {
        let source = GraphDependencySource::Asset(palette.plant);
        dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
    }
    for binding in &asset.suitability {
        let source = GraphDependencySource::Field(binding.channel);
        dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
    }
    for node in &compiled_nodes {
        for dependency in inferred_node_dependencies(&node.definition, resolver)? {
            dependencies.insert(dependency, resolver.resolve_dependency_hash(dependency)?);
        }
        for dependency in &node.definition.dependencies {
            dependencies.insert(*dependency, resolver.resolve_dependency_hash(*dependency)?);
        }
        if let Some(module) = &node.module {
            let source = GraphDependencySource::Asset(module.biome);
            dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
            dependencies.extend(
                module
                    .dependencies
                    .iter()
                    .map(|dependency| (dependency.source, dependency.content_hash)),
            );
        }
    }
    let estimate = compiled_nodes
        .iter()
        .fold(GraphEstimate::default(), |total, node| {
            max_estimate(total, node.estimate)
        });
    let output_halo_by_level = compile_output_halo(&compiled_nodes, &edges, &document.outputs)?;
    let mut semantic_identity = b"saffron-anima/compiled-biome-unit/v1\0".to_vec();
    semantic_identity.extend_from_slice(&sha256(&crate::write_biome_asset(asset)?));
    semantic_identity.extend_from_slice(&document.identity());
    let document_hash = sha256(&semantic_identity);
    let result = CompiledGraphUnit {
        biome: asset.id,
        role: asset.role,
        inputs: document.inputs,
        outputs: document.outputs,
        output_authority,
        output_estimates,
        output_lineage,
        nodes: compiled_nodes,
        edges,
        document_hash,
        dependencies: dependencies
            .into_iter()
            .map(|(source, content_hash)| GraphDependencyFingerprint {
                source,
                content_hash,
            })
            .collect(),
        palette: asset.palette.clone(),
        suitability: asset.suitability.clone(),
        competition: asset.competition.clone(),
        companions: asset.companions.clone(),
        succession: asset.succession.clone(),
        require_authoritative_fields: asset.policy.require_authoritative_fields,
        maximum_influence_radius: asset.policy.maximum_influence_radius,
        estimate,
        output_halo_by_level,
    };
    stack.pop();
    Ok(result)
}

fn compile_node_dependencies(
    node: &GraphNodeDefinition,
    asset: &BiomeAsset,
    module: Option<&CompiledGraphUnit>,
    resolver: &dyn BiomeGraphResolver,
) -> Result<Vec<GraphDependencyFingerprint>> {
    let mut sources = inferred_node_dependencies(node, resolver)?;
    sources.extend(node.dependencies.iter().copied());
    if matches!(
        node.operator,
        GraphOperator::SpeciesInput
            | GraphOperator::Competition
            | GraphOperator::BoundsOverlap
            | GraphOperator::CommunityBlend
            | GraphOperator::MacroOutput
    ) || (node.operator == GraphOperator::VariableSpacing
        && matches!(
            node.parameter("prototypeAware"),
            Some(GraphParameterValue::Boolean(true))
        ))
    {
        sources.extend(
            asset
                .palette
                .iter()
                .map(|entry| GraphDependencySource::Asset(entry.plant)),
        );
    }
    if node.operator == GraphOperator::Suitability {
        sources.extend(
            asset
                .suitability
                .iter()
                .filter(|binding| binding.node_guid == node.guid)
                .map(|binding| GraphDependencySource::Field(binding.channel)),
        );
    }
    let mut dependencies = sources
        .into_iter()
        .map(|source| {
            Ok(GraphDependencyFingerprint {
                source,
                content_hash: resolver.resolve_dependency_hash(source)?,
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if let Some(module) = module {
        let source = GraphDependencySource::Asset(module.biome);
        dependencies.insert(GraphDependencyFingerprint {
            source,
            content_hash: resolver.resolve_dependency_hash(source)?,
        });
        dependencies.extend(module.dependencies.iter().copied());
    }
    Ok(dependencies.into_iter().collect())
}

fn inferred_node_dependencies(
    node: &GraphNodeDefinition,
    resolver: &dyn BiomeGraphResolver,
) -> Result<BTreeSet<GraphDependencySource>> {
    let mut dependencies = BTreeSet::new();
    match node.operator {
        GraphOperator::ExplicitAnchors | GraphOperator::PaintedTile => {
            dependencies.insert(GraphDependencySource::MapLayer(required_guid(
                node, "layer",
            )?));
        }
        GraphOperator::FieldSample => {
            dependencies.insert(GraphDependencySource::Field(
                match node.parameter("channel") {
                    Some(GraphParameterValue::FieldChannel(channel)) => *channel,
                    _ => {
                        return Err(graph_document(
                            &format!("graph.nodes.{:032x}.parameters.channel", node.guid),
                            "field sample requires a typed channel",
                        ));
                    }
                },
            ));
            dependencies.extend(
                resolver
                    .available_dependencies()
                    .into_iter()
                    .filter(|source| matches!(source, GraphDependencySource::SurfaceProvider(_))),
            );
        }
        GraphOperator::SurfaceProjection => {
            let provider = match node.parameter("provider") {
                Some(GraphParameterValue::U64(provider)) => *provider,
                None => 0,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.provider", node.guid),
                        "surface provider filter must be an unsigned identity",
                    ));
                }
            };
            if provider == 0 {
                dependencies.extend(
                    resolver
                        .available_dependencies()
                        .into_iter()
                        .filter(|source| {
                            matches!(source, GraphDependencySource::SurfaceProvider(_))
                        }),
                );
            } else {
                dependencies.insert(GraphDependencySource::SurfaceProvider(provider));
            }
        }
        GraphOperator::DistanceField => {
            let distance_source = match node.parameter("source") {
                Some(GraphParameterValue::DistanceSource(source)) => *source,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.source", node.guid),
                        "distance field requires a typed source",
                    ));
                }
            };
            match distance_source {
                GraphDistanceSource::Water => {
                    dependencies.insert(GraphDependencySource::Field(FieldChannel::WaterDistance));
                }
                GraphDistanceSource::Blocker => {
                    dependencies.insert(GraphDependencySource::Field(FieldChannel::SignedBlocker));
                }
                GraphDistanceSource::Spline | GraphDistanceSource::Shape => {}
            }
            let source = match node.parameter("sourceGuid") {
                Some(GraphParameterValue::Guid(source)) => *source,
                None => 0,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.sourceGuid", node.guid),
                        "distance source identity must be a GUID",
                    ));
                }
            };
            if source == 0 {
                dependencies.extend(
                    resolver
                        .available_dependencies()
                        .into_iter()
                        .filter(|source| matches!(source, GraphDependencySource::MapLayer(_))),
                );
            } else {
                dependencies.insert(GraphDependencySource::MapLayer(source));
            }
        }
        GraphOperator::SplineFollow => {
            dependencies.extend(
                resolver
                    .available_dependencies()
                    .into_iter()
                    .filter(|source| matches!(source, GraphDependencySource::MapLayer(_))),
            );
        }
        _ => {}
    }
    Ok(dependencies)
}

fn domain_has_candidate_lineage(domain: GraphDomain) -> bool {
    matches!(
        domain,
        GraphDomain::Candidates
            | GraphDomain::ScalarField
            | GraphDomain::VectorField
            | GraphDomain::HessianField
            | GraphDomain::SurfaceField
    )
}

fn resolve_node_lineage(
    node: &GraphNodeDefinition,
    outputs: &[GraphPin],
    inputs: &BTreeMap<String, GraphValueLineage>,
    module: Option<&CompiledGraphUnit>,
    module_path: &[u128],
) -> Result<BTreeMap<String, GraphValueLineage>> {
    use GraphOperator as O;

    let require = |name: &str| {
        inputs.get(name).cloned().ok_or_else(|| {
            graph_document(
                &format!("graph.nodes.{:032x}.{name}", node.guid),
                "candidate lineage is missing",
            )
        })
    };
    let require_same = |left_name: &str, right_name: &str| {
        let left = require(left_name)?;
        let right = require(right_name)?;
        if left != right {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.{right_name}", node.guid),
                "input belongs to a different candidate stream",
            ));
        }
        Ok(left)
    };
    let validate_optional = |base: &GraphValueLineage, name: &str| {
        if let Some(value) = inputs.get(name)
            && value != base
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.{name}", node.guid),
                "input belongs to a different candidate stream",
            ));
        }
        Ok(())
    };

    let lineage = match node.operator {
        O::InterfaceInput => Some(GraphValueLineage::InterfaceInput(
            match node.parameter("name") {
                Some(GraphParameterValue::String(name)) => name.clone(),
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.name", node.guid),
                        "interface input requires a name",
                    ));
                }
            },
        )),
        O::ExplicitAnchors
        | O::StratifiedCoverage
        | O::BlueNoisePoisson
        | O::ClusterPatchColony
        | O::SplineFollow
        | O::RecursiveCompanion => Some(GraphValueLineage::CandidateOrigin {
            module_path: module_path.to_vec(),
            node: node.guid,
        }),
        O::SurfaceProjection => Some(require("candidates")?),
        O::FieldSample | O::PaintedTile | O::Noise | O::Gradient | O::DistanceField => {
            Some(require("candidates")?)
        }
        O::Curve | O::Remap | O::Clamp => Some(require("field")?),
        O::Combine => Some(require_same("left", "right")?),
        O::WeightedElimination | O::FieldImportance | O::Suitability => {
            Some(require_same("candidates", "weights")?)
        }
        O::PriorityExclusion => {
            let base = require_same("candidates", "weights")?;
            let radius = require("radius")?;
            if base != radius {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.radius", node.guid),
                    "input belongs to a different candidate stream",
                ));
            }
            Some(base)
        }
        O::VariableSpacing => Some(require_same("candidates", "radius")?),
        O::Competition => Some(require("candidates")?),
        O::Transform => {
            let base = require("candidates")?;
            validate_optional(&base, "surface")?;
            validate_optional(&base, "scale")?;
            validate_optional(&base, "offset")?;
            Some(base)
        }
        O::CommunityBlend => {
            let base = require("candidates")?;
            validate_optional(&base, "shade")?;
            Some(base)
        }
        O::BoundsOverlap | O::SuccessionInput => Some(require("candidates")?),
        O::MicroOutput => {
            let base = require("candidates")?;
            validate_optional(&base, "density")?;
            for (name, lineage) in inputs {
                if name.starts_with("attribute-") && lineage != &base {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.{name}", node.guid),
                        "input belongs to a different candidate stream",
                    ));
                }
            }
            None
        }
        O::DiagnosticOutput => {
            if let (Some(candidates), Some(field)) = (inputs.get("candidates"), inputs.get("field"))
                && candidates != field
            {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.field", node.guid),
                    "input belongs to a different candidate stream",
                ));
            }
            None
        }
        O::ModuleCall => {
            let module = module.ok_or_else(|| {
                graph_document("graph.nodes.module-call", "compiled module is missing")
            })?;
            let mut result = BTreeMap::new();
            for output in outputs {
                if !domain_has_candidate_lineage(output.domain) {
                    continue;
                }
                let lineage = module.output_lineage.get(&output.name).ok_or_else(|| {
                    graph_document(
                        &format!("graph.nodes.{:032x}.{}", node.guid, output.name),
                        "module output candidate lineage is missing",
                    )
                })?;
                let lineage = match lineage {
                    GraphValueLineage::InterfaceInput(name) => {
                        inputs.get(name).cloned().ok_or_else(|| {
                            graph_document(
                                &format!("graph.nodes.{:032x}.{name}", node.guid),
                                "module input candidate lineage is missing",
                            )
                        })?
                    }
                    GraphValueLineage::CandidateOrigin { .. } => lineage.clone(),
                };
                result.insert(output.name.clone(), lineage);
            }
            return Ok(result);
        }
        O::RegionInput | O::SplineInput | O::SpeciesInput | O::CommunityInput | O::MacroOutput => {
            None
        }
    };

    outputs
        .iter()
        .filter(|output| domain_has_candidate_lineage(output.domain))
        .map(|output| {
            Ok((
                output.name.clone(),
                lineage.clone().ok_or_else(|| {
                    graph_document(
                        &format!("graph.nodes.{:032x}.{}", node.guid, output.name),
                        "operator output candidate lineage is missing",
                    )
                })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()
}

type NodeSignature = (Vec<GraphPin>, Vec<GraphPin>);
type NodeSignatures = BTreeMap<u128, NodeSignature>;

fn node_signatures(
    nodes: &[GraphNodeDefinition],
    interface_inputs: &[GraphInterfaceInput],
    modules: &BTreeMap<u128, Box<CompiledGraphUnit>>,
) -> Result<NodeSignatures> {
    let mut result = BTreeMap::new();
    for node in nodes {
        let signature = if node.operator == GraphOperator::ModuleCall {
            let module = modules.get(&node.guid).ok_or_else(|| {
                graph_document("graph.nodes.module-call", "compiled module is missing")
            })?;
            (
                module
                    .inputs
                    .iter()
                    .map(|input| pin(&input.name, input.domain))
                    .collect(),
                module
                    .outputs
                    .iter()
                    .map(|output| pin(&output.name, output.domain))
                    .collect(),
            )
        } else if node.operator == GraphOperator::InterfaceInput {
            let name = match node.parameter("name") {
                Some(GraphParameterValue::String(value)) => value,
                _ => {
                    return Err(graph_document(
                        "graph.nodes.interface-input.name",
                        "interface input requires a name",
                    ));
                }
            };
            let input = interface_inputs
                .iter()
                .find(|input| &input.name == name)
                .ok_or_else(|| {
                    graph_document(
                        "graph.nodes.interface-input.name",
                        "unknown public interface input",
                    )
                })?;
            (Vec::new(), vec![pin("value", input.domain)])
        } else if node.operator == GraphOperator::FieldSample {
            let domain = match node.parameter("derivative") {
                None | Some(GraphParameterValue::FieldDerivative(FieldDerivative::Value)) => {
                    GraphDomain::ScalarField
                }
                Some(GraphParameterValue::FieldDerivative(FieldDerivative::Gradient)) => {
                    GraphDomain::VectorField
                }
                Some(GraphParameterValue::FieldDerivative(FieldDerivative::Hessian)) => {
                    GraphDomain::HessianField
                }
                Some(_) => {
                    return Err(graph_document(
                        "graph.nodes.field-sample.derivative",
                        "field derivative has the wrong type",
                    ));
                }
            };
            (node.operator.input_pins(), vec![pin("field", domain)])
        } else if node.operator == GraphOperator::MicroOutput {
            let mut inputs = node.operator.input_pins();
            let channels = match node.parameter("attributeChannels") {
                None => Vec::new(),
                Some(GraphParameterValue::GuidList(channels)) => channels.clone(),
                Some(_) => {
                    return Err(graph_document(
                        "graph.nodes.micro-output.attributeChannels",
                        "attribute channels have the wrong type",
                    ));
                }
            };
            inputs.extend(
                channels
                    .into_iter()
                    .map(|channel| pin(micro_attribute_pin(channel), GraphDomain::ScalarField)),
            );
            (inputs, node.operator.output_pins())
        } else {
            (node.operator.input_pins(), node.operator.output_pins())
        };
        result.insert(node.guid, signature);
    }
    Ok(result)
}

fn validate_edges(
    document: &BiomeGraphDocument,
    signatures: &BTreeMap<u128, (Vec<GraphPin>, Vec<GraphPin>)>,
) -> Result<()> {
    let mut destinations = BTreeSet::new();
    for edge in &document.edges {
        let source = signatures
            .get(&edge.from_node)
            .and_then(|(_, outputs)| outputs.iter().find(|pin| pin.name == edge.from_pin))
            .ok_or_else(|| graph_document("graph.edges.from", "source pin does not exist"))?;
        let destination = signatures
            .get(&edge.to_node)
            .and_then(|(inputs, _)| inputs.iter().find(|pin| pin.name == edge.to_pin))
            .ok_or_else(|| graph_document("graph.edges.to", "destination pin does not exist"))?;
        if source.domain != destination.domain {
            return Err(Error::GraphTypeMismatch {
                from_node: edge.from_node,
                from_pin: edge.from_pin.clone(),
                from_domain: source.domain.as_wire(),
                to_node: edge.to_node,
                to_pin: edge.to_pin.clone(),
                to_domain: destination.domain.as_wire(),
            });
        }
        if !destinations.insert((edge.to_node, edge.to_pin.clone())) {
            return Err(graph_document(
                "graph.edges.to",
                "an input pin has more than one producer",
            ));
        }
    }
    for (guid, (inputs, _)) in signatures {
        for input in inputs.iter().filter(|input| input.required) {
            if !destinations.contains(&(*guid, input.name.clone())) {
                return Err(graph_document(
                    &format!("graph.nodes.{guid:032x}.{}", input.name),
                    "required input is not connected",
                ));
            }
        }
    }
    for output in &document.outputs {
        let pin = signatures
            .get(&output.node)
            .and_then(|(_, outputs)| outputs.iter().find(|pin| pin.name == output.pin))
            .ok_or_else(|| graph_document("graph.outputs", "source pin does not exist"))?;
        if pin.domain != output.domain {
            return Err(graph_document(
                "graph.outputs.domain",
                "output domain does not match pin",
            ));
        }
    }
    Ok(())
}

pub(super) fn topological_order(
    nodes: &[GraphNodeDefinition],
    edges: &[GraphEdge],
) -> Result<Vec<u128>> {
    let mut incoming = nodes
        .iter()
        .map(|node| (node.guid, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing: BTreeMap<u128, BTreeSet<u128>> = BTreeMap::new();
    for edge in edges {
        if !incoming.contains_key(&edge.from_node) {
            return Err(graph_document("graph.edges", "unknown source node"));
        }
        let inserted = outgoing
            .entry(edge.from_node)
            .or_default()
            .insert(edge.to_node);
        if inserted {
            *incoming
                .get_mut(&edge.to_node)
                .ok_or_else(|| graph_document("graph.edges", "unknown destination node"))? += 1;
        }
    }
    let mut ready: BTreeSet<_> = incoming
        .iter()
        .filter_map(|(guid, count)| (*count == 0).then_some(*guid))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(guid) = ready.pop_first() {
        order.push(guid);
        for destination in outgoing.get(&guid).into_iter().flatten() {
            let count = incoming.get_mut(destination).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.insert(*destination);
            }
        }
    }
    if order.len() != nodes.len() {
        let node = incoming
            .into_iter()
            .find_map(|(guid, count)| (count != 0).then_some(guid))
            .unwrap_or(0);
        return Err(Error::GraphCycle { node });
    }
    Ok(order)
}

pub(super) fn incoming_edges(edges: &[GraphEdge]) -> BTreeMap<u128, Vec<&GraphEdge>> {
    let mut result: BTreeMap<u128, Vec<_>> = BTreeMap::new();
    for edge in edges {
        result.entry(edge.to_node).or_default().push(edge);
    }
    for values in result.values_mut() {
        values.sort_by(|left, right| left.to_pin.cmp(&right.to_pin));
    }
    result
}
