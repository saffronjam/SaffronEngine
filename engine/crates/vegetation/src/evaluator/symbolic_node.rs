//! Symbolic output bounds per operator.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{WeightedSurfaceTag, WorldPosition};

use crate::memory::{
    ALLOCATION_OVERHEAD_BYTES, checked_memory_sum, requested_btree_bytes_for_len,
    requested_vec_bytes_for_len,
};
use crate::{
    CompiledGraphNode, CompiledGraphUnit, Error, GraphAuthority, GraphDomain, GraphOperator,
    PlantPoint, Result,
};

pub(super) fn symbolic_node_outputs(
    context: SymbolicTraversalContext<'_>,
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    incoming: &BTreeMap<u128, Vec<&crate::GraphEdge>>,
    values: &BTreeMap<(u128, String), SymbolicValueBound>,
    bound: &mut SymbolicEvaluationBound,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    use GraphOperator as O;
    let inputs = context.inputs;
    let limits = context.graph.limits;
    let candidate_input = || required_symbolic_items(node, incoming, values, "candidates");
    let field_input = |pin| required_symbolic_items(node, incoming, values, pin);
    let candidate = |items| symbolic_candidate_value(items, limits);
    let scalar = |items| symbolic_field_value(GraphDomain::ScalarField, items, limits);
    let singleton = |name: &str, value| BTreeMap::from([(name.to_owned(), value)]);
    let outputs = match node.definition.operator {
        O::InterfaceInput | O::ModuleCall => unreachable!("handled by symbolic unit traversal"),
        O::RegionInput => {
            let regions = if inputs.regions.is_empty() {
                cell_region_count(inputs.read_bounds, inputs.output_cell.level())?
            } else {
                inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .count()
            };
            singleton(
                "regions",
                symbolic_vec_value::<EvaluationRegion>(
                    GraphDomain::Regions,
                    regions as u64,
                    limits,
                )?,
            )
        }
        O::SplineInput => {
            let points = inputs.splines.iter().try_fold(0_u64, |total, spline| {
                bound_add(
                    "memory bytes",
                    total,
                    spline.points.len() as u64,
                    limits.max_memory_bytes / 64,
                )
            })?;
            singleton(
                "splines",
                SymbolicValueBound {
                    domain: Some(GraphDomain::Splines),
                    items: inputs.splines.len() as u64,
                    bytes: checked_memory_sum([
                        requested_vec_bytes_for_len::<EvaluationSpline>(
                            inputs.splines.len() as u64
                        )?,
                        requested_vec_bytes_for_len::<WorldPosition>(points)?,
                        (inputs.splines.len() as u64)
                            .checked_mul(ALLOCATION_OVERHEAD_BYTES)
                            .ok_or(Error::NumericOverflow)?,
                    ])?,
                    ..SymbolicValueBound::default()
                },
            )
        }
        O::SpeciesInput => singleton(
            "species",
            symbolic_vec_value::<crate::BiomePaletteEntry>(
                GraphDomain::SpeciesTable,
                unit.palette.len() as u64,
                limits,
            )?,
        ),
        O::CommunityInput => singleton(
            "communities",
            SymbolicValueBound {
                domain: Some(GraphDomain::CommunityTable),
                items: 1,
                bytes: checked_memory_sum([
                    requested_vec_bytes_for_len::<crate::CompetitionRule>(
                        unit.competition.len() as u64
                    )?,
                    requested_vec_bytes_for_len::<crate::CompanionRule>(
                        unit.companions.len() as u64
                    )?,
                    requested_vec_bytes_for_len::<crate::SuccessionRule>(
                        unit.succession.len() as u64
                    )?,
                ])?,
                ..SymbolicValueBound::default()
            },
        ),
        O::ExplicitAnchors => {
            let layer = guid_parameter(node, "layer", 0)?;
            let count = inputs
                .anchors
                .iter()
                .filter(|anchor| {
                    anchor.layer == layer && inputs.read_bounds.contains(anchor.point.position)
                })
                .count() as u64;
            singleton("candidates", candidate(count)?)
        }
        O::StratifiedCoverage => {
            let region_inputs = required_symbolic_items(node, incoming, values, "regions")?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                stage_region_scratch_bytes(region_inputs)?,
                limits.max_memory_bytes,
            )?;
            let regions = symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let count = u64::from(u32_parameter(node, "count", 0)?);
            let items = bound_mul("candidate count", regions, count, limits.max_candidates)?;
            singleton("candidates", candidate(items)?)
        }
        O::BlueNoisePoisson => {
            let region_inputs = required_symbolic_items(node, incoming, values, "regions")?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                stage_region_scratch_bytes(region_inputs)?,
                limits.max_memory_bytes,
            )?;
            let regions = symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let count = u64::from(u32_parameter(node, "count", 0)?);
            let items = bound_mul("candidate count", regions, count, limits.max_candidates)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                blue_noise_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::SurfaceProjection => {
            let items = candidate_input()?;
            let output_demand = NodeOutputDemand::new(context.demand, node);
            symbolic_add_rejections(bound, node, items, limits)?;
            let tags_per_hit = symbolic_surface_tags_per_hit(node, inputs)?;
            let tag_bytes = requested_vec_bytes_for_len::<WeightedSurfaceTag>(tags_per_hit)?;
            let has_matching_tile = inputs.surface_projection_tiles.iter().any(|tile| {
                tile.node == node.definition.guid
                    && tile.node_semantic_revision == node.definition.semantic_revision
            });
            if items > 0 && node.definition.authority != GraphAuthority::Cosmetic {
                let tile_bytes_per_item =
                    requested_vec_bytes_for_len::<QuantizedSurfaceProjectionEntry>(1)?;
                let tile_bytes_per_item = bound_add(
                    "memory bytes",
                    tile_bytes_per_item,
                    tag_bytes,
                    limits.max_memory_bytes,
                )?;
                let tile_bytes = bound_mul(
                    "memory bytes",
                    items,
                    tile_bytes_per_item,
                    limits.max_memory_bytes,
                )?;
                if !inputs.surface_providers.is_empty() || has_matching_tile {
                    bound.memory_bytes = bound_add(
                        "memory bytes",
                        bound.memory_bytes,
                        projection_preparation_cache_bytes(items, tags_per_hit)?,
                        limits.max_memory_bytes,
                    )?;
                    symbolic_add_published_input(bound, tile_bytes, limits)?;
                }
                if !inputs.surface_providers.is_empty() {
                    symbolic_add_generated_input(bound, tile_bytes, limits)?;
                }
            }
            let mut outputs = BTreeMap::new();
            if output_demand.contains("candidates") {
                outputs.insert("candidates".to_owned(), candidate(items)?);
            }
            if output_demand.contains("surface") {
                let mut surface = symbolic_field_value(GraphDomain::SurfaceField, items, limits)?;
                surface.bytes = bound_add(
                    "memory bytes",
                    surface.bytes,
                    bound_mul("memory bytes", items, tag_bytes, limits.max_memory_bytes)?,
                    limits.max_memory_bytes,
                )?;
                outputs.insert("surface".to_owned(), surface);
            }
            outputs
        }
        O::FieldSample => {
            let items = candidate_input()?;
            let domain = node
                .outputs
                .iter()
                .find(|output| output.name == "field")
                .map(|output| output.domain)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "field-sample output schema is missing".to_owned(),
                })?;
            let has_matching_tile = inputs.surface_field_query_tiles.iter().any(|tile| {
                tile.node == node.definition.guid
                    && tile.node_semantic_revision == node.definition.semantic_revision
            });
            if items > 0 && node.definition.authority != GraphAuthority::Cosmetic {
                let tile_bytes =
                    requested_vec_bytes_for_len::<QuantizedSurfaceFieldQueryEntry>(items)?;
                if !inputs.surface_providers.is_empty() || has_matching_tile {
                    bound.memory_bytes = bound_add(
                        "memory bytes",
                        bound.memory_bytes,
                        field_preparation_cache_bytes(items)?,
                        limits.max_memory_bytes,
                    )?;
                    symbolic_add_published_input(bound, tile_bytes, limits)?;
                }
                if !inputs.surface_providers.is_empty() {
                    symbolic_add_generated_input(bound, tile_bytes, limits)?;
                }
            }
            singleton("field", symbolic_field_value(domain, items, limits)?)
        }
        O::PaintedTile | O::Noise | O::Gradient | O::DistanceField => {
            singleton("field", scalar(candidate_input()?)?)
        }
        O::Curve | O::Remap | O::Clamp => singleton("field", scalar(field_input("field")?)?),
        O::Combine => singleton(
            "field",
            scalar(field_input("left")?.max(field_input("right")?))?,
        ),
        O::WeightedElimination => {
            let input = candidate_input()?;
            let items = input.min(u64::from(u32_parameter(node, "targetCount", 0)?));
            let scratch = weighted_elimination_scratch_bytes(
                input,
                items,
                u64::from(u32_parameter(node, "maximumNeighbours", 0)?),
            )?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                scratch,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, input, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::VariableSpacing | O::PriorityExclusion => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                xz_filter_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::Competition => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                competition_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::BoundsOverlap => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                bounds_overlap_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::CommunityBlend => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                community_blend_scratch_bytes(items, unit.palette.len() as u64)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::FieldImportance | O::Suitability => {
            let items = candidate_input()?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::ClusterPatchColony => {
            let factor = u64::from(u32_parameter(node, "children", 0)?) + 1;
            let items = bound_mul(
                "candidate count",
                candidate_input()?,
                factor,
                limits.max_candidates,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::RecursiveCompanion => {
            let children = u64::from(u32_parameter(node, "children", 0)?);
            let depth = u32_parameter(node, "maximumDepth", 0)?;
            let mut generation = 1_u64;
            let mut factor = 1_u64;
            for _ in 0..depth {
                generation = bound_mul(
                    "candidate count",
                    generation,
                    children,
                    limits.max_candidates,
                )?;
                factor = bound_add("candidate count", factor, generation, limits.max_candidates)?;
            }
            let items = bound_mul(
                "candidate count",
                candidate_input()?,
                factor,
                limits.max_candidates,
            )?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                companion_scratch_bytes(candidate_input()?, items, unit.companions.len() as u64)?,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::SplineFollow => {
            let items = symbolic_spline_candidate_count(node, inputs, limits, bound)?;
            let region_inputs = if inputs.regions.is_empty() {
                u64::try_from(cell_region_count(
                    inputs.read_bounds,
                    inputs.output_cell.level(),
                )?)
                .map_err(|_| Error::NumericOverflow)?
            } else {
                inputs.regions.len() as u64
            };
            let stage_regions =
                symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let points = inputs.splines.iter().try_fold(0_u64, |total, spline| {
                total
                    .checked_add(spline.points.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?;
            let scratch = checked_memory_sum([
                stage_region_scratch_bytes(region_inputs)?,
                requested_vec_bytes_for_len::<EvaluationRegion>(stage_regions)?,
                requested_vec_bytes_for_len::<[i128; 3]>(points)?,
                requested_vec_bytes_for_len::<SplineSegment>(points)?,
                requested_vec_bytes_for_len::<[i128; 3]>(items)?,
                requested_vec_bytes_for_len::<GraphCandidate>(items)?,
            ])?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                scratch,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::Transform | O::SuccessionInput => {
            singleton("candidates", candidate(candidate_input()?)?)
        }
        O::MacroOutput => {
            let items = candidate_input()?;
            check_limit("accepted count", items, limits.max_macro_points)?;
            symbolic_add_rejections(bound, node, items, limits)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                macro_output_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            singleton(
                "points",
                symbolic_vec_value::<PlantPoint>(GraphDomain::MacroPoints, items, limits)?,
            )
        }
        O::MicroOutput => {
            let dimensions = u32_vec3_parameter(node, "dimensions")?;
            let mut samples_per_family = 1_u64;
            for dimension in dimensions {
                samples_per_family = bound_mul(
                    "micro samples",
                    samples_per_family,
                    u64::from(dimension),
                    limits.max_micro_samples,
                )?;
            }
            let families = candidate_input()?;
            let samples = bound_mul(
                "micro samples",
                samples_per_family,
                families,
                limits.max_micro_samples,
            )?;
            let channels = guid_list_parameter(node, "attributeChannels")?.len() as u64;
            let attribute_maps = channels
                .checked_mul(families)
                .ok_or(Error::NumericOverflow)?;
            let attribute_values = bound_mul(
                "memory bytes",
                channels,
                requested_vec_bytes_for_len::<i32>(samples)?,
                limits.max_memory_bytes,
            )?;
            let bytes = checked_memory_sum([
                requested_vec_bytes_for_len::<MicroFieldTile>(families)?,
                requested_vec_bytes_for_len::<u16>(samples)?,
                requested_btree_bytes_for_len::<u128, Vec<i32>>(attribute_maps)?,
                attribute_values,
            ])?;
            check_limit("memory bytes", bytes, limits.max_memory_bytes)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                micro_output_scratch_bytes(samples, channels)?,
                limits.max_memory_bytes,
            )?;
            singleton(
                "micro",
                SymbolicValueBound {
                    domain: Some(GraphDomain::MicroField),
                    items: samples,
                    bytes,
                    ..SymbolicValueBound::default()
                },
            )
        }
        O::DiagnosticOutput => {
            let candidates = symbolic_input(node, incoming, values, "candidates")?
                .map_or(0, |value| value.items);
            let field =
                symbolic_input(node, incoming, values, "field")?.map_or(0, |value| value.items);
            let candidate_bytes =
                requested_vec_bytes_for_len::<DiagnosticCandidateSample>(candidates)?;
            let field_bytes = requested_vec_bytes_for_len::<DiagnosticScalarSample>(field)?;
            let rejected_bytes = requested_vec_bytes_for_len::<RejectedCandidate>(bound.rejected)?;
            let label_bytes = string_parameter(node, "label", "")?.len() as u64;
            let container_bytes = checked_memory_sum([
                requested_vec_bytes_for_len::<NamedDiagnosticStream>(1)?,
                requested_vec_bytes_for_len::<u128>(node.debug_symbol.module_path.len() as u64)?,
                requested_vec_bytes_for_len::<u8>(label_bytes)?,
            ])?;
            singleton(
                "diagnostics",
                SymbolicValueBound {
                    domain: Some(GraphDomain::Diagnostics),
                    items: 1,
                    bytes: checked_memory_sum([
                        candidate_bytes,
                        field_bytes,
                        rejected_bytes,
                        container_bytes,
                    ])?,
                    diagnostic_candidates: candidates,
                    diagnostic_fields: field,
                    diagnostic_rejected: bound.rejected,
                    diagnostic_module_path_items: node.debug_symbol.module_path.len() as u64,
                    diagnostic_label_bytes: label_bytes,
                },
            )
        }
    };
    Ok(outputs)
}
