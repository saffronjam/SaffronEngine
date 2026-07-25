//! Runtime vegetation residency, query, inspection, and strict state snapshot commands.

use std::str::FromStr;

use saffron_geometry::glam::DVec3;
use saffron_protocol::{
    EmptyParams, InteractionPolicyDto, PlantId as WirePlantId, PlantLifecycleDto, ProvenanceDto,
    ResidencyFacetDto, Uuid as WireUuid, VegetationRuntimeAvailableStatusDto,
    VegetationRuntimeCellParams, VegetationRuntimeCellResult, VegetationRuntimeFacetBytesDto,
    VegetationRuntimePendingCellDto, VegetationRuntimePlantDto,
    VegetationRuntimePlantInspectResult, VegetationRuntimePlantParams,
    VegetationRuntimePlantStateDto, VegetationRuntimeQueryDto, VegetationRuntimeQueryFilterDto,
    VegetationRuntimeQueryHitDto, VegetationRuntimeQueryParams, VegetationRuntimeQueryResult,
    VegetationRuntimeStatusDto, VegetationRuntimeUnavailableReasonDto, VegetationStateImportParams,
    VegetationStateSnapshotDto,
};
use saffron_runtime::{VegetationRuntimeBindingStatus, VegetationRuntimeUnavailableReason};
use saffron_spatial::{
    ResidencyFacet, ResidencyMask, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};
use saffron_vegetation::{
    ContentHash, InteractionPolicy, PlantId, PlantLifecycle, PlantPersistentState, PlantTagId,
    ProvenanceRecord, VegetationPlantSnapshot, VegetationQueryFilter, VegetationQueryRay,
    VegetationWorld,
};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

const MAX_QUERY_RESULTS: usize = 100_000;
const DEFAULT_QUERY_RESULTS: usize = 10_000;

enum RuntimeAvailability {
    Unavailable {
        reason: VegetationRuntimeUnavailableReasonDto,
        detail: Option<String>,
    },
    Available,
}

pub(crate) fn register_runtime_vegetation_commands(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, VegetationRuntimeStatusDto>(
        "vegetation-runtime-status",
        "report the exact runtime vegetation generation, state, queues, residency, and budgets",
        |ctx, _| runtime_status(ctx),
    );
    reg.register::<VegetationRuntimeCellParams, VegetationRuntimeCellResult>(
        "vegetation-runtime-cell",
        "inspect one immutable CPU-resident vegetation cell generation",
        |ctx, params| {
            require_runtime(ctx)?;
            let cell = parse_cell(&params.cell)?;
            let generation = runtime(ctx)?
                .cell_snapshot(cell)
                .ok_or_else(|| Error::command("vegetation runtime cell is not resident"))?;
            Ok(VegetationRuntimeCellResult {
                cell: crate::vegetation_cook_dto::world_cell_dto(cell),
                generation: generation.id().generation.to_string(),
                manifest_identity: generation.manifest_identity().to_string(),
                resident_facets: facets_dto(generation.resident_facets()),
                macro_plants: generation
                    .macro_points()
                    .row_count()
                    .map_err(Error::from)?
                    .to_string(),
                micro_tiles: generation.micro_fields().map_or(0, <[_]>::len).to_string(),
                disturbance_masks: generation.disturbance_masks().len().to_string(),
            })
        },
    );
    reg.register::<VegetationRuntimeQueryParams, VegetationRuntimeQueryResult>(
        "vegetation-runtime-query",
        "query CPU-resident macro vegetation by bounds, radius, ray, or nearest",
        |ctx, params| {
            require_runtime(ctx)?;
            let season = season_mille(ctx);
            runtime_query(runtime(ctx)?, ctx.assets, season, params)
        },
    );
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
            promotion
                .request_promotion(plant)
                .map_err(|error| Error::command(error.to_string()))?;
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
            promotion
                .request_felling(plant)
                .map_err(|error| Error::command(error.to_string()))?;
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
            promotion
                .request_demotion(plant)
                .map_err(|error| Error::command(error.to_string()))?;
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
    reg.register::<EmptyParams, VegetationStateSnapshotDto>(
        "vegetation-state-export",
        "export the canonical strict runtime vegetation state snapshot",
        |ctx, _| {
            require_runtime(ctx)?;
            flush_promoted_state(ctx)?;
            snapshot_dto(runtime(ctx)?)
        },
    );
    reg.register::<saffron_protocol::VegetationMutateParams, saffron_protocol::VegetationMutateResult>(
        "vegetation-mutate",
        "apply typed vegetation mutations (tombstone/override/anchor/…) through the reducer",
        |ctx, params| {
            require_runtime(ctx)?;
            let records = params
                .records
                .iter()
                .map(crate::vegetation_mutation_dto::record_from_dto)
                .collect::<Result<Vec<_>>>()?;
            let applied = u32::try_from(records.len()).map_err(|_| {
                Error::command("vegetation mutation batch exceeds u32 records")
            })?;
            runtime_mut(ctx)?
                .apply_confirmed_mutations(&records)
                .map_err(Error::from)?;
            Ok(saffron_protocol::VegetationMutateResult { applied })
        },
    );
    reg.register::<
        saffron_protocol::VegetationAdvanceEcologyParams,
        saffron_protocol::VegetationEcologyReportDto,
    >(
        "vegetation-advance-ecology",
        "advance biological time and catch dependency regions up to it",
        |ctx, params| {
            require_runtime(ctx)?;
            let target = crate::commands_vegetation::parse_u64(&params.target_tick, "targetTick")?;
            let rules = runtime(ctx)?.ecology_rules();
            let relations = runtime(ctx)?.ecology_relations();
            let plan = saffron_vegetation::EcologyCatchUp {
                target_tick: target,
                budget: saffron_vegetation::EcologyCatchUpBudget {
                    max_ticks: params.max_ticks,
                },
                influence: saffron_vegetation::EcologyInfluence::default(),
                rules: &rules,
                relations: &relations,
                weather: saffron_vegetation::EcologyWeather {
                    water: UnitInterval::from_bits(params.water),
                    warmth: UnitInterval::from_bits(params.warmth),
                },
            };
            let report = runtime_mut(ctx)?
                .advance_ecology(&plan)
                .map_err(Error::from)?;
            let checkpoint = runtime(ctx)?
                .persistent_state()
                .ecology()
                .checkpoint_identity();
            Ok(saffron_protocol::VegetationEcologyReportDto {
                world_tick: report.world_tick.to_string(),
                regions: report.regions as u32,
                regions_caught_up: report.regions_caught_up as u32,
                regions_awaiting_residency: report.regions_awaiting_residency as u32,
                ticks_run: report.ticks_run.to_string(),
                ticks_owed: report.ticks_owed.to_string(),
                checkpoint: encode_hex(&checkpoint.bytes()),
            })
        },
    );
    reg.register::<EmptyParams, saffron_protocol::VegetationEcologyStatusDto>(
        "vegetation-ecology-status",
        "where biological time stands, region by region, with the checkpoint identity",
        |ctx, _| {
            require_runtime(ctx)?;
            ecology_status(runtime(ctx)?)
        },
    );
    reg.register::<saffron_protocol::VegetationCombustionParams, saffron_protocol::VegetationCombustionDto>(
        "vegetation-combustion",
        "sample fuel, moisture, health, occupancy, and what is alight in a volume",
        |ctx, params| {
            require_runtime(ctx)?;
            let bounds = parse_bounds(&params.bounds)?;
            let filter = query_filter(params.filter.clone())?;
            let sample = runtime(ctx)?
                .combustion_sample(bounds, &filter)
                .map_err(Error::from)?;
            Ok(saffron_protocol::VegetationCombustionDto {
                plants: sample.plants,
                ignited: sample.ignited,
                fuel: sample.fuel.bits(),
                moisture: sample.moisture.bits(),
                health: sample.health.bits(),
                occupancy: sample.occupancy.bits(),
            })
        },
    );
    reg.register::<VegetationStateImportParams, VegetationStateSnapshotDto>(
        "vegetation-state-import",
        "verify and atomically import one exact runtime vegetation state snapshot",
        |ctx, params| {
            require_runtime(ctx)?;
            let bytes = decode_hex(&params.data_hex)?;
            runtime_mut(ctx)?
                .import_state_snapshot(&bytes)
                .map_err(Error::from)?;
            snapshot_dto(runtime(ctx)?)
        },
    );
}

fn runtime_status(ctx: &mut EngineContext<'_>) -> Result<VegetationRuntimeStatusDto> {
    match current_availability(ctx) {
        RuntimeAvailability::Unavailable { reason, detail } => {
            Ok(VegetationRuntimeStatusDto::Unavailable { reason, detail })
        }
        RuntimeAvailability::Available => {
            let collision_dto = ctx.vegetation_collision.map(|report| {
                saffron_protocol::VegetationCollisionResidencyDto {
                    resident_cells: report.resident_cells.to_string(),
                    resident_bodies: report.resident_bodies.to_string(),
                    created_total: report.created_total.to_string(),
                    removed_total: report.removed_total.to_string(),
                    hull_skipped_total: report.hull_skipped_total.to_string(),
                    failed_families: report.failed_families.to_string(),
                }
            });
            let promotion_dto = ctx.vegetation_promotion.as_ref().map(|promotion| {
                let report = promotion.report();
                saffron_protocol::VegetationPromotionReportDto {
                    promoted: report.promoted.to_string(),
                    promoting: report.promoting.to_string(),
                    demoting: report.demoting.to_string(),
                    promoted_total: report.promoted_total.to_string(),
                    demoted_total: report.demoted_total.to_string(),
                    felled_total: report.felled_total.to_string(),
                    failed_total: report.failed_total.to_string(),
                    flushed_total: report.flushed_total.to_string(),
                }
            });
            let world = runtime(ctx)?;
            let report = world.residency_report().map_err(Error::from)?;
            let state_bytes = world
                .persistent_state()
                .canonical_bytes()
                .map_err(Error::from)?;
            let persistent_plants =
                world
                    .persistent_state()
                    .cells()
                    .values()
                    .try_fold(0_usize, |total, cell| {
                        total
                            .checked_add(cell.plants.len())
                            .ok_or_else(|| Error::command("persistent plant count overflow"))
                    })?;
            Ok(VegetationRuntimeStatusDto::Available(Box::new(
                VegetationRuntimeAvailableStatusDto {
                    world: WireUuid::from(world.manifest().world),
                    map: WireUuid::from(world.manifest().map),
                    manifest_identity: world.manifest_identity().to_string(),
                    persistent_state_identity: ContentHash::of(&state_bytes).to_string(),
                    persistent_cells: world.persistent_state().cells().len().to_string(),
                    persistent_plants: persistent_plants.to_string(),
                    prediction_count: world.prediction_count().to_string(),
                    source_count: report.source_count.to_string(),
                    requested_cells: report.requested_cells.to_string(),
                    resident_cells: report.resident_cells.to_string(),
                    requested_bytes: facet_bytes_dto(report.requested_bytes),
                    resident_bytes: facet_bytes_dto(report.resident_bytes),
                    budgets: facet_bytes_dto(report.budgets),
                    collision: collision_dto,
                    promotion: promotion_dto,
                    pending: report
                        .pending
                        .into_iter()
                        .map(|pending| VegetationRuntimePendingCellDto {
                            cell: crate::vegetation_cook_dto::world_cell_dto(pending.cell),
                            facets: facets_dto(pending.facets),
                            priority: pending.priority,
                            source_revision: pending.source_revision.to_string(),
                        })
                        .collect(),
                    regeneration_cells: ctx
                        .vegetation_regeneration_cells
                        .iter()
                        .copied()
                        .map(crate::vegetation_cook_dto::world_cell_dto)
                        .collect(),
                },
            )))
        }
    }
}

fn current_availability(ctx: &EngineContext<'_>) -> RuntimeAvailability {
    match &ctx.vegetation_status {
        VegetationRuntimeBindingStatus::Unavailable { reason, detail } => {
            RuntimeAvailability::Unavailable {
                reason: unavailable_reason_dto(reason),
                detail: detail.clone(),
            }
        }
        VegetationRuntimeBindingStatus::Available if ctx.vegetation.is_some() => {
            RuntimeAvailability::Available
        }
        VegetationRuntimeBindingStatus::Available => RuntimeAvailability::Unavailable {
            reason: VegetationRuntimeUnavailableReasonDto::Fault,
            detail: Some(
                "runtime reported an available vegetation binding without a world".to_owned(),
            ),
        },
    }
}

/// The scene calendar's seasonal phase, as the renderer derives it.
fn season_mille(ctx: &mut EngineContext<'_>) -> u16 {
    let calendar = &ctx.scene_edit.active_scene().environment.time_of_day;
    saffron_vegetation::season_phase_mille(
        calendar.year,
        calendar.month,
        calendar.day,
        calendar.latitude,
    )
}

fn require_runtime(ctx: &mut EngineContext<'_>) -> Result<()> {
    match current_availability(ctx) {
        RuntimeAvailability::Available => Ok(()),
        RuntimeAvailability::Unavailable { reason, detail } => {
            Err(Error::command(detail.unwrap_or_else(|| {
                unavailable_reason_message(reason).to_owned()
            })))
        }
    }
}

fn unavailable_reason_dto(
    reason: &VegetationRuntimeUnavailableReason,
) -> VegetationRuntimeUnavailableReasonDto {
    match reason {
        VegetationRuntimeUnavailableReason::NoProject => {
            VegetationRuntimeUnavailableReasonDto::NoProject
        }
        VegetationRuntimeUnavailableReason::NoEnabledField => {
            VegetationRuntimeUnavailableReasonDto::NoEnabledField
        }
        VegetationRuntimeUnavailableReason::NoCookedManifest => {
            VegetationRuntimeUnavailableReasonDto::NoCookedManifest
        }
        VegetationRuntimeUnavailableReason::Fault => VegetationRuntimeUnavailableReasonDto::Fault,
    }
}

fn unavailable_reason_message(reason: VegetationRuntimeUnavailableReasonDto) -> &'static str {
    match reason {
        VegetationRuntimeUnavailableReasonDto::NoProject => "no project is ready",
        VegetationRuntimeUnavailableReasonDto::NoEnabledField => {
            "active scene has no enabled VegetationField"
        }
        VegetationRuntimeUnavailableReasonDto::NoCookedManifest => {
            "selected vegetation map has no current cooked manifest"
        }
        VegetationRuntimeUnavailableReasonDto::Fault => "vegetation runtime binding failed",
    }
}

fn runtime<'a>(ctx: &'a EngineContext<'_>) -> Result<&'a VegetationWorld> {
    ctx.vegetation
        .as_ref()
        .ok_or_else(|| Error::command("vegetation runtime is unavailable"))
}

/// Reduces every promoted plant's live state through the reducer, so a snapshot read in this
/// command sees the promoted plants' current transform and velocity without waiting for anything
/// to settle. A drain with no play world (or no promoted plant) is a no-op.
pub(crate) fn flush_promoted_state(ctx: &mut EngineContext<'_>) -> Result<usize> {
    let Some(promotion) = ctx.vegetation_promotion.as_deref_mut() else {
        return Ok(0);
    };
    let Some(world) = ctx.vegetation.as_mut() else {
        return Ok(0);
    };
    promotion
        .flush_state(ctx.scene_edit.active_scene(), world, ctx.physics.as_deref())
        .map_err(|error| Error::command(error.to_string()))
}

fn navigation_dto(
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

fn event_dto(event: &saffron_vegetation::VegetationEvent) -> saffron_protocol::VegetationEventDto {
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

fn promotion_state_dto(
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

fn promotion_authority<'a>(
    ctx: &'a mut EngineContext<'_>,
) -> Result<&'a mut saffron_runtime::VegetationPromotion> {
    ctx.vegetation_promotion
        .as_deref_mut()
        .ok_or_else(|| Error::command("promotion requires a live play world"))
}

fn runtime_mut<'a>(ctx: &'a mut EngineContext<'_>) -> Result<&'a mut VegetationWorld> {
    ctx.vegetation
        .as_mut()
        .ok_or_else(|| Error::command("vegetation runtime is unavailable"))
}

fn runtime_query(
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

fn plant_inspect(
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

fn plant_state_dto(
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
fn rendered_phenotype_for(
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

fn plant_dto(
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

fn provenance_dto(record: ProvenanceRecord) -> ProvenanceDto {
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

fn query_filter(filter: VegetationRuntimeQueryFilterDto) -> Result<VegetationQueryFilter> {
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

fn snapshot_dto(world: &VegetationWorld) -> Result<VegetationStateSnapshotDto> {
    let bytes = world.export_state_snapshot().map_err(Error::from)?;
    Ok(VegetationStateSnapshotDto {
        manifest_identity: world.manifest_identity().to_string(),
        content_hash: ContentHash::of(&bytes).to_string(),
        bytes: bytes.len().to_string(),
        data_hex: encode_hex(&bytes),
    })
}

fn facet_bytes_dto(bytes: [u64; saffron_spatial::FACET_COUNT]) -> VegetationRuntimeFacetBytesDto {
    VegetationRuntimeFacetBytesDto {
        render: bytes[ResidencyFacet::Render as usize].to_string(),
        physics: bytes[ResidencyFacet::Physics as usize].to_string(),
        simulation: bytes[ResidencyFacet::Simulation as usize].to_string(),
        editing: bytes[ResidencyFacet::Editing as usize].to_string(),
        navigation: bytes[ResidencyFacet::Navigation as usize].to_string(),
        network: bytes[ResidencyFacet::Network as usize].to_string(),
    }
}

fn facets_dto(mask: ResidencyMask) -> Vec<ResidencyFacetDto> {
    mask.iter().map(facet_dto).collect()
}

fn facet_dto(facet: ResidencyFacet) -> ResidencyFacetDto {
    match facet {
        ResidencyFacet::Render => ResidencyFacetDto::Render,
        ResidencyFacet::Physics => ResidencyFacetDto::Physics,
        ResidencyFacet::Simulation => ResidencyFacetDto::Simulation,
        ResidencyFacet::Editing => ResidencyFacetDto::Editing,
        ResidencyFacet::Navigation => ResidencyFacetDto::Navigation,
        ResidencyFacet::Network => ResidencyFacetDto::Network,
    }
}

pub(crate) fn lifecycle_dto(value: PlantLifecycle) -> PlantLifecycleDto {
    match value {
        PlantLifecycle::Seed => PlantLifecycleDto::Seed,
        PlantLifecycle::Sprout => PlantLifecycleDto::Sprout,
        PlantLifecycle::Juvenile => PlantLifecycleDto::Juvenile,
        PlantLifecycle::Mature => PlantLifecycleDto::Mature,
        PlantLifecycle::Senescent => PlantLifecycleDto::Senescent,
        PlantLifecycle::Dead => PlantLifecycleDto::Dead,
        PlantLifecycle::Stump => PlantLifecycleDto::Stump,
        PlantLifecycle::Removed => PlantLifecycleDto::Removed,
    }
}

pub(crate) fn lifecycle_from_dto(value: PlantLifecycleDto) -> PlantLifecycle {
    match value {
        PlantLifecycleDto::Seed => PlantLifecycle::Seed,
        PlantLifecycleDto::Sprout => PlantLifecycle::Sprout,
        PlantLifecycleDto::Juvenile => PlantLifecycle::Juvenile,
        PlantLifecycleDto::Mature => PlantLifecycle::Mature,
        PlantLifecycleDto::Senescent => PlantLifecycle::Senescent,
        PlantLifecycleDto::Dead => PlantLifecycle::Dead,
        PlantLifecycleDto::Stump => PlantLifecycle::Stump,
        PlantLifecycleDto::Removed => PlantLifecycle::Removed,
    }
}

fn interaction_dto(value: InteractionPolicy) -> InteractionPolicyDto {
    match value {
        InteractionPolicy::Decorative => InteractionPolicyDto::Decorative,
        InteractionPolicy::Interactive => InteractionPolicyDto::Interactive,
        InteractionPolicy::Harvestable => InteractionPolicyDto::Harvestable,
        InteractionPolicy::Structural => InteractionPolicyDto::Structural,
    }
}

pub(crate) fn interaction_from_dto(value: InteractionPolicyDto) -> InteractionPolicy {
    match value {
        InteractionPolicyDto::Decorative => InteractionPolicy::Decorative,
        InteractionPolicyDto::Interactive => InteractionPolicy::Interactive,
        InteractionPolicyDto::Harvestable => InteractionPolicy::Harvestable,
        InteractionPolicyDto::Structural => InteractionPolicy::Structural,
    }
}

fn parse_cell(value: &saffron_protocol::WorldCellDto) -> Result<WorldCellKey> {
    let coordinates = [
        parse_i64(&value.coordinates[0], "cell.coordinates[0]")?,
        parse_i64(&value.coordinates[1], "cell.coordinates[1]")?,
        parse_i64(&value.coordinates[2], "cell.coordinates[2]")?,
    ];
    WorldCellKey::new(coordinates[0], coordinates[1], coordinates[2], value.level)
        .map_err(|error| Error::command(error.to_string()))
}

fn parse_bounds(value: &saffron_protocol::WorldBoundsDto) -> Result<WorldBounds> {
    WorldBounds::new(
        parse_ticks(&value.min_ticks, "query.bounds.minTicks")?,
        parse_ticks(&value.max_ticks_exclusive, "query.bounds.maxTicksExclusive")?,
    )
    .map_err(|error| Error::command(error.to_string()))
}

fn parse_position(values: &[String; 3], field: &str) -> Result<WorldPosition> {
    WorldPosition::from_global_ticks(parse_ticks(values, field)?)
        .map_err(|error| Error::command(error.to_string()))
}

fn parse_ticks(values: &[String; 3], field: &str) -> Result<[i128; 3]> {
    Ok([
        parse_i128(&values[0], field)?,
        parse_i128(&values[1], field)?,
        parse_i128(&values[2], field)?,
    ])
}

fn parse_i64(value: &str, field: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i64 decimal")))
}

fn parse_i128(value: &str, field: &str) -> Result<i128> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i128 decimal")))
}

fn ticks_dto(ticks: [i128; 3]) -> [String; 3] {
    ticks.map(|value| value.to_string())
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2)
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(Error::command(
            "snapshot data must be canonical lowercase hexadecimal",
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            std::str::from_utf8(chunk)
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok())
                .ok_or_else(|| Error::command("snapshot data contains invalid hexadecimal"))
        })
        .collect()
}

/// Where biological time stands: world clock, per-region catch-up, and every boundary summary.
fn ecology_status(world: &VegetationWorld) -> Result<saffron_protocol::VegetationEcologyStatusDto> {
    let influence = saffron_vegetation::EcologyInfluence::default();
    let ecology = world.persistent_state().ecology();
    let regions = world
        .ecology_regions(influence)
        .iter()
        .map(|region| {
            let tick = world.ecology_region_tick(region).map_err(Error::from)?;
            Ok(saffron_protocol::VegetationEcologyRegionDto {
                cells: region
                    .cells()
                    .iter()
                    .map(|cell| crate::vegetation_cook_dto::world_cell_dto(*cell))
                    .collect(),
                tick: tick.to_string(),
                caught_up: tick == ecology.clock().tick(),
                resident: world.region_is_resident(region),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(saffron_protocol::VegetationEcologyStatusDto {
        world_tick: ecology.clock().tick().to_string(),
        simulation_version: ecology.version(),
        checkpoint: encode_hex(&ecology.checkpoint_identity().bytes()),
        region_radius_cells: influence.region_radius_cells(),
        regions,
        cells: ecology
            .summaries()
            .iter()
            .map(
                |(cell, summary)| saffron_protocol::VegetationEcologyCellDto {
                    cell: crate::vegetation_cook_dto::world_cell_dto(*cell),
                    tick: summary.tick.to_string(),
                    plants: summary.plants,
                    canopy: summary.canopy.bits(),
                    health: summary.health.bits(),
                    moisture: summary.moisture.bits(),
                    fuel: summary.fuel.bits(),
                },
            )
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::test_support::{StubRenderer, with_stub};

    #[test]
    fn snapshot_hex_is_strict_and_roundtrips() {
        let bytes = b"canonical vegetation state";
        let encoded = encode_hex(bytes);
        assert_eq!(decode_hex(&encoded).unwrap(), bytes);
        assert!(decode_hex("AA").is_err());
        assert!(decode_hex("0").is_err());
    }

    #[test]
    fn filter_mapping_is_exhaustive_and_validates_tags() {
        let filter = query_filter(VegetationRuntimeQueryFilterDto {
            families: vec![WireUuid::from(7_u64)],
            required_tags: vec!["42".to_owned()],
            lifecycles: vec![PlantLifecycleDto::Mature],
            interaction_policies: vec![InteractionPolicyDto::Structural],
        })
        .unwrap();
        assert_eq!(filter.families[0].value(), 7);
        assert_eq!(filter.required_tags.iter().next().unwrap().value(), 42);
        assert!(filter.lifecycles.contains(&PlantLifecycle::Mature));
        assert_eq!(filter.interaction_policies, [InteractionPolicy::Structural]);
        assert!(
            query_filter(VegetationRuntimeQueryFilterDto {
                required_tags: vec!["0".to_owned()],
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn status_reports_the_runtime_sessions_typed_unavailability() {
        let mut registry = CommandRegistry::new();
        register_runtime_vegetation_commands(&mut registry);
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |context| {
            let reply = registry.dispatch(
                context,
                &json!({ "cmd": "vegetation-runtime-status", "params": {} }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["state"], json!("unavailable"));
            assert_eq!(reply["result"]["reason"], json!("no-project"));
        });
    }
}
