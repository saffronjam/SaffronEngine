//! Conversions between the botanical graph DTO and the typed document.
//!
//! One place, both directions, so the wire form and the authored document cannot drift. Every value
//! crosses as its exact bit pattern — Q15.16 for lengths, `UnitInterval` bits for ratios and angles,
//! decimal strings for the 128-bit identities JSON cannot hold — so a graph read out and written back
//! is byte-identical.

use saffron_protocol::{
    BotanicalAxisDto, BotanicalCurvePointDto, BotanicalDrawnPointDto, BotanicalEdgeDto,
    BotanicalEditActionDto, BotanicalEditOrphanDto, BotanicalEditOrphanReasonDto,
    BotanicalElementDto, BotanicalGraphDto, BotanicalManualEditDto, BotanicalNodeDto,
    BotanicalOperatorDto, BotanicalPlacementDto, BotanicalVariationDto, PhyllotaxisPatternDto,
    PlantGraftSourceDto, PlantImportSettingsDto, PlantPivotDto, PlantSourceLocatorDto,
    PlantTangentPolicyDto, PruneRuleDto, SourceAxisDto, SourceHandednessDto, SourceUnitsDto,
    SourceUvOriginDto, SourceWindingDto, TropismKindDto, VegetationGuid,
};
use saffron_spatial::{DecisionCurve, DecisionScalar, UnitInterval};
use saffron_vegetation::{
    BotanicalAxis, BotanicalDrawnPoint, BotanicalEdge, BotanicalEditAction, BotanicalEditOrphan,
    BotanicalEditOrphanReason, BotanicalElement, BotanicalElementId, BotanicalGraphDocument,
    BotanicalManualEdit, BotanicalNode, BotanicalOperator, BotanicalPlacement, BotanicalVariation,
    PhyllotaxisPattern, PruneRule, TropismKind,
};

use crate::error::{Error, Result};

/// The DTO form of one botanical graph.
#[must_use]
pub(crate) fn graph_dto(graph: &BotanicalGraphDocument) -> BotanicalGraphDto {
    BotanicalGraphDto {
        variations: graph
            .variations
            .iter()
            .map(|variation| BotanicalVariationDto {
                seed: variation.seed.to_string(),
                age: variation.age.bits(),
                name: variation.name.clone(),
            })
            .collect(),
        nodes: graph
            .nodes
            .iter()
            .map(|node| BotanicalNodeDto {
                guid: node.guid.to_string(),
                version: node.version,
                semantic_revision: node.semantic_revision,
                operator: operator_dto(&node.operator),
            })
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|edge| BotanicalEdgeDto {
                from_node: edge.from_node.to_string(),
                from_pin: edge.from_pin.clone(),
                to_node: edge.to_node.to_string(),
                to_pin: edge.to_pin.clone(),
            })
            .collect(),
        edits: graph
            .edits
            .iter()
            .map(|edit| BotanicalManualEditDto {
                target: edit.target.value().to_string(),
                action: action_dto(&edit.action),
            })
            .collect(),
    }
}

/// The DTO form of one edit action.
#[must_use]
pub(crate) fn action_dto(action: &BotanicalEditAction) -> BotanicalEditActionDto {
    match action {
        BotanicalEditAction::Transform {
            offset,
            roll,
            scale,
        } => BotanicalEditActionDto::Transform {
            offset_bits: offset.map(DecisionScalar::bits),
            roll: roll.bits(),
            scale_bits: scale.bits(),
        },
        BotanicalEditAction::Trim { at } => BotanicalEditActionDto::Trim { at: at.bits() },
        BotanicalEditAction::Remove => BotanicalEditActionDto::Remove,
        BotanicalEditAction::Graft { source, selector } => BotanicalEditActionDto::Graft {
            source: VegetationGuid(format!("{source:032x}")),
            selector: crate::commands_asset::plant_source_selector_dto(selector),
        },
    }
}

/// The DTO form of one graft source.
#[must_use]
pub(crate) fn graft_source_dto(
    source: &saffron_vegetation::PlantSourceReference,
) -> PlantGraftSourceDto {
    PlantGraftSourceDto {
        id: VegetationGuid(format!("{:032x}", source.id)),
        locator: match &source.locator {
            saffron_vegetation::PlantSourceLocator::Asset(asset) => PlantSourceLocatorDto::Asset {
                asset: saffron_protocol::Uuid(asset.value()),
            },
            saffron_vegetation::PlantSourceLocator::File(uri) => {
                PlantSourceLocatorDto::File { uri: uri.clone() }
            }
        },
        selector: crate::commands_asset::plant_source_selector_dto(&source.selector),
        settings: import_settings_dto(&source.settings),
        provenance: crate::commands_asset::source_provenance_dto(&source.provenance),
    }
}

/// The graft source one DTO describes, always a geometry role.
///
/// # Errors
///
/// [`Error::command`] when the identity is not 32 hex digits or a selector is malformed.
pub(crate) fn graft_source_from_dto(
    dto: &PlantGraftSourceDto,
) -> Result<saffron_vegetation::PlantSourceReference> {
    let id = u128::from_str_radix(&dto.id.0, 16)
        .map_err(|_| Error::command("grafts.id must be 32 hex digits"))?;
    Ok(saffron_vegetation::PlantSourceReference {
        id,
        locator: match &dto.locator {
            PlantSourceLocatorDto::Asset { asset } => {
                saffron_vegetation::PlantSourceLocator::Asset(saffron_core::Uuid(asset.0))
            }
            PlantSourceLocatorDto::File { uri } => {
                saffron_vegetation::PlantSourceLocator::File(uri.clone())
            }
        },
        role: saffron_vegetation::PlantSourceRole::Geometry,
        selector: crate::commands_asset::plant_source_selector_from_dto(&dto.selector)?,
        // A fresh declaration has read nothing yet; the first cook records what it found.
        content_hash: [0; 32],
        settings: import_settings_from_dto(&dto.settings)?,
        provenance: crate::commands_asset::source_provenance_from_dto(&dto.provenance),
    })
}

fn import_settings_dto(
    settings: &saffron_vegetation::PlantImportSettings,
) -> PlantImportSettingsDto {
    use saffron_vegetation as veg;
    PlantImportSettingsDto {
        units: match settings.units {
            veg::SourceUnits::Meters => SourceUnitsDto::Meters,
            veg::SourceUnits::Centimeters => SourceUnitsDto::Centimeters,
            veg::SourceUnits::Millimeters => SourceUnitsDto::Millimeters,
            veg::SourceUnits::Feet => SourceUnitsDto::Feet,
        },
        up_axis: axis_dto_of(settings.up_axis),
        forward_axis: axis_dto_of(settings.forward_axis),
        handedness: match settings.handedness {
            veg::SourceHandedness::Right => SourceHandednessDto::Right,
            veg::SourceHandedness::Left => SourceHandednessDto::Left,
        },
        scale_bits: settings.scale.bits(),
        pivot: match settings.pivot {
            veg::PlantPivot::SourceOrigin => PlantPivotDto::SourceOrigin,
            veg::PlantPivot::BoundsBaseCenter => PlantPivotDto::BoundsBaseCenter,
            veg::PlantPivot::Explicit(position) => PlantPivotDto::Explicit {
                position_bits: position.map(DecisionScalar::bits),
            },
            veg::PlantPivot::SemanticPart(part) => PlantPivotDto::SemanticPart {
                part: VegetationGuid(format!("{part:032x}")),
            },
        },
        winding: match settings.winding {
            veg::SourceWinding::CounterClockwise => SourceWindingDto::CounterClockwise,
            veg::SourceWinding::Clockwise => SourceWindingDto::Clockwise,
        },
        uv_origin: match settings.uv_origin {
            veg::SourceUvOrigin::TopLeft => SourceUvOriginDto::TopLeft,
            veg::SourceUvOrigin::BottomLeft => SourceUvOriginDto::BottomLeft,
        },
        uv_scale_bits: settings.uv_scale.map(DecisionScalar::bits),
        uv_offset_bits: settings.uv_offset.map(DecisionScalar::bits),
        tangent_policy: match settings.tangent_policy {
            veg::PlantTangentPolicy::Require => PlantTangentPolicyDto::Require,
            veg::PlantTangentPolicy::GenerateMissing => PlantTangentPolicyDto::GenerateMissing,
            veg::PlantTangentPolicy::Regenerate => PlantTangentPolicyDto::Regenerate,
        },
    }
}

fn import_settings_from_dto(
    dto: &PlantImportSettingsDto,
) -> Result<saffron_vegetation::PlantImportSettings> {
    use saffron_vegetation as veg;
    Ok(veg::PlantImportSettings {
        units: match dto.units {
            SourceUnitsDto::Meters => veg::SourceUnits::Meters,
            SourceUnitsDto::Centimeters => veg::SourceUnits::Centimeters,
            SourceUnitsDto::Millimeters => veg::SourceUnits::Millimeters,
            SourceUnitsDto::Feet => veg::SourceUnits::Feet,
        },
        up_axis: axis_of_dto(dto.up_axis),
        forward_axis: axis_of_dto(dto.forward_axis),
        handedness: match dto.handedness {
            SourceHandednessDto::Right => veg::SourceHandedness::Right,
            SourceHandednessDto::Left => veg::SourceHandedness::Left,
        },
        scale: DecisionScalar::from_bits(dto.scale_bits),
        pivot: match &dto.pivot {
            PlantPivotDto::SourceOrigin => veg::PlantPivot::SourceOrigin,
            PlantPivotDto::BoundsBaseCenter => veg::PlantPivot::BoundsBaseCenter,
            PlantPivotDto::Explicit { position_bits } => {
                veg::PlantPivot::Explicit(position_bits.map(DecisionScalar::from_bits))
            }
            PlantPivotDto::SemanticPart { part } => {
                veg::PlantPivot::SemanticPart(u128::from_str_radix(&part.0, 16).map_err(|_| {
                    Error::command("grafts.settings.pivot.part must be 32 hex digits")
                })?)
            }
        },
        winding: match dto.winding {
            SourceWindingDto::CounterClockwise => veg::SourceWinding::CounterClockwise,
            SourceWindingDto::Clockwise => veg::SourceWinding::Clockwise,
        },
        uv_origin: match dto.uv_origin {
            SourceUvOriginDto::TopLeft => veg::SourceUvOrigin::TopLeft,
            SourceUvOriginDto::BottomLeft => veg::SourceUvOrigin::BottomLeft,
        },
        uv_scale: dto.uv_scale_bits.map(DecisionScalar::from_bits),
        uv_offset: dto.uv_offset_bits.map(DecisionScalar::from_bits),
        tangent_policy: match dto.tangent_policy {
            PlantTangentPolicyDto::Require => veg::PlantTangentPolicy::Require,
            PlantTangentPolicyDto::GenerateMissing => veg::PlantTangentPolicy::GenerateMissing,
            PlantTangentPolicyDto::Regenerate => veg::PlantTangentPolicy::Regenerate,
        },
    })
}

const fn axis_dto_of(axis: saffron_vegetation::SourceAxis) -> SourceAxisDto {
    use saffron_vegetation::SourceAxis;
    match axis {
        SourceAxis::PositiveX => SourceAxisDto::PositiveX,
        SourceAxis::NegativeX => SourceAxisDto::NegativeX,
        SourceAxis::PositiveY => SourceAxisDto::PositiveY,
        SourceAxis::NegativeY => SourceAxisDto::NegativeY,
        SourceAxis::PositiveZ => SourceAxisDto::PositiveZ,
        SourceAxis::NegativeZ => SourceAxisDto::NegativeZ,
    }
}

const fn axis_of_dto(axis: SourceAxisDto) -> saffron_vegetation::SourceAxis {
    use saffron_vegetation::SourceAxis;
    match axis {
        SourceAxisDto::PositiveX => SourceAxis::PositiveX,
        SourceAxisDto::NegativeX => SourceAxis::NegativeX,
        SourceAxisDto::PositiveY => SourceAxis::PositiveY,
        SourceAxisDto::NegativeY => SourceAxis::NegativeY,
        SourceAxisDto::PositiveZ => SourceAxis::PositiveZ,
        SourceAxisDto::NegativeZ => SourceAxis::NegativeZ,
    }
}

/// The DTO form of one grown axis, as an edit addresses it.
#[must_use]
pub(crate) fn axis_dto(axis: &BotanicalAxis) -> BotanicalAxisDto {
    let zero = [DecisionScalar::from_bits(0); 3];
    BotanicalAxisDto {
        id: axis.id.value().to_string(),
        parent: axis.parent.map(|parent| parent.value().to_string()),
        frame: axis.frame.map(|frame| frame.value().to_string()),
        element: element_dto(axis.element),
        base_bits: axis
            .points
            .first()
            .copied()
            .unwrap_or(zero)
            .map(DecisionScalar::bits),
        tip_bits: axis
            .points
            .last()
            .copied()
            .unwrap_or(zero)
            .map(DecisionScalar::bits),
        base_radius_bits: axis.radii.first().copied().map_or(0, DecisionScalar::bits),
        points: u32::try_from(axis.points.len()).unwrap_or(u32::MAX),
    }
}

/// The DTO form of one placed element, as an edit addresses it.
#[must_use]
pub(crate) fn placement_dto(placement: &BotanicalPlacement) -> BotanicalPlacementDto {
    BotanicalPlacementDto {
        id: placement.id.value().to_string(),
        frame: placement.frame.value().to_string(),
        element: element_dto(placement.element),
        material_slot: placement.material_slot,
        position_bits: placement.position.map(DecisionScalar::bits),
        size_bits: placement.size.bits(),
        roll: placement.roll.bits(),
    }
}

/// The DTO form of one orphaned edit.
#[must_use]
pub(crate) fn orphan_dto(orphan: &BotanicalEditOrphan) -> BotanicalEditOrphanDto {
    BotanicalEditOrphanDto {
        target: orphan.target.value().to_string(),
        action: action_dto(&orphan.action),
        reason: match orphan.reason {
            BotanicalEditOrphanReason::TargetMissing => BotanicalEditOrphanReasonDto::TargetMissing,
            BotanicalEditOrphanReason::TargetKind => BotanicalEditOrphanReasonDto::TargetKind,
            BotanicalEditOrphanReason::TargetRemoved => BotanicalEditOrphanReasonDto::TargetRemoved,
        },
    }
}

/// The typed document one DTO describes.
///
/// # Errors
///
/// [`Error::command`] when an identity is not a decimal `u128`, a curve is not canonically ordered,
/// or the assembled document fails structural validation.
pub(crate) fn graph_from_dto(dto: &BotanicalGraphDto) -> Result<BotanicalGraphDocument> {
    let guid = |value: &str, field: &str| -> Result<u128> {
        value
            .parse::<u128>()
            .map_err(|_| Error::command(format!("{field} must be a decimal u128")))
    };
    let graph = BotanicalGraphDocument {
        variations: dto
            .variations
            .iter()
            .map(|variation| {
                Ok(BotanicalVariation {
                    seed: guid(&variation.seed, "graph.variations.seed")?,
                    age: UnitInterval::from_bits(variation.age),
                    name: variation.name.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        nodes: dto
            .nodes
            .iter()
            .map(|node| {
                Ok(BotanicalNode {
                    guid: guid(&node.guid, "graph.nodes.guid")?,
                    version: node.version,
                    semantic_revision: node.semantic_revision,
                    operator: operator_from_dto(&node.operator)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        edges: dto
            .edges
            .iter()
            .map(|edge| {
                Ok(BotanicalEdge {
                    from_node: guid(&edge.from_node, "graph.edges.fromNode")?,
                    from_pin: edge.from_pin.clone(),
                    to_node: guid(&edge.to_node, "graph.edges.toNode")?,
                    to_pin: edge.to_pin.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        edits: dto
            .edits
            .iter()
            .map(|edit| {
                Ok(BotanicalManualEdit {
                    target: BotanicalElementId::from_value(guid(
                        &edit.target,
                        "graph.edits.target",
                    )?),
                    action: match &edit.action {
                        BotanicalEditActionDto::Transform {
                            offset_bits,
                            roll,
                            scale_bits,
                        } => BotanicalEditAction::Transform {
                            offset: offset_bits.map(DecisionScalar::from_bits),
                            roll: UnitInterval::from_bits(*roll),
                            scale: DecisionScalar::from_bits(*scale_bits),
                        },
                        BotanicalEditActionDto::Trim { at } => BotanicalEditAction::Trim {
                            at: UnitInterval::from_bits(*at),
                        },
                        BotanicalEditActionDto::Remove => BotanicalEditAction::Remove,
                        BotanicalEditActionDto::Graft { source, selector } => {
                            BotanicalEditAction::Graft {
                                source: u128::from_str_radix(&source.0, 16).map_err(|_| {
                                    Error::command("graph.edits.graft.source must be 32 hex digits")
                                })?,
                                selector: crate::commands_asset::plant_source_selector_from_dto(
                                    selector,
                                )?,
                            }
                        }
                    },
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    graph
        .validate()
        .map_err(|error| Error::command(error.to_string()))?;
    Ok(graph)
}

fn operator_dto(operator: &BotanicalOperator) -> BotanicalOperatorDto {
    match operator {
        BotanicalOperator::Trunk {
            element,
            length,
            base_radius,
            taper,
            segments,
        } => BotanicalOperatorDto::Trunk {
            element: element_dto(*element),
            length_bits: length.bits(),
            base_radius_bits: base_radius.bits(),
            taper: taper
                .points()
                .iter()
                .map(|(at, factor)| BotanicalCurvePointDto {
                    at: at.bits(),
                    factor_bits: factor.bits(),
                })
                .collect(),
            segments: *segments,
        },
        BotanicalOperator::Branch {
            element,
            length_ratio,
            radius_ratio,
            declination,
            jitter,
            segments,
        } => BotanicalOperatorDto::Branch {
            element: element_dto(*element),
            length_ratio: length_ratio.bits(),
            radius_ratio: radius_ratio.bits(),
            declination: declination.bits(),
            jitter: jitter.bits(),
            segments: *segments,
        },
        BotanicalOperator::Phyllotaxis {
            pattern,
            count,
            nodes,
            start,
            end,
            divergence,
        } => BotanicalOperatorDto::Phyllotaxis {
            pattern: match pattern {
                PhyllotaxisPattern::Alternate => PhyllotaxisPatternDto::Alternate,
                PhyllotaxisPattern::Opposite => PhyllotaxisPatternDto::Opposite,
                PhyllotaxisPattern::Whorled => PhyllotaxisPatternDto::Whorled,
                PhyllotaxisPattern::Spiral => PhyllotaxisPatternDto::Spiral,
            },
            count: *count,
            nodes: *nodes,
            start: start.bits(),
            end: end.bits(),
            divergence: divergence.bits(),
        },
        BotanicalOperator::Tropism { kind, strength } => BotanicalOperatorDto::Tropism {
            kind_of: match kind {
                TropismKind::Phototropism => TropismKindDto::Phototropism,
                TropismKind::Gravitropism => TropismKindDto::Gravitropism,
                TropismKind::Thigmotropism => TropismKindDto::Thigmotropism,
            },
            strength: strength.bits(),
        },
        BotanicalOperator::Prune {
            rule,
            threshold,
            count,
        } => BotanicalOperatorDto::Prune {
            rule: match rule {
                PruneRule::BelowHeight => PruneRuleDto::BelowHeight,
                PruneRule::ShorterThan => PruneRuleDto::ShorterThan,
                PruneRule::KeepStrongest => PruneRuleDto::KeepStrongest,
            },
            threshold_bits: threshold.bits(),
            count: *count,
        },
        BotanicalOperator::Roots {
            depth_ratio,
            spread_ratio,
            count,
        } => BotanicalOperatorDto::Roots {
            depth_ratio: depth_ratio.bits(),
            spread_ratio: spread_ratio.bits(),
            count: *count,
        },
        BotanicalOperator::Shell {
            material_slot,
            sides,
        } => BotanicalOperatorDto::Shell {
            material_slot: *material_slot,
            sides: *sides,
        },
        BotanicalOperator::Instance {
            element,
            material_slot,
            size,
            jitter,
        } => BotanicalOperatorDto::Instance {
            element: element_dto(*element),
            material_slot: *material_slot,
            size_bits: size.bits(),
            jitter: jitter.bits(),
        },
        BotanicalOperator::Drawn { element, points } => BotanicalOperatorDto::Drawn {
            element: element_dto(*element),
            points: points
                .iter()
                .map(|point| BotanicalDrawnPointDto {
                    position_bits: point.position.map(DecisionScalar::bits),
                    radius_bits: point.radius.bits(),
                })
                .collect(),
        },
        BotanicalOperator::ModuleCall { call_guid } => BotanicalOperatorDto::ModuleCall {
            call_guid: saffron_protocol::VegetationGuid(format!("{call_guid:032x}")),
        },
        BotanicalOperator::Family => BotanicalOperatorDto::Family,
    }
}

fn operator_from_dto(operator: &BotanicalOperatorDto) -> Result<BotanicalOperator> {
    let unit = UnitInterval::from_bits;
    let fixed = DecisionScalar::from_bits;
    Ok(match operator {
        BotanicalOperatorDto::Trunk {
            element,
            length_bits,
            base_radius_bits,
            taper,
            segments,
        } => BotanicalOperator::Trunk {
            element: element_from_dto(*element),
            length: fixed(*length_bits),
            base_radius: fixed(*base_radius_bits),
            taper: DecisionCurve::new(
                taper
                    .iter()
                    .map(|point| (unit(point.at), fixed(point.factor_bits)))
                    .collect(),
            )
            .map_err(|error| Error::command(error.to_string()))?,
            segments: *segments,
        },
        BotanicalOperatorDto::Branch {
            element,
            length_ratio,
            radius_ratio,
            declination,
            jitter,
            segments,
        } => BotanicalOperator::Branch {
            element: element_from_dto(*element),
            length_ratio: unit(*length_ratio),
            radius_ratio: unit(*radius_ratio),
            declination: unit(*declination),
            jitter: unit(*jitter),
            segments: *segments,
        },
        BotanicalOperatorDto::Phyllotaxis {
            pattern,
            count,
            nodes,
            start,
            end,
            divergence,
        } => BotanicalOperator::Phyllotaxis {
            pattern: match pattern {
                PhyllotaxisPatternDto::Alternate => PhyllotaxisPattern::Alternate,
                PhyllotaxisPatternDto::Opposite => PhyllotaxisPattern::Opposite,
                PhyllotaxisPatternDto::Whorled => PhyllotaxisPattern::Whorled,
                PhyllotaxisPatternDto::Spiral => PhyllotaxisPattern::Spiral,
            },
            count: *count,
            nodes: *nodes,
            start: unit(*start),
            end: unit(*end),
            divergence: unit(*divergence),
        },
        BotanicalOperatorDto::Tropism { kind_of, strength } => BotanicalOperator::Tropism {
            kind: match kind_of {
                TropismKindDto::Phototropism => TropismKind::Phototropism,
                TropismKindDto::Gravitropism => TropismKind::Gravitropism,
                TropismKindDto::Thigmotropism => TropismKind::Thigmotropism,
            },
            strength: unit(*strength),
        },
        BotanicalOperatorDto::Prune {
            rule,
            threshold_bits,
            count,
        } => BotanicalOperator::Prune {
            rule: match rule {
                PruneRuleDto::BelowHeight => PruneRule::BelowHeight,
                PruneRuleDto::ShorterThan => PruneRule::ShorterThan,
                PruneRuleDto::KeepStrongest => PruneRule::KeepStrongest,
            },
            threshold: fixed(*threshold_bits),
            count: *count,
        },
        BotanicalOperatorDto::Roots {
            depth_ratio,
            spread_ratio,
            count,
        } => BotanicalOperator::Roots {
            depth_ratio: unit(*depth_ratio),
            spread_ratio: unit(*spread_ratio),
            count: *count,
        },
        BotanicalOperatorDto::Shell {
            material_slot,
            sides,
        } => BotanicalOperator::Shell {
            material_slot: *material_slot,
            sides: *sides,
        },
        BotanicalOperatorDto::Instance {
            element,
            material_slot,
            size_bits,
            jitter,
        } => BotanicalOperator::Instance {
            element: element_from_dto(*element),
            material_slot: *material_slot,
            size: fixed(*size_bits),
            jitter: unit(*jitter),
        },
        BotanicalOperatorDto::Drawn { element, points } => BotanicalOperator::Drawn {
            element: element_from_dto(*element),
            points: points
                .iter()
                .map(|point| BotanicalDrawnPoint {
                    position: point.position_bits.map(DecisionScalar::from_bits),
                    radius: fixed(point.radius_bits),
                })
                .collect(),
        },
        BotanicalOperatorDto::ModuleCall { call_guid } => BotanicalOperator::ModuleCall {
            call_guid: u128::from_str_radix(&call_guid.0, 16)
                .map_err(|_| Error::command("moduleCall.callGuid must be 32 hex digits"))?,
        },
        BotanicalOperatorDto::Family => BotanicalOperator::Family,
    })
}

const fn element_dto(element: BotanicalElement) -> BotanicalElementDto {
    match element {
        BotanicalElement::Trunk => BotanicalElementDto::Trunk,
        BotanicalElement::Branch => BotanicalElementDto::Branch,
        BotanicalElement::Root => BotanicalElementDto::Root,
        BotanicalElement::Vine => BotanicalElementDto::Vine,
        BotanicalElement::Frond => BotanicalElementDto::Frond,
        BotanicalElement::Leaf => BotanicalElementDto::Leaf,
        BotanicalElement::Needle => BotanicalElementDto::Needle,
        BotanicalElement::Blade => BotanicalElementDto::Blade,
        BotanicalElement::Flower => BotanicalElementDto::Flower,
        BotanicalElement::Fruit => BotanicalElementDto::Fruit,
        BotanicalElement::Bud => BotanicalElementDto::Bud,
        BotanicalElement::Scar => BotanicalElementDto::Scar,
        BotanicalElement::DeadPart => BotanicalElementDto::DeadPart,
    }
}

const fn element_from_dto(element: BotanicalElementDto) -> BotanicalElement {
    match element {
        BotanicalElementDto::Trunk => BotanicalElement::Trunk,
        BotanicalElementDto::Branch => BotanicalElement::Branch,
        BotanicalElementDto::Root => BotanicalElement::Root,
        BotanicalElementDto::Vine => BotanicalElement::Vine,
        BotanicalElementDto::Frond => BotanicalElement::Frond,
        BotanicalElementDto::Leaf => BotanicalElement::Leaf,
        BotanicalElementDto::Needle => BotanicalElement::Needle,
        BotanicalElementDto::Blade => BotanicalElement::Blade,
        BotanicalElementDto::Flower => BotanicalElement::Flower,
        BotanicalElementDto::Fruit => BotanicalElement::Fruit,
        BotanicalElementDto::Bud => BotanicalElement::Bud,
        BotanicalElementDto::Scar => BotanicalElement::Scar,
        BotanicalElementDto::DeadPart => BotanicalElement::DeadPart,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form round-trips byte-exactly, so reading a graph out and writing it back cannot
    /// perturb the plant it grows.
    #[test]
    fn the_graph_round_trips_through_the_wire_form() {
        let document = BotanicalGraphDocument::sapling(0x5a11);
        let dto = graph_dto(&document);
        let decoded = graph_from_dto(&dto).expect("the wire form decodes");
        assert_eq!(decoded, document);
        assert_eq!(decoded.identity(), document.identity());
        assert_eq!(
            document.nodes[0].version,
            saffron_vegetation::BOTANICAL_NODE_VERSION,
            "the starter graph is authored against the current schema"
        );
    }

    /// A malformed identity or an out-of-range parameter is refused at the seam, not carried into
    /// the authored document.
    #[test]
    fn malformed_input_is_refused() {
        let mut dto = graph_dto(&BotanicalGraphDocument::sapling(0x5a11));
        let good = dto.clone();
        dto.variations[0].seed = "not a number".to_owned();
        assert!(graph_from_dto(&dto).is_err());

        let mut cyclic = good.clone();
        cyclic.edges.push(BotanicalEdgeDto {
            from_node: "6".to_owned(),
            from_pin: "shells".to_owned(),
            to_node: "1".to_owned(),
            to_pin: "axes".to_owned(),
        });
        assert!(graph_from_dto(&cyclic).is_err());
    }
}
