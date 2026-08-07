use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_spatial::DecisionScalar;

use crate::{Error, Result};

use super::plant::{
    MAX_PLANT_MODULE_RECURSION, PLANT_ASSET_VERSION, PhenotypeRole, PlantFamilyAsset,
    PlantFamilySource, PlantPivot, PlantSemanticDestination, PlantSourceLocator, PlantSourceRole,
    PlantSourceSelector, SourceAxis, SourceProvenance,
};

/// Validates one plant family without performing any I/O or cooking.
///
/// # Errors
///
/// [`Error::FormatVersion`] on a noncurrent document, and [`Error::InvalidFormat`] naming the
/// section that failed.
pub fn validate_plant_family(asset: &PlantFamilyAsset) -> Result<()> {
    if asset.version != PLANT_ASSET_VERSION {
        return Err(Error::FormatVersion {
            format: ".splant",
            found: asset.version,
            expected: PLANT_ASSET_VERSION,
        });
    }
    if asset.id.value() == 0 || asset.name.trim().is_empty() || asset.parts.is_empty() {
        return Err(field("id/name/parts"));
    }
    if asset.tags.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(field("tags"));
    }
    let mut part_ids = BTreeSet::new();
    for part in &asset.parts {
        if part.id == 0 || !part_ids.insert(part.id) {
            return Err(field("parts.id"));
        }
    }
    if asset.parts.iter().any(|part| {
        part.parent
            .is_some_and(|parent| !part_ids.contains(&parent))
    }) {
        return Err(field("parts.parent"));
    }
    validate_parent_forest(
        asset.parts.iter().map(|part| (part.id, part.parent)),
        "parts.parent",
    )?;
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            let source_by_id = recipe
                .sources
                .iter()
                .map(|source| (source.id, source))
                .collect::<BTreeMap<_, _>>();
            let source_ids = source_by_id.keys().copied().collect::<BTreeSet<_>>();
            let duplicate_contribution =
                recipe.sources.iter().enumerate().any(|(index, source)| {
                    recipe.sources[..index].iter().any(|previous| {
                        previous.locator == source.locator
                            && previous.role == source.role
                            && previous.selector == source.selector
                    })
                });
            if recipe.sources.is_empty()
                || source_ids.len() != recipe.sources.len()
                || duplicate_contribution
                || recipe.sources.iter().any(|source| {
                    source.id == 0
                        || source.content_hash == [0; 32]
                        || source.settings.scale.bits() <= 0
                        || source
                            .settings
                            .uv_scale
                            .iter()
                            .any(|value| value.bits() == 0)
                        || !source_axes_are_orthogonal(
                            source.settings.up_axis,
                            source.settings.forward_axis,
                        )
                        || !valid_source_locator(&source.locator)
                        || !valid_source_selector(&source.selector)
                        || !valid_source_provenance(&source.provenance)
                })
            {
                return Err(field("source.imported.sources"));
            }
            let target_ids = recipe
                .semantic_targets
                .iter()
                .map(|target| target.id)
                .collect::<BTreeSet<_>>();
            let duplicate_binding =
                recipe
                    .semantic_targets
                    .iter()
                    .enumerate()
                    .any(|(index, target)| {
                        recipe.semantic_targets[..index].iter().any(|previous| {
                            previous.source == target.source
                                && previous.selector == target.selector
                                && previous.destination == target.destination
                        })
                    });
            if target_ids.len() != recipe.semantic_targets.len()
                || target_ids.contains(&0)
                || duplicate_binding
                || recipe.semantic_targets.iter().any(|target| {
                    !valid_source_selector(&target.selector)
                        || !source_by_id.get(&target.source).is_some_and(|source| {
                            source_selector_contains(&source.selector, &target.selector)
                                && source_role_supports_destination(source.role, target.destination)
                        })
                        || !semantic_destination_exists(target.destination, &part_ids, asset)
                })
                || recipe.sources.iter().any(|source| {
                    !recipe
                        .semantic_targets
                        .iter()
                        .any(|target| target.source == source.id)
                        || matches!(source.settings.pivot, PlantPivot::SemanticPart(part) if
                        !part_ids.contains(&part)
                            || !recipe.semantic_targets.iter().any(|target| {
                                target.source == source.id
                                    && target.destination
                                        == PlantSemanticDestination::Part(part)
                            }))
                })
                || asset.parts.iter().any(|part| {
                    part.sources.is_empty()
                        || part.sources.iter().copied().collect::<BTreeSet<_>>().len()
                            != part.sources.len()
                        || part.sources.iter().any(|source| {
                            !source_by_id
                                .get(source)
                                .is_some_and(|source| source.role == PlantSourceRole::Geometry)
                        })
                        || !recipe.semantic_targets.iter().any(|target| {
                            part.sources.contains(&target.source)
                                && target.destination == PlantSemanticDestination::Part(part.id)
                        })
                })
            {
                return Err(field("source.imported.semanticTargets"));
            }
        }
        PlantFamilySource::Native { graph, grafts } => {
            // A native family's geometry is grown, so no part may reference an external source.
            if asset.parts.iter().any(|part| !part.sources.is_empty()) {
                return Err(field("source.native.parts"));
            }
            graph.validate().map_err(|_| field("source.native.graph"))?;
            let mut declared = BTreeSet::new();
            for pair in grafts.windows(2) {
                if pair[0].id >= pair[1].id {
                    return Err(field("source.native.grafts.order"));
                }
            }
            for graft in grafts {
                if graft.id == 0 || !declared.insert(graft.id) {
                    return Err(field("source.native.grafts.id"));
                }
                if graft.role != PlantSourceRole::Geometry {
                    return Err(field("source.native.grafts.role"));
                }
                // The two top bits are reserved for the identities a native family derives: its own
                // graph source and one per variation.
                if graft.id >> 126 != 0 {
                    return Err(field("source.native.grafts.reserved"));
                }
            }
            // A graft naming a source the family does not declare has no geometry to substitute.
            for edit in &graph.edits {
                if let crate::BotanicalEditAction::Graft { source, .. } = &edit.action
                    && !declared.contains(source)
                {
                    return Err(field("source.native.grafts.reference"));
                }
            }
        }
    }
    if asset.material_slots.is_empty()
        || asset
            .parts
            .iter()
            .any(|part| part.material_slot as usize >= asset.material_slots.len())
        || asset.material_slots.contains(&Uuid(0))
        || asset
            .material_slots
            .iter()
            .map(|material| material.value())
            .collect::<BTreeSet<_>>()
            .len()
            != asset.material_slots.len()
        || asset.dimensions.height.bits() <= 0
        || asset.dimensions.trunk_radius.bits() < 0
        || asset
            .dimensions
            .crown_radius
            .iter()
            .chain(&asset.dimensions.root_radius)
            .any(|radius| radius.bits() <= 0)
        || (0..3).any(|axis| {
            asset.dimensions.local_bounds_min[axis] >= asset.dimensions.local_bounds_max[axis]
        })
        || asset.dimensions.local_bounds_min[1].bits() > 0
        || asset.dimensions.local_bounds_max[1] < asset.dimensions.height
        || asset.dimensions.crown_radius[0]
            > asset.dimensions.local_bounds_max[0]
                .checked_sub(asset.dimensions.local_bounds_min[0])?
        || asset.dimensions.crown_radius[1]
            > asset.dimensions.local_bounds_max[2]
                .checked_sub(asset.dimensions.local_bounds_min[2])?
    {
        return Err(field("dimensions/materialSlots"));
    }
    let spine_ids: BTreeSet<_> = asset.spines.iter().map(|spine| spine.id).collect();
    if spine_ids.len() != asset.spines.len()
        || spine_ids.contains(&0)
        || asset.spines.iter().any(|spine| {
            !part_ids.contains(&spine.part)
                || spine.rest_points.len() < 2
                || spine.rest_points.len() != spine.radii.len()
                || spine.radii.iter().any(|radius| radius.bits() <= 0)
                || spine
                    .parent
                    .is_some_and(|parent| !spine_ids.contains(&parent))
        })
    {
        return Err(field("spines"));
    }
    validate_parent_forest(
        asset.spines.iter().map(|spine| (spine.id, spine.parent)),
        "spines.parent",
    )?;
    if asset.mechanics.stiffness.bits() < 0
        || asset.mechanics.drag.bits() < 0
        || asset.mechanics.flutter.bits() < 0
        || asset.mechanics.damage_threshold.bits() < 0
        || asset.mechanics.break_threshold < asset.mechanics.damage_threshold
    {
        return Err(field("mechanics"));
    }
    validate_variations(asset, &part_ids)?;
    validate_phenotypes(asset, &part_ids)?;
    validate_proxies(asset, &part_ids)?;
    if let Some(habitat) = &asset.habitat
        && habitat
            .fields
            .iter()
            .any(|(_, minimum, maximum)| minimum > maximum)
    {
        return Err(field("habitat.fields"));
    }
    // Stages only ever advance, and a species cannot relate to itself or name a family twice: both
    // would make the tick's answer depend on which duplicate it read.
    if asset
        .ecology
        .rules
        .stage_ticks
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(field("ecology.rules.stageTicks"));
    }
    let related = asset
        .ecology
        .relations
        .iter()
        .map(|relation| relation.family.value())
        .collect::<BTreeSet<_>>();
    if related.len() != asset.ecology.relations.len()
        || related.contains(&asset.id.value())
        || asset
            .ecology
            .relations
            .windows(2)
            .any(|pair| pair[0].family.value() >= pair[1].family.value())
    {
        return Err(field("ecology.relations"));
    }
    validate_plant_modules(asset)
}

/// Variations name declared sources, and every imported source belongs to at least one variation.
fn validate_variations(asset: &PlantFamilyAsset, part_ids: &BTreeSet<u128>) -> Result<()> {
    let variation_ids = asset
        .variations
        .iter()
        .map(|variation| variation.id)
        .collect::<BTreeSet<_>>();
    let imported_source_ids = match &asset.source {
        PlantFamilySource::Imported(recipe) => Some(
            recipe
                .sources
                .iter()
                .map(|source| source.id)
                .collect::<BTreeSet<_>>(),
        ),
        PlantFamilySource::Native { .. } => None,
    };
    // A native family's variation sources are the per-individual geometry the graph grows, one
    // source per declared variation, so they are derived rather than authored.
    let native_source_ids = match &asset.source {
        PlantFamilySource::Native { graph, .. } => Some(
            (0..graph.variations.len())
                .map(crate::native_variation_source_id)
                .collect::<BTreeSet<_>>(),
        ),
        PlantFamilySource::Imported(_) => None,
    };
    if asset.variations.is_empty()
        || variation_ids.len() != asset.variations.len()
        || asset.variations.iter().any(|variation| {
            let variation_sources = variation.sources.iter().copied().collect::<BTreeSet<_>>();
            let active_parts = variation
                .active_parts
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            variation.name.trim().is_empty()
                || variation_sources.len() != variation.sources.len()
                || active_parts.len() != variation.active_parts.len()
                || variation
                    .active_parts
                    .iter()
                    .any(|part| !part_ids.contains(part))
                || imported_source_ids.as_ref().is_some_and(|sources| {
                    variation.sources.is_empty()
                        || variation
                            .sources
                            .iter()
                            .any(|source| !sources.contains(source))
                        || asset.parts.iter().any(|part| {
                            (variation.active_parts.is_empty() || active_parts.contains(&part.id))
                                && !part
                                    .sources
                                    .iter()
                                    .any(|source| variation_sources.contains(source))
                        })
                })
                || native_source_ids.as_ref().is_some_and(|sources| {
                    variation.sources.len() != 1
                        || variation
                            .sources
                            .iter()
                            .any(|source| !sources.contains(source))
                })
        })
        || imported_source_ids.as_ref().is_some_and(|sources| {
            sources.iter().any(|source| {
                !asset
                    .variations
                    .iter()
                    .any(|variation| variation.sources.contains(source))
            })
        })
    {
        return Err(field("variations"));
    }
    Ok(())
}

/// Every variation offers at least one appearance, and no role repeats within a variation.
fn validate_phenotypes(asset: &PlantFamilyAsset, part_ids: &BTreeSet<u128>) -> Result<()> {
    let variation_ids = asset
        .variations
        .iter()
        .map(|variation| variation.id)
        .collect::<BTreeSet<_>>();
    let phenotype_ids: BTreeSet<_> = asset
        .phenotypes
        .iter()
        .map(|phenotype| phenotype.id)
        .collect();
    let duplicate_role = asset
        .phenotypes
        .iter()
        .enumerate()
        .any(|(index, phenotype)| {
            asset.phenotypes[..index].iter().any(|previous| {
                previous.variation == phenotype.variation && previous.role == phenotype.role
            })
        });
    if asset.phenotypes.is_empty()
        || phenotype_ids.len() != asset.phenotypes.len()
        || duplicate_role
        || !asset
            .phenotypes
            .iter()
            .any(|phenotype| phenotype.role == PhenotypeRole::Healthy)
        || asset.variations.iter().any(|variation| {
            !asset
                .phenotypes
                .iter()
                .any(|phenotype| phenotype.variation == variation.id)
        })
        || asset.phenotypes.iter().any(|phenotype| {
            let material_sources = phenotype
                .material_remap
                .iter()
                .map(|(from, _)| *from)
                .collect::<BTreeSet<_>>();
            let active_parts = phenotype
                .active_parts
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let variation = asset
                .variations
                .iter()
                .find(|variation| variation.id == phenotype.variation);
            !variation_ids.contains(&phenotype.variation)
                || material_sources.len() != phenotype.material_remap.len()
                || active_parts.len() != phenotype.active_parts.len()
                || phenotype.material_remap.iter().any(|(from, to)| {
                    *from as usize >= asset.material_slots.len()
                        || *to as usize >= asset.material_slots.len()
                        || from == to
                })
                || phenotype
                    .active_parts
                    .iter()
                    .any(|part| !part_ids.contains(part))
                || variation.is_some_and(|variation| {
                    !variation.active_parts.is_empty()
                        && phenotype
                            .active_parts
                            .iter()
                            .any(|part| !variation.active_parts.contains(part))
                })
        })
    {
        return Err(field("phenotypes"));
    }
    Ok(())
}

/// Collision and navigation proxies carry unique identities and real extent.
fn validate_proxies(asset: &PlantFamilyAsset, part_ids: &BTreeSet<u128>) -> Result<()> {
    let collision_ids = asset
        .collision_proxies
        .iter()
        .map(|proxy| proxy.id)
        .collect::<BTreeSet<_>>();
    if collision_ids.len() != asset.collision_proxies.len()
        || collision_ids.contains(&0)
        || asset.collision_proxies.iter().any(|proxy| {
            !part_ids.contains(&proxy.part)
                || proxy
                    .dimensions
                    .iter()
                    .any(|dimension| dimension.bits() <= 0)
        })
    {
        return Err(field("collisionProxies"));
    }
    let navigation_ids = asset
        .navigation_proxies
        .iter()
        .map(|proxy| proxy.id)
        .collect::<BTreeSet<_>>();
    if navigation_ids.len() != asset.navigation_proxies.len()
        || navigation_ids.contains(&0)
        || asset.navigation_proxies.iter().any(|proxy| {
            proxy.footprint.len() < 3
                || proxy.height.bits() <= 0
                || polygon_area_twice(&proxy.footprint) == 0
        })
    {
        return Err(field("navigationProxies"));
    }
    Ok(())
}

/// The module table's own invariants: a bounded declared depth, unique non-zero call GUIDs, no
/// self-reference, and exactly one reference per `ModuleCall` node — a call with no reference
/// resolves to nothing, and a reference with no call is a binding nothing can read.
fn validate_plant_modules(asset: &PlantFamilyAsset) -> Result<()> {
    if asset.module_recursion_limit > MAX_PLANT_MODULE_RECURSION {
        return Err(field("moduleRecursionLimit"));
    }
    let call_guids: BTreeSet<_> = asset
        .modules
        .iter()
        .map(|module| module.call_guid)
        .collect();
    if call_guids.len() != asset.modules.len() || call_guids.contains(&0) {
        return Err(field("modules.callGuid"));
    }
    if asset.modules.iter().any(|module| {
        module.plant.value() == 0 || module.plant == asset.id || module.scale.bits() <= 0
    }) {
        return Err(field("modules"));
    }
    let PlantFamilySource::Native { graph, .. } = &asset.source else {
        // An imported family has no graph to call from, so a module table on one is a binding
        // nothing can read.
        return if asset.modules.is_empty() {
            Ok(())
        } else {
            Err(field("modules.source"))
        };
    };
    let mut called = BTreeSet::new();
    for node in &graph.nodes {
        if let crate::BotanicalOperator::ModuleCall { call_guid } = &node.operator
            && (!called.insert(*call_guid) || !call_guids.contains(call_guid))
        {
            return Err(field("modules.callGuid"));
        }
    }
    if called.len() != asset.modules.len() {
        return Err(field("modules"));
    }
    Ok(())
}

fn field(name: &str) -> Error {
    Error::InvalidFormat {
        format: ".splant",
        field: name.to_owned(),
    }
}

fn source_axis_vector(axis: SourceAxis) -> [i8; 3] {
    match axis {
        SourceAxis::PositiveX => [1, 0, 0],
        SourceAxis::NegativeX => [-1, 0, 0],
        SourceAxis::PositiveY => [0, 1, 0],
        SourceAxis::NegativeY => [0, -1, 0],
        SourceAxis::PositiveZ => [0, 0, 1],
        SourceAxis::NegativeZ => [0, 0, -1],
    }
}

fn source_axes_are_orthogonal(up: SourceAxis, forward: SourceAxis) -> bool {
    let up = source_axis_vector(up);
    let forward = source_axis_vector(forward);
    up.into_iter()
        .zip(forward)
        .map(|(first, second)| i16::from(first) * i16::from(second))
        .sum::<i16>()
        == 0
}

fn valid_source_locator(locator: &PlantSourceLocator) -> bool {
    match locator {
        PlantSourceLocator::Asset(id) => id.value() != 0,
        PlantSourceLocator::File(uri) => !uri.trim().is_empty(),
    }
}

fn valid_source_provenance(provenance: &SourceProvenance) -> bool {
    !provenance.source.trim().is_empty()
        && !provenance.source_uri.trim().is_empty()
        && !provenance.license_id.trim().is_empty()
        && !provenance.license_uri.trim().is_empty()
        && (!provenance.requires_attribution
            || (!provenance.author.trim().is_empty() && !provenance.attribution.trim().is_empty()))
}

fn valid_source_selector(selector: &PlantSourceSelector) -> bool {
    match selector {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { id, path } => *id != 0 && !path.trim().is_empty(),
        PlantSourceSelector::Submesh { element, .. } => *element != 0,
    }
}

fn source_selector_contains(source: &PlantSourceSelector, target: &PlantSourceSelector) -> bool {
    match source {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { id, .. } => match target {
            PlantSourceSelector::Whole => false,
            PlantSourceSelector::Element { id: target, .. }
            | PlantSourceSelector::Submesh {
                element: target, ..
            } => id == target,
        },
        PlantSourceSelector::Submesh { element, index } => {
            matches!(target, PlantSourceSelector::Submesh {
                element: target_element,
                index: target_index,
            } if element == target_element && index == target_index)
        }
    }
}

fn source_role_supports_destination(
    role: PlantSourceRole,
    destination: PlantSemanticDestination,
) -> bool {
    matches!(
        (role, destination),
        (
            PlantSourceRole::Geometry,
            PlantSemanticDestination::Part(_) | PlantSemanticDestination::Phenotype(_)
        ) | (
            PlantSourceRole::Material,
            PlantSemanticDestination::MaterialSlot(_) | PlantSemanticDestination::Phenotype(_)
        ) | (
            PlantSourceRole::Skeleton,
            PlantSemanticDestination::Spine(_)
        ) | (
            PlantSourceRole::Collision,
            PlantSemanticDestination::CollisionProxy(_)
        ) | (
            PlantSourceRole::Navigation,
            PlantSemanticDestination::NavigationProxy(_)
        )
    )
}

fn semantic_destination_exists(
    destination: PlantSemanticDestination,
    part_ids: &BTreeSet<u128>,
    asset: &PlantFamilyAsset,
) -> bool {
    match destination {
        PlantSemanticDestination::Part(id) => part_ids.contains(&id),
        PlantSemanticDestination::Spine(id) => asset.spines.iter().any(|spine| spine.id == id),
        PlantSemanticDestination::MaterialSlot(slot) => {
            (slot as usize) < asset.material_slots.len()
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            asset.collision_proxies.iter().any(|proxy| proxy.id == id)
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            asset.navigation_proxies.iter().any(|proxy| proxy.id == id)
        }
        PlantSemanticDestination::Phenotype(id) => {
            asset.phenotypes.iter().any(|phenotype| phenotype.id == id)
        }
    }
}

fn validate_parent_forest(
    entries: impl IntoIterator<Item = (u128, Option<u128>)>,
    name: &str,
) -> Result<()> {
    let parents = entries.into_iter().collect::<BTreeMap<_, _>>();
    for start in parents.keys().copied() {
        let mut active = BTreeSet::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if !active.insert(id) {
                return Err(field(name));
            }
            current = parents.get(&id).copied().flatten();
        }
    }
    Ok(())
}

fn polygon_area_twice(points: &[[DecisionScalar; 2]]) -> i128 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(first, second)| {
            i128::from(first[0].bits()) * i128::from(second[1].bits())
                - i128::from(second[0].bits()) * i128::from(first[1].bits())
        })
        .sum()
}
