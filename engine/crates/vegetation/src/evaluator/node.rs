//! Operator dispatch for one compiled graph node.

use super::*;

use std::collections::BTreeMap;

use crate::graph::CompiledDemandSlice;
use crate::{CompiledGraphNode, CompiledGraphUnit, Error, GraphOperator, Result};

pub(super) fn evaluate_node(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    interface_values: &BTreeMap<String, GraphValue>,
    demand: &CompiledDemandSlice,
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    use GraphOperator as O;
    let output_demand = NodeOutputDemand::new(demand, node);
    let produced = match node.definition.operator {
        O::InterfaceInput => {
            let name = string_parameter(node, "name", "")?;
            let value =
                interface_values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: format!("module interface input '{name}' is missing"),
                    })?;
            singleton("value", value)
        }
        O::RegionInput => singleton(
            "regions",
            GraphValue::Regions(if state.inputs.regions.is_empty() {
                canonical_cell_regions(
                    state.inputs.read_bounds,
                    state.inputs.output_cell.level(),
                    0,
                )?
            } else {
                let count = state
                    .inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .count();
                let mut regions = Vec::new();
                crate::memory::reserve_exact(&mut regions, count, "biome input regions")?;
                regions.extend(
                    state
                        .inputs
                        .regions
                        .iter()
                        .filter(|region| region.kind == EvaluationRegionKind::Biome)
                        .copied(),
                );
                regions
            }),
        ),
        O::SplineInput => singleton("splines", GraphValue::Splines(state.inputs.splines.clone())),
        O::SpeciesInput => singleton("species", GraphValue::Species(unit.palette.clone())),
        O::CommunityInput => {
            let mut tables = CommunityTables {
                competition: unit.competition.clone(),
                companions: unit.companions.clone(),
                succession: unit.succession.clone(),
            };
            tables.canonicalize();
            singleton("communities", GraphValue::Communities(tables))
        }
        O::ExplicitAnchors => singleton(
            "candidates",
            GraphValue::Candidates(explicit_anchor_candidates(node, state)?),
        ),
        O::StratifiedCoverage => singleton(
            "candidates",
            GraphValue::Candidates(stratified_candidates(
                node,
                regions_input(inputs, "regions")?,
                state,
            )?),
        ),
        O::BlueNoisePoisson => singleton(
            "candidates",
            GraphValue::Candidates(blue_noise_candidates(
                node,
                regions_input(inputs, "regions")?,
                state,
            )?),
        ),
        O::SurfaceProjection => {
            let (outputs, retained_count) = project_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                output_demand,
                state,
            )?;
            state.diagnostics.candidate_count =
                state.diagnostics.candidate_count.max(retained_count);
            outputs
        }
        O::FieldSample => singleton(
            "field",
            sample_field(
                node,
                candidates_input(inputs, "candidates")?,
                unit.require_authoritative_fields,
                state,
            )?,
        ),
        O::PaintedTile => singleton(
            "field",
            GraphValue::Scalar(sample_painted_tile(
                node,
                candidates_input(inputs, "candidates")?,
                unit.require_authoritative_fields,
                state,
            )?),
        ),
        O::Noise => singleton(
            "field",
            GraphValue::Scalar(noise_field(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::Gradient => singleton(
            "field",
            GraphValue::Scalar(gradient_field(
                node,
                candidates_input(inputs, "candidates")?,
            )?),
        ),
        O::Curve => singleton(
            "field",
            GraphValue::Scalar(curve_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::Remap => singleton(
            "field",
            GraphValue::Scalar(remap_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::Combine => singleton(
            "field",
            GraphValue::Scalar(combine_fields(
                node,
                scalar_input(inputs, "left")?,
                scalar_input(inputs, "right")?,
            )?),
        ),
        O::Clamp => singleton(
            "field",
            GraphValue::Scalar(clamp_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::DistanceField => singleton(
            "field",
            GraphValue::Scalar(distance_field(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::WeightedElimination => singleton(
            "candidates",
            GraphValue::Candidates(weighted_elimination(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                state,
            )?),
        ),
        O::VariableSpacing => singleton(
            "candidates",
            GraphValue::Candidates(variable_spacing(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "radius")?,
                state,
            )?),
        ),
        O::Competition => singleton(
            "candidates",
            GraphValue::Candidates(competition_claims(
                node,
                candidates_input(inputs, "candidates")?,
                communities_input(inputs, "communities")?,
                state,
            )?),
        ),
        O::FieldImportance => singleton(
            "candidates",
            GraphValue::Candidates(threshold_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                state,
            )?),
        ),
        O::Suitability => singleton(
            "candidates",
            GraphValue::Candidates(suitability_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                unit,
                state,
            )?),
        ),
        O::ClusterPatchColony => singleton(
            "candidates",
            GraphValue::Candidates(expand_cluster(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::RecursiveCompanion => singleton(
            "candidates",
            GraphValue::Candidates(expand_companions(
                node,
                candidates_input(inputs, "candidates")?,
                unit,
                state,
            )?),
        ),
        O::SplineFollow => singleton(
            "candidates",
            GraphValue::Candidates(follow_splines(
                node,
                splines_input(inputs, "splines")?,
                state,
            )?),
        ),
        O::Transform => singleton(
            "candidates",
            GraphValue::Candidates(transform_candidates(node, inputs, state)?),
        ),
        O::PriorityExclusion => singleton(
            "candidates",
            GraphValue::Candidates(priority_exclusion(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                scalar_input(inputs, "radius")?,
                state,
            )?),
        ),
        O::BoundsOverlap => singleton(
            "candidates",
            GraphValue::Candidates(bounds_overlap(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::CommunityBlend => singleton(
            "candidates",
            GraphValue::Candidates(community_blend(node, inputs, unit, state)?),
        ),
        O::SuccessionInput => singleton(
            "candidates",
            GraphValue::Candidates(succession_input(
                node,
                candidates_input(inputs, "candidates")?,
                unit,
                state,
            )?),
        ),
        O::MacroOutput => singleton(
            "points",
            GraphValue::Macro(macro_output(node, inputs, state)?),
        ),
        O::MicroOutput => singleton(
            "micro",
            GraphValue::Micro(micro_output(node, inputs, state)?),
        ),
        O::DiagnosticOutput => singleton(
            "diagnostics",
            GraphValue::Diagnostics(vec![diagnostic_output(node, inputs, state)?]),
        ),
        O::ModuleCall => {
            let module = node.module.as_ref().ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "compiled module is missing".to_owned(),
            })?;
            evaluate_unit(
                module,
                child_demand_unit(demand, node)?,
                demand,
                inputs,
                state,
            )?
        }
    };
    let mut outputs = NodeOutputBuilder::new(output_demand);
    outputs.extend(produced)?;
    outputs.finish()
}
