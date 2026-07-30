use std::str::FromStr;

use saffron_geometry::glam::DVec3;
use saffron_protocol::{
    PlantId as WirePlantId, ProvenanceDto, Uuid as WireUuid, VegetationRuntimePlantDto,
    VegetationRuntimePlantInspectResult, VegetationRuntimePlantParams,
    VegetationRuntimePlantStateDto, VegetationRuntimeQueryDto, VegetationRuntimeQueryFilterDto,
    VegetationRuntimeQueryHitDto, VegetationRuntimeQueryParams, VegetationRuntimeQueryResult,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    PlantId, PlantPersistentState, PlantTagId, ProvenanceRecord, VegetationPlantSnapshot,
    VegetationQueryFilter, VegetationQueryRay, VegetationWorld,
};

use super::*;
use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

/// Registers per-plant inspection and the promotion lifecycle.
pub(crate) fn register_runtime_plants(reg: &mut CommandRegistry) {
    reg.register::<VegetationRuntimePlantParams, VegetationRuntimePlantInspectResult>(
        "vegetation-runtime-inspect",
        "inspect one stable plant's effective row, persistent state, and resident provenance",
        |ctx, params| {
            require_runtime(ctx)?;
            let season = season_mille(ctx);
            let plant = PlantId::from_str(&params.plant.0).map_err(Error::from)?;
            let promotion = ctx
                .vegetation_promotion
                .as_ref()
                .map(|promotion| promotion_state_dto(promotion.state(plant)));
            let mut result = plant_inspect(runtime(ctx)?, ctx.assets, season, plant)?;
            result.promotion = promotion;
            Ok(result)
        },
    );
    reg.register::<VegetationRuntimePlantParams, saffron_protocol::VegetationPromotionResult>(
        "vegetation-promote",
        "promote one macro plant to a transient entity view at the next synchronization point",
        |ctx, params| {
            require_runtime(ctx)?;
            let plant = PlantId::from_str(&params.plant.0).map_err(Error::from)?;
            let promotion = promotion_authority(ctx)?;
            promotion.request_promotion(plant).map_err(Error::command)?;
            Ok(saffron_protocol::VegetationPromotionResult {
                plant: params.plant,
                state: promotion_state_dto(promotion.state(plant)),
            })
        },
    );
    reg.register::<VegetationRuntimePlantParams, saffron_protocol::VegetationPromotionResult>(
        "vegetation-fell",
        "fell one plant: the rooted plant becomes a stump and a separate product entity spawns",
        |ctx, params| {
            require_runtime(ctx)?;
            let plant = PlantId::from_str(&params.plant.0).map_err(Error::from)?;
            let promotion = promotion_authority(ctx)?;
            promotion.request_felling(plant).map_err(Error::command)?;
            Ok(saffron_protocol::VegetationPromotionResult {
                plant: params.plant,
                state: promotion_state_dto(promotion.state(plant)),
            })
        },
    );
    reg.register::<VegetationRuntimePlantParams, saffron_protocol::VegetationPromotionResult>(
        "vegetation-demote",
        "demote one promoted plant, writing its state back through the reducer",
        |ctx, params| {
            require_runtime(ctx)?;
            let plant = PlantId::from_str(&params.plant.0).map_err(Error::from)?;
            let promotion = promotion_authority(ctx)?;
            promotion.request_demotion(plant).map_err(Error::command)?;
            Ok(saffron_protocol::VegetationPromotionResult {
                plant: params.plant,
                state: promotion_state_dto(promotion.state(plant)),
            })
        },
    );
    reg.register::<
        saffron_protocol::VegetationNavigationParams,
        saffron_protocol::VegetationNavigationResult,
    >(
        "vegetation-nav-contributions",
        "read vegetation's navigation contributions and the regions awaiting a rebuild",
        |ctx, params| {
            let seam = ctx
                .vegetation_navigation
                .as_deref_mut()
                .ok_or_else(|| Error::command("the navigation seam is unavailable"))?;
            let report = seam.report();
            let cells = seam
                .cells()
                .map(|(cell, contributions)| saffron_protocol::VegetationNavigationCellDto {
                    cell: crate::vegetation_cook_dto::world_cell_dto(cell),
                    contributions: contributions.iter().map(navigation_dto).collect(),
                })
                .collect();
            let drained = params.drain_dirty.unwrap_or(false);
            let regions = if drained {
                seam.take_dirty_regions()
            } else {
                seam.dirty_regions().to_vec()
            };
            Ok(saffron_protocol::VegetationNavigationResult {
                cells,
                dirty_regions: regions
                    .into_iter()
                    .map(crate::vegetation_cook_dto::world_bounds_dto)
                    .collect(),
                contributions: report.contributions.to_string(),
                obstacles: report.obstacles.to_string(),
                dynamic_obstacles: report.dynamic_obstacles.to_string(),
                drained,
            })
        },
    );
    reg.register::<
        saffron_protocol::VegetationDrainEventsParams,
        saffron_protocol::VegetationDrainEventsResult,
    >(
        "vegetation-drain-events",
        "read committed vegetation transitions after a cursor",
        |ctx, params| {
            require_runtime(ctx)?;
            let since = match params.since.as_deref() {
                Some(value) => value
                    .parse::<u64>()
                    .map_err(|_| Error::command("since must be a decimal sequence number"))?,
                None => 0,
            };
            let drain = runtime(ctx)?.drain_events(since);
            Ok(saffron_protocol::VegetationDrainEventsResult {
                events: drain.events.iter().map(event_dto).collect(),
                high_water_seq: drain.high_water_seq.to_string(),
                oldest_seq: drain.oldest_seq.to_string(),
                overflowed: drain.overflowed,
            })
        },
    );
}

pub(crate) fn navigation_dto(
    contribution: &saffron_runtime::NavigationContribution,
) -> saffron_protocol::VegetationNavigationContributionDto {
    use saffron_protocol::NavigationContributionKindDto as KindDto;
    use saffron_runtime::NavigationContributionKind as Kind;
    saffron_protocol::VegetationNavigationContributionDto {
        plant: WirePlantId(contribution.plant.to_string()),
        kind: match contribution.kind {
            Kind::Cost => KindDto::Cost,
            Kind::StaticObstacle => KindDto::StaticObstacle,
            Kind::DynamicObstacle => KindDto::DynamicObstacle,
        },
        bounds: crate::vegetation_cook_dto::world_bounds_dto(contribution.bounds),
        footprint: contribution.footprint.clone(),
        height_m: contribution.height_m,
        cost: contribution.cost,
    }
}

pub(crate) fn event_dto(
    event: &saffron_vegetation::VegetationEvent,
) -> saffron_protocol::VegetationEventDto {
    use saffron_protocol::VegetationTransitionKindDto as KindDto;
    use saffron_vegetation::VegetationTransitionKind as Kind;
    let transition = match event.transition.kind {
        Kind::Damaged { amount, health } => KindDto::Damaged {
            amount: amount.bits(),
            health: health.bits(),
        },
        Kind::Harvested { phenotype } => KindDto::Harvested { phenotype },
        Kind::Burned {
            phenotype,
            remaining_fuel,
        } => KindDto::Burned {
            phenotype,
            remaining_fuel: remaining_fuel.bits(),
        },
        Kind::Removed => KindDto::Removed,
        Kind::Planted => KindDto::Planted,
        Kind::Regrew {
            lifecycle,
            phenotype,
        } => KindDto::Regrew {
            lifecycle: lifecycle_dto(lifecycle),
            phenotype,
        },
        Kind::LifecycleChanged { from, to } => KindDto::LifecycleChanged {
            from: from.map(lifecycle_dto),
            to: lifecycle_dto(to),
        },
        Kind::Ignited => KindDto::Ignited,
        Kind::Extinguished => KindDto::Extinguished,
        Kind::Wetted { moisture, fuel } => KindDto::Wetted {
            moisture: moisture.bits(),
            fuel: fuel.bits(),
        },
        Kind::StateReplaced => KindDto::StateReplaced,
        Kind::Moved => KindDto::Moved,
        Kind::Disturbed { categories } => KindDto::Disturbed { categories },
    };
    saffron_protocol::VegetationEventDto {
        seq: event.seq.to_string(),
        transaction: event.transition.transaction.to_string(),
        cell: crate::vegetation_cook_dto::world_cell_dto(event.transition.cell),
        plant: event
            .transition
            .plant
            .map(|plant| WirePlantId(plant.to_string())),
        transition,
    }
}

pub(crate) fn promotion_state_dto(
    state: saffron_runtime::PlantPromotionState,
) -> saffron_protocol::PlantPromotionStateDto {
    use saffron_protocol::PlantPromotionStateDto as Dto;
    use saffron_runtime::PlantPromotionState as State;
    match state {
        State::Bulk => Dto::Bulk,
        State::Promoting => Dto::Promoting,
        State::Promoted { entity } => Dto::Promoted {
            entity: entity.into(),
        },
        State::Demoting { entity } => Dto::Demoting {
            entity: entity.into(),
        },
    }
}

pub(crate) fn promotion_authority<'a>(
    ctx: &'a mut EngineContext<'_>,
) -> Result<&'a mut saffron_runtime::VegetationPromotion> {
    ctx.vegetation_promotion
        .as_deref_mut()
        .ok_or_else(|| Error::command("promotion requires a live play world"))
}

pub(crate) fn runtime_mut<'a>(ctx: &'a mut EngineContext<'_>) -> Result<&'a mut VegetationWorld> {
    ctx.vegetation
        .as_mut()
        .ok_or_else(|| Error::command("vegetation runtime is unavailable"))
}

pub(crate) fn runtime_query(
    world: &VegetationWorld,
    assets: &saffron_assets::AssetServer,
    season: u16,
    params: VegetationRuntimeQueryParams,
) -> Result<VegetationRuntimeQueryResult> {
    let filter = query_filter(params.filter)?;
    let mut hits = match params.query {
        VegetationRuntimeQueryDto::Bounds { bounds } => world
            .query_bounds(parse_bounds(&bounds)?, &filter)
            .map_err(Error::from)?
            .into_iter()
            .map(|plant| (plant, None))
            .collect::<Vec<_>>(),
        VegetationRuntimeQueryDto::Radius {
            center_ticks,
            radius_m,
        } => world
            .query_radius(
                parse_position(&center_ticks, "query.centerTicks")?,
                radius_m,
                &filter,
            )
            .map_err(Error::from)?
            .into_iter()
            .map(|plant| (plant, None))
            .collect(),
        VegetationRuntimeQueryDto::Ray {
            origin_ticks,
            direction,
            max_distance_m,
        } => {
            let ray = VegetationQueryRay::new(
                parse_position(&origin_ticks, "query.originTicks")?,
                DVec3::from_array(direction),
                max_distance_m,
            )
            .map_err(Error::from)?;
            world
                .query_ray(ray, &filter)
                .map_err(Error::from)?
                .into_iter()
                .map(|hit| (hit.plant, Some(hit.distance_m)))
                .collect()
        }
        VegetationRuntimeQueryDto::Nearest {
            position_ticks,
            max_distance_m,
        } => world
            .query_nearest(
                parse_position(&position_ticks, "query.positionTicks")?,
                max_distance_m,
                &filter,
            )
            .map_err(Error::from)?
            .into_iter()
            .map(|hit| (hit.plant, Some(hit.distance_m)))
            .collect(),
    };
    let matches = hits.len();
    let limit = usize::try_from(params.limit.unwrap_or(DEFAULT_QUERY_RESULTS as u32))
        .map_err(|_| Error::command("query limit is not representable"))?;
    if limit == 0 || limit > MAX_QUERY_RESULTS {
        return Err(Error::command(format!(
            "query limit must be between 1 and {MAX_QUERY_RESULTS}"
        )));
    }
    let truncated = hits.len() > limit;
    hits.truncate(limit);
    Ok(VegetationRuntimeQueryResult {
        matches: matches.to_string(),
        truncated,
        hits: hits
            .into_iter()
            .map(|(plant, distance_m)| {
                let rendered = rendered_phenotype_for(assets, season, &plant);
                Ok(VegetationRuntimeQueryHitDto {
                    plant: plant_dto(plant, rendered),
                    distance_m,
                })
            })
            .collect::<Result<_>>()?,
    })
}

pub(crate) fn plant_inspect(
    world: &VegetationWorld,
    assets: &saffron_assets::AssetServer,
    season: u16,
    plant: PlantId,
) -> Result<VegetationRuntimePlantInspectResult> {
    let resident = world
        .find_plant(plant)
        .map_err(Error::from)?
        .map(|snapshot| {
            let rendered = rendered_phenotype_for(assets, season, &snapshot);
            plant_dto(snapshot, rendered)
        });
    // Every cell that recorded state for the plant, not just the first: a plant that moved
    // carries a delta in its base cell and another in the cell it now occupies, and reporting one
    // of them would hide the other.
    let persistent = world
        .persistent_state()
        .cells()
        .iter()
        .filter_map(|(cell, state)| {
            state
                .plants
                .get(&plant)
                .map(|plant_state| plant_state_dto(*cell, state.revision, plant_state))
        })
        .collect::<Vec<_>>();
    if resident.is_none() && persistent.is_empty() {
        return Err(Error::command(
            "plant is not resident and has no persistent state",
        ));
    }
    Ok(VegetationRuntimePlantInspectResult {
        plant: WirePlantId(plant.to_string()),
        resident,
        persistent,
        // The caller fills the promotion state; it belongs to the play session, not the world.
        promotion: None,
    })
}

pub(crate) fn plant_state_dto(
    cell: WorldCellKey,
    revision: u64,
    state: &PlantPersistentState,
) -> VegetationRuntimePlantStateDto {
    VegetationRuntimePlantStateDto {
        cell: crate::vegetation_cook_dto::world_cell_dto(cell),
        cell_revision: revision.to_string(),
        added: state.addition.is_some(),
        tombstoned: state.tombstoned,
        position_ticks: state
            .transform
            .as_ref()
            .map(|transform| transform.0)
            .or_else(|| state.addition.as_ref().map(|point| point.position))
            .map(|position| ticks_dto(position.global_ticks())),
        lifecycle: state.lifecycle.map(lifecycle_dto),
        phenotype: state.phenotype,
        ecology_tick: state.ecology_tick.map(|tick| tick.to_string()),
        health: state.health.map(|value| value.bits()),
        moisture: state.moisture.map(|value| value.bits()),
        fuel: state.fuel.map(|value| value.bits()),
        interaction_policy: state.interaction_policy.map(interaction_dto),
        promoted: state.promotion_origin.is_some(),
    }
}

/// The renderer's phenotype resolution for one snapshot: the family asset's roles
/// and windows against the scene calendar's seasonal phase; the cooked phenotype on
/// any load miss.
pub(crate) fn rendered_phenotype_for(
    assets: &saffron_assets::AssetServer,
    season_mille: u16,
    snapshot: &VegetationPlantSnapshot,
) -> u32 {
    saffron_assets::load_plant_family_asset(assets, snapshot.family)
        .map(|family| {
            saffron_vegetation::resolve_rendered_phenotype(
                family
                    .phenotypes
                    .iter()
                    .map(|phenotype| (phenotype.id, phenotype.role, phenotype.season_window)),
                snapshot.phenotype,
                snapshot.lifecycle,
                season_mille,
            )
        })
        .unwrap_or(snapshot.phenotype)
}

pub(crate) fn plant_dto(
    snapshot: VegetationPlantSnapshot,
    rendered_phenotype: u32,
) -> VegetationRuntimePlantDto {
    VegetationRuntimePlantDto {
        plant: WirePlantId(snapshot.plant.to_string()),
        cell: crate::vegetation_cook_dto::world_cell_dto(snapshot.handle.generation.cell),
        generation: snapshot.handle.generation.generation.to_string(),
        ecology_tick: snapshot.ecology_tick.to_string(),
        position_ticks: ticks_dto(snapshot.position.global_ticks()),
        orientation: snapshot.orientation.bits(),
        scale_bits: snapshot.scale.map(|scale| scale.bits()),
        bounds: crate::vegetation_cook_dto::world_bounds_dto(snapshot.bounds),
        family: WireUuid::from(snapshot.family),
        tags: snapshot
            .tags
            .into_iter()
            .map(|tag| tag.value().to_string())
            .collect(),
        lifecycle: lifecycle_dto(snapshot.lifecycle),
        phenotype: snapshot.phenotype,
        rendered_phenotype,
        interaction_policy: interaction_dto(snapshot.interaction_policy),
        health: snapshot.health.bits(),
        moisture: snapshot.moisture.bits(),
        fuel: snapshot.fuel.bits(),
        provenance: snapshot.provenance.map(provenance_dto),
    }
}

pub(crate) fn provenance_dto(record: ProvenanceRecord) -> ProvenanceDto {
    ProvenanceDto {
        map: WireUuid::from(record.map),
        layer: crate::commands_vegetation::vegetation_guid(record.layer),
        biome: WireUuid::from(record.biome),
        decision: record.decision.0,
        candidate: record.candidate.to_string(),
        family: record.family.map(WireUuid::from),
        plant: record.plant.map(|plant| WirePlantId(plant.to_string())),
        variation: record.variation,
    }
}

pub(crate) fn query_filter(
    filter: VegetationRuntimeQueryFilterDto,
) -> Result<VegetationQueryFilter> {
    Ok(VegetationQueryFilter {
        families: filter.families.into_iter().map(Into::into).collect(),
        required_tags: filter
            .required_tags
            .into_iter()
            .map(|tag| {
                tag.parse::<u64>()
                    .map_err(|_| Error::command("required tag must be a canonical u64 decimal"))
                    .and_then(|value| PlantTagId::new(value).map_err(Error::from))
            })
            .collect::<Result<_>>()?,
        lifecycles: filter
            .lifecycles
            .into_iter()
            .map(lifecycle_from_dto)
            .collect(),
        interaction_policies: filter
            .interaction_policies
            .into_iter()
            .map(interaction_from_dto)
            .collect(),
    })
}
