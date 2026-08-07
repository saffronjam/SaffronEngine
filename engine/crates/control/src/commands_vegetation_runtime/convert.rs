use saffron_protocol::{
    EmptyParams, InteractionPolicyDto, PlantLifecycleDto, ResidencyFacetDto,
    VegetationRuntimeFacetBytesDto, VegetationStateImportParams, VegetationStateSnapshotDto,
};
use saffron_spatial::{
    ResidencyFacet, ResidencyMask, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};
use saffron_vegetation::{ContentHash, InteractionPolicy, PlantLifecycle, VegetationWorld};

use super::*;
use crate::error::{Error, Result};
use crate::registry::CommandRegistry;

/// Registers state export/import, mutation, ecology, and combustion.
pub(crate) fn register_runtime_mutation(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, VegetationStateSnapshotDto>(
        "vegetation-state-export",
        "export the canonical strict runtime vegetation state snapshot",
        |ctx, _| {
            require_runtime(ctx)?;
            flush_promoted_state(ctx)?;
            let snapshot = snapshot_dto(runtime(ctx)?)?;
            if let Some(telemetry) = ctx.vegetation_telemetry.as_deref_mut() {
                // The hex payload is two characters a byte; the snapshot's own size is what a
                // caller budgets against.
                telemetry.record_snapshot(snapshot.data_hex.len() / 2);
            }
            Ok(snapshot)
        },
    );
    reg.register::<saffron_protocol::VegetationMutateParams, saffron_protocol::VegetationMutateResult>(
        "vegetation-mutate",
        "apply one gesture of typed vegetation mutations through the reducer, replying with its inverse",
        |ctx, params| {
            require_runtime(ctx)?;
            let gesture = crate::vegetation_mutation_dto::parse_gesture(&params.gesture)?;
            let records = params
                .records
                .iter()
                .map(crate::vegetation_mutation_dto::record_from_dto)
                .collect::<Result<Vec<_>>>()?;
            let applied = u32::try_from(records.len()).map_err(|_| {
                Error::command("vegetation mutation batch exceeds u32 records")
            })?;
            let bytes = records
                .iter()
                .filter_map(|record| record.canonical_byte_len().ok())
                .sum::<usize>();
            // The preimage is read before the batch reduces; afterwards it is gone.
            let journal = saffron_vegetation::EditorJournalEnvelope::capture(
                runtime(ctx)?.persistent_state(),
                gesture,
                records,
            )
            .map_err(Error::from)?;
            runtime_mut(ctx)?
                .apply_confirmed_mutations(&journal.forward)
                .map_err(Error::from)?;
            if let Some(telemetry) = ctx.vegetation_telemetry.as_deref_mut() {
                telemetry.record_mutation(bytes);
            }
            Ok(saffron_protocol::VegetationMutateResult {
                applied,
                inverse: journal
                    .inverse
                    .iter()
                    .map(crate::vegetation_mutation_dto::record_to_dto)
                    .collect(),
            })
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
            let max_ticks = params.max_ticks;
            let report = with_ecology_clock(ctx, |clock, world| {
                clock.advance_to(world, target, max_ticks)
            })?;
            if let Some(telemetry) = ctx.vegetation_telemetry.as_deref_mut() {
                telemetry.record_ecology_ticks(report.ticks_run);
            }
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
                ticks_awaiting_residency: report.ticks_awaiting_residency.to_string(),
                workers: report.workers,
                checkpoint: encode_hex(&checkpoint.bytes()),
            })
        },
    );
    reg.register::<EmptyParams, saffron_protocol::VegetationEcologyStatusDto>(
        "vegetation-ecology-status",
        "where biological time stands, region by region, with the checkpoint identity",
        |ctx, _| {
            require_runtime(ctx)?;
            let clock = ecology_clock_dto(ctx)?;
            let influence = ctx
                .vegetation_ecology
                .as_deref()
                .ok_or_else(|| Error::command("vegetation ecology clock is unavailable"))?
                .influence();
            ecology_status(runtime(ctx)?, influence, clock)
        },
    );
    reg.register::<
        saffron_protocol::VegetationEcologyClockParams,
        saffron_protocol::VegetationEcologyClockDto,
    >(
        "vegetation-ecology-clock",
        "read or set the world simulation clock biology advances on",
        |ctx, params| {
            require_runtime(ctx)?;
            let clock = ctx
                .vegetation_ecology
                .as_deref_mut()
                .ok_or_else(|| Error::command("vegetation ecology clock is unavailable"))?;
            if let Some(running) = params.running {
                clock.set_running(running);
            }
            if let Some(milliseconds) = params.tick_milliseconds {
                clock.set_tick_milliseconds(milliseconds);
            }
            if let Some(ticks) = params.max_ticks_per_sync {
                clock.set_max_ticks_per_sync(ticks);
            }
            if let Some(workers) = params.workers {
                clock.set_workers(workers);
            }
            if params.water.is_some() || params.warmth.is_some() {
                let current = clock.weather();
                clock.set_weather(saffron_vegetation::EcologyWeather {
                    water: params.water.map_or(current.water, UnitInterval::from_bits),
                    warmth: params.warmth.map_or(current.warmth, UnitInterval::from_bits),
                });
            }
            ecology_clock_dto(ctx)
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

/// Runs `body` with the ecology clock and the bound world borrowed together, which is what an
/// explicit step needs: the clock owns the weather, influence, and worker count every tick runs
/// under, so an authored step and the world clock's own ticks cannot diverge on the rules.
fn with_ecology_clock<T>(
    ctx: &mut crate::registry::EngineContext<'_>,
    body: impl FnOnce(
        &mut saffron_runtime::VegetationEcologyClock,
        &mut VegetationWorld,
    ) -> saffron_vegetation::Result<T>,
) -> Result<T> {
    let clock = ctx
        .vegetation_ecology
        .as_deref_mut()
        .ok_or_else(|| Error::command("vegetation ecology clock is unavailable"))?;
    let world = ctx
        .vegetation
        .as_mut()
        .ok_or_else(|| Error::command("vegetation runtime is unavailable"))?;
    body(clock, world).map_err(Error::from)
}

/// The world simulation clock's current standing.
fn ecology_clock_dto(
    ctx: &crate::registry::EngineContext<'_>,
) -> Result<saffron_protocol::VegetationEcologyClockDto> {
    let clock = ctx
        .vegetation_ecology
        .as_deref()
        .ok_or_else(|| Error::command("vegetation ecology clock is unavailable"))?;
    let weather = clock.weather();
    Ok(saffron_protocol::VegetationEcologyClockDto {
        running: clock.running(),
        tick_milliseconds: clock.tick_milliseconds(),
        pending_milliseconds: clock.pending_milliseconds().to_string(),
        max_ticks_per_sync: clock.max_ticks_per_sync(),
        workers: clock.workers(),
        water: weather.water.bits(),
        warmth: weather.warmth.bits(),
        ticks_owed: clock
            .last_report()
            .map_or(0, |report| report.ticks_owed)
            .to_string(),
    })
}

pub(crate) fn snapshot_dto(world: &VegetationWorld) -> Result<VegetationStateSnapshotDto> {
    let bytes = world.export_state_snapshot().map_err(Error::from)?;
    Ok(VegetationStateSnapshotDto {
        manifest_identity: world.manifest_identity().to_string(),
        content_hash: ContentHash::of(&bytes).to_string(),
        bytes: bytes.len().to_string(),
        data_hex: encode_hex(&bytes),
    })
}

/// The cook queue's counters, read from the manager rather than derived from a job walk.
pub(crate) fn cook_queue_dto(
    ctx: &crate::registry::EngineContext<'_>,
) -> saffron_protocol::VegetationCookQueueDto {
    let (counters, live) = ctx.vegetation_cook_jobs.counters();
    saffron_protocol::VegetationCookQueueDto {
        live: live.to_string(),
        submitted: counters.submitted.to_string(),
        completed: counters.completed.to_string(),
        cancelled: counters.cancelled.to_string(),
        superseded: counters.superseded.to_string(),
        failed: counters.failed.to_string(),
        latency_us: counters.latency_us.to_string(),
    }
}

pub(crate) fn facet_bytes_dto(
    bytes: [u64; saffron_spatial::FACET_COUNT],
) -> VegetationRuntimeFacetBytesDto {
    VegetationRuntimeFacetBytesDto {
        render: bytes[ResidencyFacet::Render as usize].to_string(),
        physics: bytes[ResidencyFacet::Physics as usize].to_string(),
        simulation: bytes[ResidencyFacet::Simulation as usize].to_string(),
        editing: bytes[ResidencyFacet::Editing as usize].to_string(),
        navigation: bytes[ResidencyFacet::Navigation as usize].to_string(),
        network: bytes[ResidencyFacet::Network as usize].to_string(),
    }
}

pub(crate) fn facets_dto(mask: ResidencyMask) -> Vec<ResidencyFacetDto> {
    mask.iter().map(facet_dto).collect()
}

pub(crate) fn facet_dto(facet: ResidencyFacet) -> ResidencyFacetDto {
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

pub(crate) fn interaction_dto(value: InteractionPolicy) -> InteractionPolicyDto {
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

pub(crate) fn parse_cell(value: &saffron_protocol::WorldCellDto) -> Result<WorldCellKey> {
    let coordinates = [
        parse_i64(&value.coordinates[0], "cell.coordinates[0]")?,
        parse_i64(&value.coordinates[1], "cell.coordinates[1]")?,
        parse_i64(&value.coordinates[2], "cell.coordinates[2]")?,
    ];
    WorldCellKey::new(coordinates[0], coordinates[1], coordinates[2], value.level)
        .map_err(Error::command)
}

pub(crate) fn parse_bounds(value: &saffron_protocol::WorldBoundsDto) -> Result<WorldBounds> {
    WorldBounds::new(
        parse_ticks(&value.min_ticks, "query.bounds.minTicks")?,
        parse_ticks(&value.max_ticks_exclusive, "query.bounds.maxTicksExclusive")?,
    )
    .map_err(Error::command)
}

pub(crate) fn parse_position(values: &[String; 3], field: &str) -> Result<WorldPosition> {
    WorldPosition::from_global_ticks(parse_ticks(values, field)?).map_err(Error::command)
}

pub(crate) fn parse_ticks(values: &[String; 3], field: &str) -> Result<[i128; 3]> {
    Ok([
        parse_i128(&values[0], field)?,
        parse_i128(&values[1], field)?,
        parse_i128(&values[2], field)?,
    ])
}

pub(crate) fn parse_i64(value: &str, field: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i64 decimal")))
}

pub(crate) fn parse_i128(value: &str, field: &str) -> Result<i128> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i128 decimal")))
}

pub(crate) fn ticks_dto(ticks: [i128; 3]) -> [String; 3] {
    ticks.map(|value| value.to_string())
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

pub(crate) fn decode_hex(value: &str) -> Result<Vec<u8>> {
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
