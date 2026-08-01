use std::str::FromStr;

use saffron_protocol::{
    EmptyParams, Uuid as WireUuid, VegetationRuntimeAvailableStatusDto,
    VegetationRuntimeCellParams, VegetationRuntimeCellResult, VegetationRuntimePendingCellDto,
    VegetationRuntimeQueryParams, VegetationRuntimeQueryResult, VegetationRuntimeStatusDto,
    VegetationRuntimeUnavailableReasonDto,
};
use saffron_runtime::{VegetationRuntimeBindingStatus, VegetationRuntimeUnavailableReason};
use saffron_vegetation::{ContentHash, PlantId, VegetationWorld};

use super::*;
use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

/// Registers residency, query, interchange, verification, and telemetry.
pub(crate) fn register_runtime_state(reg: &mut CommandRegistry) {
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
            let result = runtime_query(runtime(ctx)?, ctx.assets, season, params)?;
            let cost = runtime(ctx)?.take_query_cost();
            if let Some(telemetry) = ctx.vegetation_telemetry.as_deref_mut() {
                telemetry.record_query(cost);
            }
            Ok(result)
        },
    );
    reg.register::<saffron_protocol::UsdSkeletonsParams, saffron_protocol::UsdSkeletonsResult>(
        "vegetation-usd-skeletons",
        "vegetation-usd-skeletons {path} — the UsdSkel skeletons a USD stage declares",
        |_ctx, params| {
            let text = std::fs::read_to_string(&params.path).map_err(Error::command)?;
            let (skeletons, unsupported) =
                saffron_vegetation::read_usd_skeletons(&text).map_err(Error::command)?;
            Ok(saffron_protocol::UsdSkeletonsResult {
                skeletons: skeletons
                    .into_iter()
                    .map(|skeleton| saffron_protocol::UsdSkeletonDto {
                        name: skeleton.name,
                        skel_root: skeleton.skel_root,
                        joints: skeleton
                            .joints
                            .into_iter()
                            .map(|joint| saffron_protocol::UsdJointDto {
                                path: joint.path,
                                parent: joint.parent.and_then(|index| u32::try_from(index).ok()),
                                rest: joint.rest.to_vec(),
                                bind: joint.bind.to_vec(),
                            })
                            .collect(),
                    })
                    .collect(),
                unsupported,
            })
        },
    );
    reg.register::<
        saffron_protocol::VegetationWindRecordParams,
        saffron_protocol::VegetationWindRecordResult,
    >(
        "vegetation-wind-record",
        "vegetation-wind-record {cell, plant} — one plant's GPU wind prepass record",
        |ctx, params| {
            let cell = parse_cell(&params.cell)?;
            let plant = PlantId::from_str(&params.plant.0)
                .map_err(Error::command)?;
            let captured = ctx
                .renderer
                .capture_plant_wind_record(cell, plant)
                .map_err(Error::command)?
                .ok_or_else(|| {
                    Error::command("the plant is not mirrored, or no frame has run yet")
                })?;
            let record = captured.record;
            Ok(saffron_protocol::VegetationWindRecordResult {
                slot: captured.slot,
                sway_current_m: record.sway_current,
                sway_previous_m: record.sway_previous,
                interaction_current_m: record.interaction_current,
                interaction_previous_m: record.interaction_previous,
                interaction_reset: record.interaction_reset != 0.0,
                branch_quadrature: record.branch_quadrature,
                branch_amplitude_m: record.branch_amplitude,
                flutter_amplitude_m: record.flutter_amplitude,
                height_scale: record.height_scale,
                bounds_inflation_m: record.bounds_inflation,
                mechanics: (captured.mechanics != [0; 4]).then(|| {
                    saffron_protocol::VegetationMechanicsDto {
                        stiffness: captured.mechanics[0] as i32 as f32 / 65_536.0,
                        drag: captured.mechanics[1] as i32 as f32 / 65_536.0,
                        flutter: captured.mechanics[2] as i32 as f32 / 65_536.0,
                        damping: (captured.mechanics[3] & 0xffff) as f32 / 65_535.0,
                        bend_limit: (captured.mechanics[3] >> 16) as f32 / 65_535.0,
                    }
                }),
            })
        },
    );
    reg.register::<
        saffron_protocol::VegetationBudgetsParams,
        saffron_protocol::VegetationBudgetsResult,
    >(
        "vegetation-budgets",
        "vegetation-budgets {cellPlants?, familyInstances?, familyMicroPredicted?} — resident-population budgets (omit to read)",
        |ctx, params| {
            let mut budgets = ctx.renderer.vegetation_budgets();
            if let Some(value) = params.cell_plants {
                budgets.cell_plants = value;
            }
            if let Some(value) = params.family_instances {
                budgets.family_instances = value;
            }
            if let Some(value) = params.family_micro_predicted {
                budgets.family_micro_predicted = value
                    .parse::<u64>()
                    .map_err(|_| Error::command("familyMicroPredicted must be a u64 string"))?;
            }
            ctx.renderer.set_vegetation_budgets(budgets);
            Ok(saffron_protocol::VegetationBudgetsResult {
                cell_plants: budgets.cell_plants,
                family_instances: budgets.family_instances,
                family_micro_predicted: budgets.family_micro_predicted.to_string(),
            })
        },
    );

    reg.register::<saffron_protocol::VegetationVerifyParams, saffron_protocol::VegetationVerifyResult>(
        "vegetation-verify-artifacts",
        "rehash every artifact the current generations name, optionally removing corrupt ones",
        |ctx, params| {
            let maps: Vec<saffron_core::Uuid> = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| entry.asset_type == saffron_scene::AssetType::VegetationMap)
                .map(|entry| entry.id)
                .collect();
            let store = ctx.assets.vegetation_artifact_store();
            let state = ctx.assets.vegetation_state_store();
            let report =
                saffron_assets::verify_vegetation_artifacts(&store, &state, maps, params.repair)
                    .map_err(Error::command)?;
            Ok(saffron_protocol::VegetationVerifyResult {
                checked: report.checked.to_string(),
                repaired: report.repaired.to_string(),
                faults: report
                    .faults
                    .iter()
                    .map(|fault| saffron_protocol::VegetationArtifactFaultDto {
                        path: fault.relative.display().to_string(),
                        fault: fault.fault.name().to_owned(),
                    })
                    .collect(),
            })
        },
    );
    reg.register::<saffron_protocol::EmptyParams, saffron_protocol::VegetationStateBaselineResult>(
        "vegetation-state-baseline",
        "publish the current runtime vegetation state as the generation's starting state",
        |ctx, _| {
            require_runtime(ctx)?;
            // The same barrier a save takes: a promoted plant's live transform reduces through the
            // reducer first, so the baseline is complete without waiting for anything to settle.
            flush_promoted_state(ctx)?;
            let world = runtime(ctx)?;
            let identity = world.manifest_identity();
            let map = world.manifest().map;
            let bytes = world
                .persistent_state()
                .canonical_bytes()
                .map_err(Error::from)?;
            let cells = world.persistent_state().cells().len();
            ctx.assets
                .vegetation_state_store()
                .publish_baseline(map, identity, &bytes)
                .map_err(Error::command)?;
            Ok(saffron_protocol::VegetationStateBaselineResult {
                manifest_identity: identity.to_string(),
                bytes: bytes.len().to_string(),
                cells: cells.to_string(),
            })
        },
    );
    reg.register::<saffron_protocol::EmptyParams, saffron_protocol::VegetationTelemetryResult>(
        "vegetation-telemetry",
        "compact vegetation runtime telemetry: stage times, work counters, resident bytes",
        |ctx, _| {
            require_runtime(ctx)?;
            let resident_bytes = facet_bytes_dto(
                runtime(ctx)?
                    .residency_report()
                    .map_err(Error::from)?
                    .resident_bytes,
            );
            let collision_bodies = ctx
                .vegetation_collision
                .map_or(0, |report| report.resident_bodies);
            let navigation_contributions = ctx
                .vegetation_navigation
                .as_deref()
                .map_or(0, |seam| seam.report().contributions as u64);
            let promoted = ctx
                .vegetation_promotion
                .as_deref()
                .map_or(0, |promotion| promotion.report().promoted);
            let telemetry = ctx
                .vegetation_telemetry
                .as_deref()
                .ok_or_else(|| Error::command("vegetation telemetry is not available"))?;
            let times = |times: saffron_runtime::VegetationStageTimes| {
                saffron_protocol::VegetationStageTimesDto {
                    residency_us: times.residency.as_micros() as u64,
                    promotion_us: times.promotion.as_micros() as u64,
                    collision_us: times.collision.as_micros() as u64,
                    navigation_us: times.navigation.as_micros() as u64,
                    ecology_us: times.ecology.as_micros() as u64,
                    total_us: times.total().as_micros() as u64,
                }
            };
            let work = telemetry.work();
            Ok(saffron_protocol::VegetationTelemetryResult {
                last: times(telemetry.last()),
                average: times(telemetry.average()),
                work: saffron_protocol::VegetationWorkCountersDto {
                    synchronizations: work.synchronizations.to_string(),
                    queries: work.queries.to_string(),
                    query_hits: work.query_hits.to_string(),
                    query_generations_visited: work.query_generations_visited.to_string(),
                    query_nodes_visited: work.query_nodes_visited.to_string(),
                    query_rows_tested: work.query_rows_tested.to_string(),
                    mutations: work.mutations.to_string(),
                    mutation_bytes: work.mutation_bytes.to_string(),
                    snapshots: work.snapshots.to_string(),
                    snapshot_bytes: work.snapshot_bytes.to_string(),
                    ecology_ticks: work.ecology_ticks.to_string(),
                },
                resident_bytes,
                collision_bodies: collision_bodies.to_string(),
                navigation_contributions: navigation_contributions.to_string(),
                promoted: promoted.to_string(),
                cook_queue: cook_queue_dto(ctx),
            })
        },
    );
    super::network::register_runtime_network(reg);
}

pub(crate) fn runtime_status(ctx: &mut EngineContext<'_>) -> Result<VegetationRuntimeStatusDto> {
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
                    released_total: report.released_total.to_string(),
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

pub(crate) fn current_availability(ctx: &EngineContext<'_>) -> RuntimeAvailability {
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
pub(crate) fn season_mille(ctx: &mut EngineContext<'_>) -> u16 {
    let calendar = &ctx.scene_edit.active_scene().environment.time_of_day;
    saffron_vegetation::season_phase_mille(
        calendar.year,
        calendar.month,
        calendar.day,
        calendar.latitude,
    )
}

pub(crate) fn require_runtime(ctx: &mut EngineContext<'_>) -> Result<()> {
    match current_availability(ctx) {
        RuntimeAvailability::Available => Ok(()),
        RuntimeAvailability::Unavailable { reason, detail } => {
            Err(Error::command(detail.unwrap_or_else(|| {
                unavailable_reason_message(reason).to_owned()
            })))
        }
    }
}

pub(crate) fn unavailable_reason_dto(
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

pub(crate) fn unavailable_reason_message(
    reason: VegetationRuntimeUnavailableReasonDto,
) -> &'static str {
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

pub(crate) fn runtime<'a>(ctx: &'a EngineContext<'_>) -> Result<&'a VegetationWorld> {
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
        .map_err(Error::command)
}

/// Where biological time stands: world clock, per-region catch-up, and every boundary summary.
pub(crate) fn ecology_status(
    world: &VegetationWorld,
    influence: saffron_vegetation::EcologyInfluence,
    clock: saffron_protocol::VegetationEcologyClockDto,
) -> Result<saffron_protocol::VegetationEcologyStatusDto> {
    let ecology = world.persistent_state().ecology();
    let regions = world
        .ecology_region_standings(influence)
        .map_err(Error::from)?
        .into_iter()
        .map(|standing| saffron_protocol::VegetationEcologyRegionDto {
            cells: standing
                .region
                .cells()
                .iter()
                .map(|cell| crate::vegetation_cook_dto::world_cell_dto(*cell))
                .collect(),
            tick: standing.tick.to_string(),
            caught_up: standing.tick == ecology.clock().tick(),
            resident: standing.resident,
        })
        .collect();
    Ok(saffron_protocol::VegetationEcologyStatusDto {
        world_tick: ecology.clock().tick().to_string(),
        simulation_version: ecology.version(),
        checkpoint: encode_hex(&ecology.checkpoint_identity().bytes()),
        region_radius_cells: influence.region_radius_cells(),
        clock,
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
