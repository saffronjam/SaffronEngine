use saffron_protocol::{
    AaModeDto, AnamorphicParams, EmptyParams, GetTaaParamsResult, GetUpscaleResult, GiModeDto,
    RenderQualityResult, SetAaParams, SetAaResult, SetBloomParams, SetBloomResult,
    SetClusteredResult, SetColorGradingParams, SetColorGradingResult, SetDepthPrepassResult,
    SetDisplacementResult, SetExposureParams, SetExposureResult, SetGdfResult, SetGiParams,
    SetGiResult, SetIblResult, SetRenderQualityParams, SetRestirResult, SetRtReflectionsResult,
    SetRtShadowsResult, SetShadowsResult, SetSkinningResult, SetSkyOcclusionResult, SetSsrResult,
    SetTaaParamsParams, SetTaaParamsResult, SetTessellationQualityParams,
    SetTessellationQualityResult, SetTonemapParams, SetUpscaleParams, SetUpscaleResult,
    SetViewModeParams, SetViewModeResult, SetViewportPowerStateParams, SetViewportSizeParams,
    SetViewportSizeResult, ToggleParams, TonemapResult, UpscaleDto, Uuid, Vec3, ViewModeDto,
    ViewportNativeInfoResult, ViewportPowerStateResult,
};
use saffron_rendering::{ViewId, ViewMode};

use crate::error::Error;
use crate::registry::{CommandRegistry, ControlRenderer, EngineContext};
use crate::server::control_socket_path;

/// Converts a glam world vector into the wire `Vec3`.
/// Builds the `RenderQualityResult` reply from the renderer's current tier + resolved per-effect
/// state (shared by `set-render-quality` and `get-render-quality`).
pub(crate) fn render_quality_result(ctx: &EngineContext<'_>) -> RenderQualityResult {
    RenderQualityResult {
        tier: ctx.renderer.render_quality_tier(),
        ssgi: ctx.renderer.ssgi_enabled(),
        gtao: ctx.renderer.ssao_enabled(),
        contact_shadows: ctx.renderer.contact_shadows_enabled(),
    }
}

pub(crate) fn to_vec3(v: saffron_geometry::glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Maps the wire AA mode to a `(samples, fxaa, taa)` selection.
pub(crate) fn aa_selection(mode: AaModeDto) -> (u32, bool, bool) {
    match mode {
        AaModeDto::Off => (1, false, false),
        AaModeDto::Fxaa => (1, true, false),
        AaModeDto::Taa => (1, false, true),
        AaModeDto::Msaa2 => (2, false, false),
        AaModeDto::Msaa4 => (4, false, false),
        AaModeDto::Msaa8 => (8, false, false),
    }
}

/// The AA mode read back from the renderer's mode name.
pub(crate) fn aa_mode_from_name(name: &str) -> AaModeDto {
    match name {
        "fxaa" => AaModeDto::Fxaa,
        "taa" => AaModeDto::Taa,
        "msaa2" => AaModeDto::Msaa2,
        "msaa4" => AaModeDto::Msaa4,
        "msaa8" => AaModeDto::Msaa8,
        _ => AaModeDto::Off,
    }
}

pub(crate) fn view_mode_to_dto(mode: ViewMode) -> ViewModeDto {
    match mode {
        ViewMode::Lit => ViewModeDto::Lit,
        ViewMode::Unlit => ViewModeDto::Unlit,
        ViewMode::Wireframe => ViewModeDto::Wireframe,
        ViewMode::LitWireframe => ViewModeDto::LitWireframe,
        ViewMode::DetailLighting => ViewModeDto::DetailLighting,
        ViewMode::LightingOnly => ViewModeDto::LightingOnly,
        ViewMode::Reflections => ViewModeDto::Reflections,
        ViewMode::Albedo => ViewModeDto::Albedo,
        ViewMode::Normal => ViewModeDto::Normal,
        ViewMode::Roughness => ViewModeDto::Roughness,
        ViewMode::Metallic => ViewModeDto::Metallic,
        ViewMode::Emissive => ViewModeDto::Emissive,
        ViewMode::Depth => ViewModeDto::Depth,
        ViewMode::AmbientOcclusion => ViewModeDto::AmbientOcclusion,
        ViewMode::Gi => ViewModeDto::Gi,
        ViewMode::LightComplexity => ViewModeDto::LightComplexity,
        ViewMode::MotionVectors => ViewModeDto::MotionVectors,
        ViewMode::Fog => ViewModeDto::Fog,
        ViewMode::CloudDensity => ViewModeDto::CloudDensity,
        ViewMode::ShadowPages => ViewModeDto::ShadowPages,
    }
}

pub(crate) fn view_mode_from_dto(mode: ViewModeDto) -> ViewMode {
    match mode {
        ViewModeDto::Lit => ViewMode::Lit,
        ViewModeDto::Unlit => ViewMode::Unlit,
        ViewModeDto::Wireframe => ViewMode::Wireframe,
        ViewModeDto::LitWireframe => ViewMode::LitWireframe,
        ViewModeDto::DetailLighting => ViewMode::DetailLighting,
        ViewModeDto::LightingOnly => ViewMode::LightingOnly,
        ViewModeDto::Reflections => ViewMode::Reflections,
        ViewModeDto::Albedo => ViewMode::Albedo,
        ViewModeDto::Normal => ViewMode::Normal,
        ViewModeDto::Roughness => ViewMode::Roughness,
        ViewModeDto::Metallic => ViewMode::Metallic,
        ViewModeDto::Emissive => ViewMode::Emissive,
        ViewModeDto::Depth => ViewMode::Depth,
        ViewModeDto::AmbientOcclusion => ViewMode::AmbientOcclusion,
        ViewModeDto::Gi => ViewMode::Gi,
        ViewModeDto::LightComplexity => ViewMode::LightComplexity,
        ViewModeDto::MotionVectors => ViewMode::MotionVectors,
        ViewModeDto::Fog => ViewMode::Fog,
        ViewModeDto::CloudDensity => ViewMode::CloudDensity,
        ViewModeDto::ShadowPages => ViewMode::ShadowPages,
    }
}

/// Builds the `render-stats` DTO from the renderer's full snapshot plus its individual
/// toggle queries.
/// Echoes the applied grade back as the flat `set-color-grading` result (the same shape the panel
/// reads from `render-stats`).
pub(crate) fn result_from_grading(p: SetColorGradingParams) -> SetColorGradingResult {
    SetColorGradingResult {
        temperature: p.temperature,
        tint: p.tint,
        contrast: p.contrast,
        pivot: p.pivot,
        saturation: p.saturation,
        slope: p.slope,
        offset: p.offset,
        power: p.power,
        shadows: p.shadows,
        midtones: p.midtones,
        highlights: p.highlights,
        shadows_max: p.shadows_max,
        highlights_min: p.highlights_min,
        channel_mixer: p.channel_mixer,
        split_tone: p.split_tone,
        creative_lut_asset: p.creative_lut_asset,
        creative_lut_intensity: p.creative_lut_intensity,
    }
}

/// The live TAAU upscale surface off the renderer: the fixed ratio, the dynamic-resolution toggle +
/// budget (from the perf config), and the current input/display extents. A partial `set-upscale`
/// merges onto this — reading it back through the same helper keeps the two in one shape.
pub(crate) fn upscale_dto(renderer: &dyn ControlRenderer) -> UpscaleDto {
    let config = renderer.perf_config();
    let (input_width, input_height) = renderer.input_extent();
    let (display_width, display_height) = renderer.display_extent();
    UpscaleDto {
        ratio: renderer.render_scale(),
        dynamic: config.auto_quality,
        target_ms: config.budget_ms(),
        input_width,
        input_height,
        display_width,
        display_height,
    }
}

/// Registers the render-scale read/write pair.
pub(crate) fn register_upscale(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, GetUpscaleResult>(
        "get-upscale",
        "get-upscale — the TAAU ratio, dynamic-resolution state, and input/display extents",
        |ctx, _params| {
            Ok(GetUpscaleResult {
                upscale: upscale_dto(ctx.renderer),
            })
        },
    );

    reg.register::<SetUpscaleParams, SetUpscaleResult>(
        "set-upscale",
        "set-upscale {ratio,dynamic,targetMs} — the TAAU input:display scale + dynamic resolution",
        |ctx, params| {
            if let Some(ratio) = params.ratio {
                ctx.renderer.set_render_scale(ratio);
            }
            let mut config = ctx.renderer.perf_config();
            if let Some(dynamic) = params.dynamic {
                config.auto_quality = dynamic;
            }
            if let Some(ms) = params.target_ms {
                // targetMs <= 0 means uncapped (target_fps = 0), matching the budget controller's
                // budget_ms <= 0 hold.
                config.target_fps = if ms > 0.0 { 1000.0 / ms } else { 0.0 };
            }
            ctx.renderer.set_perf_config(config);
            Ok(SetUpscaleResult {
                upscale: upscale_dto(ctx.renderer),
            })
        },
    );
}

/// Registers the anti-aliasing, view-mode, quality, tonemap, ray-tracing, grading, and viewport commands.
pub(crate) fn register_toggles(reg: &mut CommandRegistry) {
    reg.register::<SetAaParams, SetAaResult>(
        "set-aa",
        "set-aa {off|fxaa|taa|msaa2|msaa4|msaa8} — anti-aliasing mode",
        |ctx, params| {
            let (samples, fxaa, taa) = aa_selection(params.mode.unwrap_or(AaModeDto::Off));
            ctx.renderer
                .set_aa(samples, fxaa, taa)
                .map_err(Error::Command)?;
            Ok(SetAaResult {
                aa: aa_mode_from_name(&ctx.renderer.aa_mode()),
            })
        },
    );

    reg.register::<EmptyParams, GetTaaParamsResult>(
        "get-taa-params",
        "get-taa-params — current TAA blend/sharpen parameters",
        |ctx, _params| {
            Ok(GetTaaParamsResult {
                params: ctx.renderer.taa_params(),
            })
        },
    );

    reg.register::<SetTaaParamsParams, SetTaaParamsResult>(
        "set-taa-params",
        "set-taa-params {feedbackMin,feedbackMax,velocityRejection,clipGamma,sharpness} — tune TAA (partial update)",
        |ctx, p| {
            // Partial update: read current, overlay only the provided fields, write back.
            let mut cur = ctx.renderer.taa_params();
            if let Some(v) = p.feedback_min {
                cur.feedback_min = v;
            }
            if let Some(v) = p.feedback_max {
                cur.feedback_max = v;
            }
            if let Some(v) = p.velocity_rejection {
                cur.velocity_rejection = v;
            }
            if let Some(v) = p.clip_gamma {
                cur.clip_gamma = v;
            }
            if let Some(v) = p.sharpness {
                cur.sharpness = v;
            }
            ctx.renderer.set_taa_params(cur.clone());
            Ok(SetTaaParamsResult { params: cur })
        },
    );

    reg.register::<SetViewModeParams, SetViewModeResult>(
        "set-view-mode",
        "set-view-mode {lit|unlit|wireframe|lit-wireframe|detail-lighting|lighting-only|reflections|albedo|normal|roughness|metallic|emissive|depth|ambient-occlusion|gi|light-complexity|motion-vectors} — debug render-output (transient)",
        |ctx, params| {
            ctx.renderer
                .set_view_mode(view_mode_from_dto(params.mode.unwrap_or(ViewModeDto::Lit)));
            Ok(SetViewModeResult {
                view_mode: view_mode_to_dto(ctx.renderer.view_mode()),
            })
        },
    );

    reg.register::<ToggleParams, SetClusteredResult>(
        "set-clustered",
        "set-clustered {0|1} — toggle clustered light culling",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_clustered(enabled);
            Ok(SetClusteredResult { clustered: enabled })
        },
    );

    reg.register::<ToggleParams, SetIblResult>(
        "set-ibl",
        "set-ibl {0|1} — toggle image-based ambient (vs flat ambient)",
        |ctx, params| {
            ctx.renderer.set_ibl(params.enabled.unwrap_or(true));
            Ok(SetIblResult {
                ibl: ctx.renderer.ibl_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetSkyOcclusionResult>(
        "set-sky-occlusion",
        "set-sky-occlusion {0|1} — occlude the reflected skybox with the Global SDF reflection-occlusion cone",
        |ctx, params| {
            ctx.renderer
                .set_sky_occlusion(params.enabled.unwrap_or(true));
            Ok(SetSkyOcclusionResult {
                sky_occlusion: ctx.renderer.sky_occlusion_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetGdfResult>(
        "set-gdf",
        "set-gdf {0|1} — composite per-mesh SDFs into the Global Distance Field cascade clipmap \
         (the far-field cone-march tap, O(1) in instance count)",
        |ctx, params| {
            ctx.renderer.set_gdf(params.enabled.unwrap_or(true));
            Ok(SetGdfResult {
                gdf: ctx.renderer.gdf_enabled(),
            })
        },
    );

    reg.register::<SetRenderQualityParams, RenderQualityResult>(
        "set-render-quality",
        "set-render-quality {low|medium|high|ultra} — the SSGI/GTAO/contact-shadow quality knob",
        |ctx, params| {
            if !ctx.renderer.set_render_quality(&params.tier) {
                return Err(Error::command(format!(
                    "unknown render-quality tier '{}' (expected low|medium|high|ultra)",
                    params.tier
                )));
            }
            Ok(render_quality_result(ctx))
        },
    );

    reg.register::<EmptyParams, RenderQualityResult>(
        "get-render-quality",
        "get-render-quality — the active render-quality tier + resolved per-effect state",
        |ctx, _params| Ok(render_quality_result(ctx)),
    );

    reg.register::<SetTonemapParams, TonemapResult>(
        "set-tonemap",
        "set-tonemap {reinhard|aces|agx|pbr-neutral} — the HDR→display tonemap operator",
        |ctx, params| {
            if !ctx.renderer.set_tonemap(&params.mode) {
                return Err(Error::command(format!(
                    "unknown tonemap '{}' (expected reinhard|aces|agx|pbr-neutral)",
                    params.mode
                )));
            }
            Ok(TonemapResult {
                mode: ctx.renderer.tonemap_mode(),
            })
        },
    );

    reg.register::<saffron_protocol::SetHierarchyCutParams, saffron_protocol::HierarchyCutResult>(
        "set-hierarchy-cut",
        "set-hierarchy-cut {auto|coarse|fine} [camera|shadow|gi] — pin one view's \
         hierarchy cut (omit the cut to read)",
        |ctx, params| {
            use saffron_protocol::{HierarchyCutDto, HierarchyCutViewDto};
            let view_dto = params.view.unwrap_or_default();
            let view = match view_dto {
                HierarchyCutViewDto::Camera => saffron_rendering::SceneViewClass::Camera,
                HierarchyCutViewDto::Shadow => saffron_rendering::SceneViewClass::ShadowPage,
                HierarchyCutViewDto::Gi => saffron_rendering::SceneViewClass::Gi,
            };
            if let Some(cut) = params.cut {
                ctx.renderer.set_cut_override(
                    view,
                    match cut {
                        HierarchyCutDto::Auto => saffron_rendering::SCENE_CUT_AUTO,
                        HierarchyCutDto::Coarse => saffron_rendering::SCENE_CUT_FORCE_COARSE,
                        HierarchyCutDto::Fine => saffron_rendering::SCENE_CUT_FORCE_FINE,
                    },
                );
            }
            Ok(saffron_protocol::HierarchyCutResult {
                view: view_dto,
                cut: match ctx.renderer.cut_override(view) {
                    saffron_rendering::SCENE_CUT_FORCE_COARSE => HierarchyCutDto::Coarse,
                    saffron_rendering::SCENE_CUT_FORCE_FINE => HierarchyCutDto::Fine,
                    _ => HierarchyCutDto::Auto,
                },
            })
        },
    );

    reg.register::<saffron_protocol::SetMeshExecutorParams, saffron_protocol::MeshExecutorResult>(
        "set-mesh-executor",
        "set-mesh-executor [true|false] — route the shaded executor through the mesh stage \
         (omit to read)",
        |ctx, params| {
            let enabled = match params.enabled {
                Some(enabled) => ctx.renderer.set_mesh_executor(enabled),
                None => ctx.renderer.mesh_executor_active(),
            };
            Ok(saffron_protocol::MeshExecutorResult {
                enabled,
                supported: ctx.renderer.mesh_executor_supported(),
            })
        },
    );

    reg.register::<saffron_protocol::VsmPageBudgetParams, saffron_protocol::VsmPageBudgetResult>(
        "vsm-page-budget",
        "vsm-page-budget {pages} — shadow pages a frame may render (omit to read)",
        |ctx, params| {
            if let Some(pages) = params.pages {
                ctx.renderer.set_vsm_page_budget(pages);
            }
            Ok(saffron_protocol::VsmPageBudgetResult {
                pages: ctx.renderer.vsm_page_budget(),
            })
        },
    );

    reg.register::<saffron_protocol::PageRequestBudgetParams, saffron_protocol::PageRequestBudgetResult>(
        "page-request-budget",
        "page-request-budget {entries} — missing-page requests one view class may raise \
         per frame (omit to read)",
        |ctx, params| {
            if let Some(entries) = params.entries {
                ctx.renderer.set_page_request_budget(entries);
            }
            Ok(saffron_protocol::PageRequestBudgetResult {
                entries: ctx.renderer.page_request_budget(),
                capacity: saffron_rendering::PAGE_REQUEST_CAPACITY,
            })
        },
    );

    reg.register::<ToggleParams, SetRtShadowsResult>(
        "set-rt-shadows",
        "set-rt-shadows {0|1} — hardware ray-query shadows (if supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer.set_rt_shadows(params.enabled.unwrap_or(true));
            Ok(SetRtShadowsResult {
                rt_shadows: ctx.renderer.rt_shadows_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetRestirResult>(
        "set-restir",
        "set-restir {0|1} — ReSTIR stochastic many-light direct (if RT supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer.set_restir(params.enabled.unwrap_or(true));
            Ok(SetRestirResult {
                restir: ctx.renderer.restir_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetSsrResult>(
        "set-ssr",
        "set-ssr {0|1} — screen-space reflections (sharp mirror reflections on smooth surfaces)",
        |ctx, params| {
            ctx.renderer.set_ssr(params.enabled.unwrap_or(true));
            Ok(SetSsrResult {
                ssr: ctx.renderer.ssr_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetRtReflectionsResult>(
        "set-rt-reflections",
        "set-rt-reflections {0|1} — ray-traced reflections (off-screen-aware, if RT supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer
                .set_rt_reflections(params.enabled.unwrap_or(true));
            Ok(SetRtReflectionsResult {
                rt_reflections: ctx.renderer.rt_reflections_enabled(),
            })
        },
    );

    reg.register::<SetGiParams, SetGiResult>(
        "set-gi",
        "set-gi {off|ddgi} — DDGI probe global illumination (multi-bounce)",
        |ctx, params| {
            ctx.renderer.set_ddgi(params.mode == GiModeDto::Ddgi);
            Ok(SetGiResult {
                ddgi: ctx.renderer.ddgi_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetShadowsResult>(
        "set-shadows",
        "set-shadows {0|1} — toggle the directional shadow map",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_shadows(enabled);
            Ok(SetShadowsResult { shadows: enabled })
        },
    );

    reg.register::<ToggleParams, SetSkinningResult>(
        "set-skinning",
        "set-skinning {0|1} — toggle the GPU skinning path",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_skinning(enabled);
            Ok(SetSkinningResult { skinning: enabled })
        },
    );

    reg.register::<ToggleParams, SetDisplacementResult>(
        "set-displacement",
        "set-displacement {0|1} — toggle the GPU compute-displacement path",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_displacement(enabled);
            Ok(SetDisplacementResult {
                displacement: enabled,
            })
        },
    );

    reg.register::<SetExposureParams, SetExposureResult>(
        "set-exposure",
        "set-exposure {ev} — tonemap exposure in stops (exp2)",
        |ctx, params| {
            ctx.renderer.set_exposure(params.ev);
            Ok(SetExposureResult {
                exposure_ev: ctx.renderer.exposure_ev(),
            })
        },
    );

    reg.register::<SetBloomParams, SetBloomResult>(
        "set-bloom",
        "set-bloom {enabled} {intensity} {scatter} {tint} {threshold} [dirtTexture] [dirtIntensity] \
         [dirtTint] [anamorphic] [perMipTint] — pre-tonemap bloom pyramid + art direction",
        |ctx, p| {
            if !(p.intensity >= 0.0 && (0.0..=1.0).contains(&p.scatter) && p.threshold >= 0.0) {
                return Err(Error::command("bloom parameters out of range"));
            }
            ctx.renderer
                .set_bloom(p.enabled, p.intensity, p.scatter, p.tint, p.threshold);
            // Lens dirt: a supplied `dirtTexture` resolves (id 0 clears) and rebinds the mask; the
            // mix + tint are a separate patch so a caller can tune them without re-sending the asset.
            if let Some(id) = p.dirt_texture {
                let mut resolved = None;
                if id.value() != 0 {
                    let assets = &mut *ctx.assets;
                    let core_id = saffron_core::Uuid(id.value());
                    ctx.renderer.with_gpu_uploader(&mut |gpu| {
                        resolved = assets.load_texture_asset(gpu, core_id);
                    });
                }
                ctx.renderer.set_bloom_dirt_texture(id.value(), resolved);
            }
            if p.dirt_intensity.is_some() || p.dirt_tint.is_some() {
                let intensity = p
                    .dirt_intensity
                    .unwrap_or_else(|| ctx.renderer.bloom_dirt_intensity());
                let tint = p.dirt_tint.unwrap_or_else(|| ctx.renderer.bloom_dirt_tint());
                ctx.renderer.set_bloom_dirt_params(intensity, tint);
            }
            if let Some(a) = &p.anamorphic {
                ctx.renderer
                    .set_bloom_anamorphic(a.enabled, a.ratio, a.tint, a.intensity);
            }
            if let Some(stack) = p.per_mip_tint {
                ctx.renderer.set_bloom_mip_tint(stack);
            }
            Ok(SetBloomResult {
                enabled: ctx.renderer.bloom_enabled(),
                intensity: ctx.renderer.bloom_intensity(),
                scatter: ctx.renderer.bloom_scatter(),
                tint: ctx.renderer.bloom_tint(),
                threshold: ctx.renderer.bloom_threshold(),
                dirt_texture: Uuid(ctx.renderer.bloom_dirt_texture()),
                dirt_intensity: ctx.renderer.bloom_dirt_intensity(),
                dirt_tint: ctx.renderer.bloom_dirt_tint(),
                anamorphic: AnamorphicParams {
                    enabled: ctx.renderer.bloom_anamorphic_enabled(),
                    ratio: ctx.renderer.bloom_anamorphic_ratio(),
                    tint: ctx.renderer.bloom_anamorphic_tint(),
                    intensity: ctx.renderer.bloom_anamorphic_intensity(),
                },
                per_mip_tint: ctx.renderer.bloom_mip_tint(),
            })
        },
    );

    reg.register::<SetColorGradingParams, SetColorGradingResult>(
        "set-color-grading",
        "set-color-grading {temperature} {tint} {contrast} {pivot} {saturation} {slope} {offset} \
         {power} — the scene-linear grade (white balance, contrast, saturation, ASC-CDL)",
        |ctx, params| {
            let finite3 = |v: [f32; 3]| v.iter().all(|c| c.is_finite());
            let range_valid = |r: &saffron_protocol::GradeRangeDto| {
                finite3(r.slope)
                    && finite3(r.offset)
                    && finite3(r.power)
                    && r.power.iter().all(|c| *c > 0.0)
                    && r.saturation.is_finite()
                    && r.saturation >= 0.0
                    && r.contrast.is_finite()
                    && r.contrast >= 0.0
            };
            let valid = params.temperature.is_finite()
                && params.tint.is_finite()
                && params.contrast.is_finite()
                && params.contrast >= 0.0
                && params.saturation.is_finite()
                && params.saturation >= 0.0
                && params.pivot.is_finite()
                && params.pivot > 0.0
                && finite3(params.slope)
                && finite3(params.offset)
                && finite3(params.power)
                && params.power.iter().all(|c| *c > 0.0)
                && range_valid(&params.shadows)
                && range_valid(&params.midtones)
                && range_valid(&params.highlights)
                && params.shadows_max.is_finite()
                && params.highlights_min.is_finite()
                && params.channel_mixer.iter().all(|c| c.is_finite())
                && finite3(params.split_tone.shadow)
                && finite3(params.split_tone.highlight)
                && params.split_tone.balance.is_finite();
            if !valid {
                return Err(Error::command(
                    "color grade out of range (pivot/power > 0, contrast/saturation ≥ 0, all finite)",
                ));
            }
            if !((0.0..=1.0).contains(&params.creative_lut_intensity)) {
                return Err(Error::command("creativeLutIntensity out of range (expected 0..=1)"));
            }
            let lut_id = params.creative_lut_asset.value();
            let lut_intensity = params.creative_lut_intensity;
            ctx.renderer.set_color_grading(params);
            // The creative-LUT slot rides the same command: resolve the asset (id 0 clears to the
            // identity default) and rebind, mirroring the lens-dirt mask. The intensity rides the grade
            // UBO, so an intensity-only change re-resolves the (cached) asset without a descriptor rewrite.
            let mut resolved = None;
            if lut_id != 0 {
                let assets = &mut *ctx.assets;
                let core_id = saffron_core::Uuid(lut_id);
                ctx.renderer.with_gpu_uploader(&mut |gpu| {
                    resolved = assets.load_cube_lut_asset(gpu, core_id);
                });
                if resolved.is_none() {
                    return Err(Error::command(format!(
                        "creative lut asset {lut_id} could not be resolved"
                    )));
                }
            }
            ctx.renderer
                .set_creative_lut_texture(lut_id, resolved, lut_intensity);
            Ok(result_from_grading(ctx.renderer.color_grading()))
        },
    );

    reg.register::<saffron_protocol::BakeLookParams, saffron_protocol::BakeLookResult>(
        "bake-look",
        "bake-look [name] — fold grade + view transform + creative LUT into a 33³ log2-shaper .slut",
        |ctx, params| {
            let name = params.name.unwrap_or_else(|| "Baked Look".to_owned());
            let (size, rgb) = ctx.renderer.bake_look_lut().map_err(Error::command)?;
            let (asset, path) = ctx
                .assets
                .import_baked_lut(&name, size, rgb)
                .map_err(Error::command)?;
            Ok(saffron_protocol::BakeLookResult {
                asset: Uuid(asset.value()),
                path,
                size,
            })
        },
    );

    reg.register::<SetTessellationQualityParams, SetTessellationQualityResult>(
        "set-tessellation-quality",
        "set-tessellation-quality [factorCap] [minFactor] [edgeLengthTarget] — displacement dice budget",
        |ctx, params| {
            ctx.renderer.set_tessellation_quality(
                params.factor_cap,
                params.min_factor,
                params.edge_length_target,
            );
            let (factor_cap, min_factor, edge_length_target) = ctx.renderer.tessellation_quality();
            Ok(SetTessellationQualityResult {
                factor_cap,
                min_factor,
                edge_length_target,
            })
        },
    );

    reg.register::<ToggleParams, SetDepthPrepassResult>(
        "set-depth-prepass",
        "set-depth-prepass {0|1} — toggle the depth pre-pass",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_depth_prepass(enabled);
            Ok(SetDepthPrepassResult {
                depth_prepass: enabled,
            })
        },
    );

    reg.register::<EmptyParams, ViewportNativeInfoResult>(
        "viewport-native-info",
        "native viewport bridge status",
        |ctx, _params| {
            Ok(ViewportNativeInfoResult {
                platform: "linux".to_owned(),
                transport: "wayland-subsurface".to_owned(),
                status: "engine-ready".to_owned(),
                control_socket: control_socket_path(),
                width: ctx.renderer.viewport_width() as i32,
                height: ctx.renderer.viewport_height() as i32,
                message: "engine renders offscreen; the editor presents frames from shared \
                          memory on a wayland subsurface"
                    .to_owned(),
            })
        },
    );

    reg.register::<SetViewportPowerStateParams, ViewportPowerStateResult>(
        "set-viewport-power-state",
        "set-viewport-power-state {state} — editor viewport visibility (focused/unfocused/occluded) \
         for idle throttling; occluded suppresses rendering",
        |ctx, params| {
            if !ctx.renderer.set_viewport_power_state(&params.state) {
                return Err(Error::command(format!(
                    "unknown power state '{}' (expected focused|unfocused|occluded)",
                    params.state
                )));
            }
            Ok(ViewportPowerStateResult {
                state: ctx.renderer.power_state(),
            })
        },
    );

    reg.register::<SetViewportSizeParams, SetViewportSizeResult>(
        "set-viewport-size",
        "set-viewport-size {view, width, height} — set a view's offscreen render size (device pixels)",
        |ctx, params| {
            let wire = params.view.unwrap_or_else(|| "scene".to_owned());
            let view = ViewId::from_wire(&wire)
                .ok_or_else(|| Error::command(format!("unknown view '{wire}'")))?;
            let width = params
                .width
                .unwrap_or(ctx.renderer.viewport_width() as i32)
                .max(1);
            let height = params
                .height
                .unwrap_or(ctx.renderer.viewport_height() as i32)
                .max(1);
            ctx.renderer
                .set_view_desired_size(view, width as u32, height as u32);
            Ok(SetViewportSizeResult { width, height })
        },
    );
}
