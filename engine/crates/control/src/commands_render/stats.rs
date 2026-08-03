use saffron_protocol::{
    AnamorphicParams, EmptyParams, GpuSceneMirrorStatsDto, RenderPassTimingDto,
    RenderPassTimingsDto, RenderStatsDto, Uuid, VsmPageFamilyStatsDto, VsmStatsDto,
};
use saffron_rendering::{PassTiming, RenderStatsFull};

use super::*;
use crate::registry::{CommandRegistry, ControlRenderer};

pub(crate) fn render_stats_dto(renderer: &dyn ControlRenderer) -> RenderStatsDto {
    let stats: RenderStatsFull = renderer.render_stats();
    RenderStatsDto {
        draw_calls: stats.draw.draw_calls as i32,
        batches: stats.draw.batches as i32,
        instances: stats.draw.instances as i32,
        scene_gather_ms: stats.scene_gather_ms,
        scene_gather_entities: stats.scene_gather_entities as i32,
        instance_upload_bytes: stats.draw.instance_upload_bytes,
        retained_mesh_cpu_bytes: stats.draw.retained_mesh_cpu_bytes,
        shadow_draw_calls: stats.draw.shadow_draw_calls as i32,
        async_compute_queue: renderer.async_compute_queue_supported(),
        async_compute_batches: stats.draw.async_compute_batches,
        vsm: VsmStatsDto {
            requested: stats.vsm.requested as i32,
            hits: stats.vsm.hits as i32,
            allocated: stats.vsm.allocated as i32,
            rendered: stats.vsm.rendered as i32,
            dirtied: stats.vsm.dirtied as i32,
            evicted: stats.vsm.evicted as i32,
            overflow: stats.vsm.overflow as i32,
            directional: vsm_family_stats_dto(stats.vsm.directional),
            spot: vsm_family_stats_dto(stats.vsm.spot),
            point: vsm_family_stats_dto(stats.vsm.point),
            requested_bootstrap: stats.vsm.requested_bootstrap as i32,
            requested_projective: stats.vsm.requested_projective as i32,
            requested_receiver: stats.vsm.requested_receiver as i32,
            dirtied_restaged: stats.vsm.dirtied_restaged as i32,
            dirtied_dynamic: stats.vsm.dirtied_dynamic as i32,
            dirtied_moved: stats.vsm.dirtied_moved as i32,
            invalidated_directional_window: stats.vsm.invalidated_directional_window as i32,
            invalidated_light_transform: stats.vsm.invalidated_light_transform as i32,
        },
        rt_instances: stats.rt_instances as i32,
        rt_aggregate_instances: stats.rt_aggregate_instances as i32,
        rt_resolvable_instances: stats.rt_resolvable_instances as i32,
        frame_ms: stats.frame_ms,
        fps: stats.fps,
        gpu_ms: stats.gpu_ms,
        cpu_frame_ms: stats.cpu_frame_ms,
        gpu_frame_ms: stats.gpu_ms,
        cpu_wait_ms: stats.cpu_wait_ms,
        triangles: stats.draw.triangles as i32,
        descriptor_binds: stats.draw.descriptor_binds as i32,
        command_buffers: stats.draw.command_buffers as i32,
        queue_submits: stats.draw.queue_submits as i32,
        pipelines_created: stats.draw.pipelines_created as i32,
        vram_usage_bytes: stats.vram_usage_bytes,
        vram_budget_bytes: stats.vram_budget_bytes,
        software_gpu: stats.software_gpu,
        profiler_mode: profiler_mode_to_dto(stats.profiler_mode),
        clustered: renderer.clustered_enabled(),
        depth_prepass: renderer.depth_prepass_enabled(),
        shadows: renderer.shadows_enabled(),
        ibl: renderer.ibl_enabled(),
        ssao: renderer.ssao_enabled(),
        contact_shadows: renderer.contact_shadows_enabled(),
        ssgi: renderer.ssgi_enabled(),
        render_scale: renderer.render_scale(),
        sky_occlusion: renderer.sky_occlusion_enabled(),
        gdf: renderer.gdf_enabled(),
        quality: renderer.render_quality_tier(),
        tonemap: renderer.tonemap_mode(),
        idle: renderer.reactive_idle(),
        converged: renderer.reactive_converged(),
        redraw_reasons: renderer.redraw_reasons(),
        power_state: renderer.power_state(),
        ddgi: renderer.ddgi_enabled(),
        rt_supported: renderer.rt_supported(),
        rt_shadows: renderer.rt_shadows_enabled(),
        restir: renderer.restir_enabled(),
        ssr: renderer.ssr_enabled(),
        rt_reflections: renderer.rt_reflections_enabled(),
        mesh_shader: renderer.mesh_shader_supported(),
        mesh_executor: renderer.mesh_executor_active(),
        sdf_instances_dropped: renderer.sdf_instances_dropped() as i32,
        sdf_instances_culled: renderer.sdf_instances_culled() as i32,
        rt_instances_culled: renderer.rt_instances_culled() as i32,
        omm_supported: renderer.rt_omm_supported(),
        blas_count: renderer.rt_blas_count() as i32,
        skinned_blas_count: renderer.rt_skinned_blas_count() as i32,
        tessellated_blas_count: renderer.rt_tessellated_blas_count() as i32,
        wind_deformed_instances: renderer.rt_wind_deformed() as i32,
        cluster_as_supported: renderer.cluster_as_supported(),
        cluster_blas_count: renderer.rt_cluster_blas_count() as i32,
        clas_count: renderer.rt_clas_count() as i32,
        ptlas_supported: renderer.ptlas_supported(),
        ptlas_partitions: renderer.rt_ptlas_ops().0 as i32,
        ptlas_writes: renderer.rt_ptlas_ops().1 as i32,
        ptlas_updates: renderer.rt_ptlas_ops().2 as i32,
        accel_build_us: renderer.rt_accel_build_us().to_string(),
        omm_micromaps: renderer.rt_omm_micromaps() as i32,
        omm_opaque: renderer.rt_omm_classes().0.to_string(),
        omm_transparent: renderer.rt_omm_classes().1.to_string(),
        omm_unknown: renderer.rt_omm_classes().2.to_string(),
        omm_derived_micromaps: renderer.rt_omm_derived().0 as i32,
        omm_derived_opaque: renderer.rt_omm_derived().1.to_string(),
        omm_derived_transparent: renderer.rt_omm_derived().2.to_string(),
        omm_derived_unknown: renderer.rt_omm_derived().3.to_string(),
        blas_bytes: renderer.rt_blas_bytes().to_string(),
        blas_built_bytes: renderer.rt_blas_built_bytes().to_string(),
        tlas_bytes: renderer.rt_tlas_bytes().to_string(),
        rt_scratch_bytes: renderer.rt_scratch_bytes().to_string(),
        pipelines: renderer.pipeline_count() as i32,
        bindless_textures: renderer.bindless_texture_count() as i32,
        bindless_free: renderer.bindless_free_count() as i32,
        hdr: true,
        exposure_ev: stats.exposure_ev,
        color_grading: renderer.color_grading(),
        creative_lut: renderer.creative_lut().map(|(asset, size, intensity)| {
            saffron_protocol::CreativeLutStat {
                asset: Uuid(asset),
                intensity,
                size,
            }
        }),
        bloom_enabled: renderer.bloom_enabled(),
        bloom_intensity: renderer.bloom_intensity(),
        bloom_scatter: renderer.bloom_scatter(),
        bloom_tint: renderer.bloom_tint(),
        bloom_threshold: renderer.bloom_threshold(),
        bloom_dirt_texture: Uuid(renderer.bloom_dirt_texture()),
        bloom_dirt_intensity: renderer.bloom_dirt_intensity(),
        bloom_dirt_tint: renderer.bloom_dirt_tint(),
        bloom_anamorphic: AnamorphicParams {
            enabled: renderer.bloom_anamorphic_enabled(),
            ratio: renderer.bloom_anamorphic_ratio(),
            tint: renderer.bloom_anamorphic_tint(),
            intensity: renderer.bloom_anamorphic_intensity(),
        },
        bloom_per_mip_tint: renderer.bloom_mip_tint(),
        aa: aa_mode_from_name(&renderer.aa_mode()),
        view_mode: view_mode_to_dto(stats.view_mode),
    }
}

fn vsm_family_stats_dto(stats: saffron_rendering::VsmPageFamilyCounters) -> VsmPageFamilyStatsDto {
    VsmPageFamilyStatsDto {
        requested: stats.requested as i32,
        hits: stats.hits as i32,
        allocated: stats.allocated as i32,
        rendered: stats.rendered as i32,
        dirtied: stats.dirtied as i32,
        invalidated: stats.invalidated as i32,
        evicted: stats.evicted as i32,
        overflow: stats.overflow as i32,
    }
}

pub(crate) fn pass_timings_dto(renderer: &dyn ControlRenderer) -> RenderPassTimingsDto {
    let passes: Vec<RenderPassTimingDto> = renderer
        .pass_timings()
        .iter()
        .map(|t: &PassTiming| RenderPassTimingDto {
            name: t.name.clone(),
            gpu_ms: t.gpu_ms,
        })
        .collect();
    RenderPassTimingsDto {
        passes,
        gpu_total_ms: renderer.pass_timings_total_ms(),
        software_gpu: renderer.software_gpu(),
        profiler_mode: profiler_mode_to_dto(renderer.profiler_mode()),
    }
}

/// Registers the draw-counter and GPU-scene statistics commands.
pub(crate) fn register_stats(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, RenderStatsDto>(
        "render-stats",
        "last frame's scene draw counters",
        |ctx, _params| Ok(render_stats_dto(ctx.renderer)),
    );

    reg.register::<
        saffron_protocol::EmitInteractionImpulseParams,
        saffron_protocol::EmitInteractionImpulseResult,
    >(
        "emit-interaction-impulse",
        "emit-interaction-impulse {positionM, radiusM, strength, direction?, depress?} — push the world interaction field",
        |ctx, params| {
            let finite = params.position_m.iter().all(|v| v.is_finite())
                && params.radius_m.is_finite()
                && params.strength.is_finite()
                && params.direction.is_none_or(|d| d.iter().all(|v| v.is_finite()))
                && params.depress.is_none_or(f64::is_finite);
            if !finite {
                return Err(crate::Error::Command(
                    "impulse fields must be finite".into(),
                ));
            }
            if !(0.01..=64.0).contains(&params.radius_m) {
                return Err(crate::Error::Command(
                    "radiusM must be within 0.01..=64".into(),
                ));
            }
            if !(0.0..=50.0).contains(&params.strength) {
                return Err(crate::Error::Command(
                    "strength must be within 0..=50".into(),
                ));
            }
            let depress = params.depress.unwrap_or(0.0);
            if !(0.0..=10.0).contains(&depress) {
                return Err(crate::Error::Command(
                    "depress must be within 0..=10".into(),
                ));
            }
            let direction = params.direction.unwrap_or([0.0, 0.0]);
            ctx.renderer
                .submit_interaction_impulse(saffron_rendering::InteractionImpulse {
                    position: [params.position_m[0] as f32, params.position_m[1] as f32],
                    radius: params.radius_m as f32,
                    strength: params.strength as f32,
                    direction: [direction[0] as f32, direction[1] as f32],
                    depress: depress as f32,
                    reserved: 0.0,
                });
            Ok(saffron_protocol::EmitInteractionImpulseResult { accepted: true })
        },
    );

    reg.register::<
        saffron_protocol::WindInteractionFieldParams,
        saffron_protocol::WindInteractionFieldResult,
    >(
        "wind-interaction-field",
        "wind-interaction-field {cascade?, resolution?} — one whole cascade of the world interaction field, reduced to a grid",
        |ctx, params| {
            let resolution = params.resolution.unwrap_or(32);
            if !(1..=256).contains(&resolution) {
                return Err(crate::Error::Command(
                    "resolution must be within 1..=256".into(),
                ));
            }
            if params.cascade >= saffron_rendering::GPU_INTERACTION_CASCADES {
                return Err(crate::Error::Command(format!(
                    "cascade must be below {}",
                    saffron_rendering::GPU_INTERACTION_CASCADES
                )));
            }
            let capture = ctx
                .renderer
                .capture_interaction_field(params.cascade, resolution)
                .map_err(crate::Error::Command)?
                .ok_or_else(|| {
                    crate::Error::Command("no frame has created the interaction field yet".into())
                })?;
            Ok(saffron_protocol::WindInteractionFieldResult {
                cascade: capture.cascade,
                texel_meters: capture.texel_meters,
                center_texel: capture.center_texel,
                generation: capture.generation,
                resolution: capture.resolution,
                cells: capture.cells,
                live_texels: capture.live_texels,
                peak_displacement_m: capture.peak_displacement_m,
                peak_velocity_mps: capture.peak_velocity_mps,
            })
        },
    );

    reg.register::<EmptyParams, saffron_protocol::VegetationRenderStatsDto>(
        "vegetation-render-stats",
        "per-family and per-cell vegetation render population plus page faults",
        |ctx, _params| {
            let breakdown = ctx.renderer.vegetation_breakdown();
            Ok(saffron_protocol::VegetationRenderStatsDto {
                families: breakdown
                    .families
                    .into_iter()
                    .map(|row| saffron_protocol::VegetationFamilyRenderDto {
                        family: saffron_protocol::Uuid(row.family),
                        instances: row.instances,
                        field_tiles: row.field_tiles,
                        micro_predicted: row.micro_predicted,
                    })
                    .collect(),
                cells: breakdown
                    .cells
                    .into_iter()
                    .map(|row| saffron_protocol::VegetationCellRenderDto {
                        cell: crate::vegetation_cook_dto::world_cell_dto(row.cell),
                        plants: row.plants,
                        field_tiles: row.field_tiles,
                    })
                    .collect(),
                page_faults: ctx.renderer.page_faults().to_string(),
            })
        },
    );

    reg.register::<EmptyParams, GpuSceneMirrorStatsDto>(
        "gpu-scene-stats",
        "persistent GPU-scene mirror population and rebuild counters",
        |ctx, _params| {
            let stats = ctx.renderer.gpu_scene_mirror_stats();
            let residency = ctx.renderer.page_residency_stats();
            Ok(GpuSceneMirrorStatsDto {
                history_invalidation: ctx.renderer.view_history_invalidation().to_owned(),
                meshes: stats.meshes as u32,
                materials: stats.materials as u32,
                textures: stats.textures as u32,
                instances: stats.instances as u32,
                lights: stats.lights as u32,
                unresolved_instances: stats.unresolved_instances as u32,
                retained_mesh_bytes: stats.retained_mesh_bytes,
                shared_rebuilds: stats.shared_rebuilds,
                world_rebuilds: stats.world_rebuilds,
                micro_predicted: stats.micro_predicted,
                page_residency: saffron_protocol::PageResidencyStatsDto {
                    registered: residency.registered,
                    resident: residency.resident,
                    resident_bytes: residency.resident_bytes,
                    budget_bytes: residency.budget_bytes,
                    requested: residency.requested,
                    loading: residency.loading,
                    ready: residency.ready,
                    evictions: residency.evictions,
                    faults: residency.faults,
                    fault_latency_us: residency.fault_latency_us,
                    requests_dropped: residency.requests_dropped,
                    request_overflow_classes: residency.request_overflow_classes,
                },
                visibility: {
                    let words = ctx.renderer.visibility_counters();
                    let gi = ctx.renderer.gi_visibility_counters();
                    saffron_protocol::SceneVisibilityStatsDto {
                        visible: words[0],
                        retested: words[1],
                        records: words[3],
                        transparent: words[5],
                        micro_candidates: words[9],
                        transitioning: words[10],
                        voxel_records: words[11],
                        max_cut_depth: words[12],
                        culled_frustum: words[13],
                        culled_occlusion: words[14],
                        sub_quad_triangles: words[15],
                        visited_nodes: words
                            [saffron_rendering::SCENE_VISIBILITY_COUNTER_VISITED_NODES],
                        culled_nodes: words
                            [saffron_rendering::SCENE_VISIBILITY_COUNTER_CULLED_NODES],
                        culled_clusters: words
                            [saffron_rendering::SCENE_VISIBILITY_COUNTER_CULLED_CLUSTERS],
                        gi_reach_visible: gi[saffron_rendering::SCENE_VISIBILITY_COUNTER_VISIBLE],
                        gi_reach_culled: gi
                            [saffron_rendering::SCENE_VISIBILITY_COUNTER_CULLED_REACH],
                        bins: words[saffron_rendering::SCENE_VISIBILITY_COUNTER_BINS],
                        deformed: words[saffron_rendering::SCENE_VISIBILITY_COUNTER_DEFORMED],
                        interaction_resets: ctx.renderer.wind_interaction_resets(),
                        covered_samples: words
                            [saffron_rendering::SCENE_VISIBILITY_COUNTER_COVERED_SAMPLES],
                        overflow_flags: words[2],
                        pressure_flags: words[4],
                    }
                },
            })
        },
    );
}
