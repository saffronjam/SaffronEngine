//! The übershader PSO cache: a typed [`PsoKey`] selects a cached [`Pipeline`], built on first
//! request and returned as a shared [`Arc`].

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ash::vk;

use crate::descriptors::Descriptors;
use crate::gpu_types::Material;
use crate::resources::{DeviceResources, Pipeline};
use crate::{Device, Error, Result, checked};

/// The offscreen color attachment format every mesh PSO renders into.
pub const OFFSCREEN_COLOR_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// The depth attachment format.
pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

/// Which screen-space compute PSO slot a request memoizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScreenCompute {
    Gtao,
    AoBlur,
    Contact,
    Ssgi,
    SsgiBlur,
    SsgiAccum,
    DfaoAccum,
    Ssr,
    CopyColor,
    DdgiBlendIrr,
    DdgiBlendDist,
    DdgiBorder,
    RestirInitial,
    RestirReuse,
    GiResolve,
}

/// The typed mesh-PSO cache key: the full übershader permutation set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PsoKey {
    /// The shader the PSO compiles (the übershader, or a codegen'd material `.spv`).
    pub shader: String,
    /// The unlit fragment permutation (a distinct spec-constant value).
    pub unlit: bool,
    /// The wireframe rasterizer permutation (`PolygonMode::LINE`).
    pub wireframe: bool,
    /// The translucent permutation: straight-alpha blending, depth-write off.
    pub blend: bool,
    /// The alpha-to-coverage permutation. Derived as `masked && sample_count > 1`, so a masked
    /// material at 1× shares the opaque PSO.
    pub alpha_to_coverage: bool,
    /// The MSAA sample count the PSO's multisample state matches.
    pub sample_count: vk::SampleCountFlags,
    /// The executor variant reached through a mesh stage instead of a vertex stage; gated on
    /// [`crate::Capabilities::mesh_shader`] by the caller.
    pub mesh_shader: bool,
}

/// The übershader PSO cache plus the lazily-built compute and preview pipelines.
pub struct Pipelines {
    resources: Arc<DeviceResources>,
    shader_dir: PathBuf,
    /// The set layouts every mesh PSO's pipeline layout binds.
    set_layouts: Vec<vk::DescriptorSetLayout>,
    /// Whether the device supports ray tracing (sets 6/7 present in the mesh layout). When
    /// false, the übershader loads its `_nort` variant so its declared descriptor interface
    /// matches the RT-less layout — strict argument-buffer backends (MoltenVK) require the match.
    rt_enabled: bool,
    fill_mode_non_solid: bool,
    sample_count: vk::SampleCountFlags,

    cache: HashMap<PsoKey, Arc<Pipeline>>,
    pipelines_created: u32,

    /// The clustered light-cull compute PSO.
    light_cull: Option<Arc<Pipeline>>,

    /// The compute skinning PSO (skin set layout, a 16-byte push).
    skin: Option<Arc<Pipeline>>,

    /// The compute morph PSO (morph set layout, a 20-byte push).
    morph: Option<Arc<Pipeline>>,

    /// The adaptive-tessellation compute PSOs. `tess_factor` binds the shared bindless set 0 (its
    /// min/max pyramid tap) + the edge storage set 1; scan / finalize / args each take a single
    /// storage-buffer set.
    tess_factor: Option<Arc<Pipeline>>,
    tess_scan: Option<Arc<Pipeline>>,
    tess_finalize: Option<Arc<Pipeline>>,
    tess_args: Option<Arc<Pipeline>>,

    /// The amplifying emit PSO (bindless set 0 + the emit set 1, a 64-byte push).
    tessellate: Option<Arc<Pipeline>>,

    /// The cluster compute set layout the cull PSO binds (set 0).
    cluster_set_layout: vk::DescriptorSetLayout,

    /// The single-set screen-space compute PSOs.
    gtao: Option<Arc<Pipeline>>,
    ao_blur: Option<Arc<Pipeline>>,
    contact: Option<Arc<Pipeline>>,
    ssgi: Option<Arc<Pipeline>>,
    ssgi_blur: Option<Arc<Pipeline>>,
    ssgi_accum: Option<Arc<Pipeline>>,
    /// The DFAO temporal-accumulation compute PSO: a clamp-free EMA, because DFAO sky visibility is
    /// a rotating low-frequency Monte-Carlo estimate that must be averaged rather than
    /// neighborhood-clamped to converge on a static surface.
    dfao_accum: Option<Arc<Pipeline>>,
    ssr: Option<Arc<Pipeline>>,
    copy_color: Option<Arc<Pipeline>>,

    /// The four DDGI compute PSOs. The trace is a three-set pipeline (bindless bricks + the light
    /// set + the DDGI trace set), so it sits outside the single-set `ScreenCompute` table.
    ddgi_trace: Option<Arc<Pipeline>>,
    ddgi_blend_irr: Option<Arc<Pipeline>>,
    ddgi_blend_dist: Option<Arc<Pipeline>>,
    ddgi_border: Option<Arc<Pipeline>>,

    /// The DFAO sky-visibility cone-trace PSO: bindless bricks + the light set + this pass's I/O
    /// set. Blurred by `ssgi_blur`, accumulated by the clamp-free `dfao_accum`.
    dfao: Option<Arc<Pipeline>>,

    /// The specular reflection-occlusion cone-trace PSO, shaped like [`Pipelines::dfao`] but with a
    /// compute3 I/O set (G-buffer + roughness samplers plus the storage image). Spatial-only: the
    /// view-dependent reflection term is not temporally reprojected, since surface motion smears
    /// it, so there is no accumulation stage.
    specocc: Option<Arc<Pipeline>>,

    /// The two Global-SDF compute PSOs, both two-set pipelines (the bindless brick array set 0 +
    /// the GDF cull/composite set 1).
    gdf_cull: Option<Arc<Pipeline>>,
    gdf_composite: Option<Arc<Pipeline>>,
    gi_occluder_scatter: Option<Arc<Pipeline>>,

    /// The three ReSTIR DI compute PSOs. RT-only, since the resolve needs ray-query.
    restir_initial: Option<Arc<Pipeline>>,
    restir_reuse: Option<Arc<Pipeline>>,
    restir_resolve: Option<Arc<Pipeline>>,
    hzb_copy: Option<Arc<Pipeline>>,
    scene_visibility: Option<Arc<Pipeline>>,
    wind_deform: Option<Arc<Pipeline>>,
    wind_interact: Option<Arc<Pipeline>>,
    vsm_demand: Option<Arc<Pipeline>>,
    vsm_demand_compact: Option<Arc<Pipeline>>,
    scene_traversal: Option<Arc<Pipeline>>,
    scene_bin_count: Option<Arc<Pipeline>>,
    scene_bin_seed: Option<Arc<Pipeline>>,
    scene_bin_scatter: Option<Arc<Pipeline>>,
    scene_micro_count: Option<Arc<Pipeline>>,
    scene_micro_scan: Option<Arc<Pipeline>>,
    scene_micro_scatter: Option<Arc<Pipeline>>,
    depth_prepass_executor: Option<Arc<Pipeline>>,
    shadow_depth_executor: Option<Arc<Pipeline>>,
    gbuffer_executor: Option<Arc<Pipeline>>,
    motion_executor: Option<Arc<Pipeline>>,
    transparent_keys: Option<Arc<Pipeline>>,
    radix_histogram: Option<Arc<Pipeline>>,
    radix_scan: Option<Arc<Pipeline>>,
    radix_scatter: Option<Arc<Pipeline>>,
    transparent_reorder: Option<Arc<Pipeline>>,
    hzb_reduce: Option<Arc<Pipeline>>,
    /// The screen-space indirect-diffuse resolve PSO (`gi_resolve.spv`, the bespoke single set).
    gi_resolve: Option<Arc<Pipeline>>,

    /// The TAA resolve compute PSO (taa-shape set layout + motion depth, a 48-byte push).
    taa: Option<Arc<Pipeline>>,
    /// The FXAA edge-blur compute PSO (fxaa set layout, no push).
    fxaa: Option<Arc<Pipeline>>,

    /// The bloom pyramid compute PSO (bloom set layout, a 32-byte push). One PSO drives all three
    /// passes — downsample / tent-upsample / composite — via the push `pass`/`karis` fields.
    bloom: Option<Arc<Pipeline>>,

    /// The mandatory tonemap compute PSO (tonemap set layout, a 16-byte push).
    tonemap: Option<Arc<Pipeline>>,
    /// The look-bake compute PSO, reusing the tonemap set layout with an 8-byte size+mode push: a
    /// `STORAGE_IMAGE`/UBO/sampler triple binds a 3D output image the same as the 2D tonemap target.
    lut_bake: Option<Arc<Pipeline>>,
    /// The tonemap compute set layout (one storage image) the tonemap PSO binds (set 0).
    tonemap_set_layout: vk::DescriptorSetLayout,
    /// The analytic height-fog composite compute PSO (fog set layout, no push).
    fog: Option<Arc<Pipeline>>,
    /// The froxel volumetric-fog injection compute PSO (mesh light set 0 + fog volume set 1 + a
    /// 64-byte medium push).
    fog_inject: Option<Arc<Pipeline>>,
    /// The froxel volumetric-fog integration compute PSO (fog integrate set, no push).
    fog_integrate: Option<Arc<Pipeline>>,
    /// The aerial-perspective fill compute PSO (atmosphere LUTs + params + storage volume).
    aerial: Option<Arc<Pipeline>>,
    /// The single weather-map resolve compute PSO.
    cloud_weather: Option<Arc<Pipeline>>,
    /// The unlit cloud-density visualization compute PSO, used in CloudDensity view mode.
    cloud_debug: Option<Arc<Pipeline>>,
    /// The adaptive lit cloud raymarch compute PSO.
    cloud_raymarch: Option<Arc<Pipeline>>,
    /// The reduced cloud temporal-reconstruction compute PSO.
    cloud_reconstruct: Option<Arc<Pipeline>>,
    /// The bilateral upscale and HDR cloud-composite compute PSO.
    cloud_upscale: Option<Arc<Pipeline>>,
    /// The cascaded density-integrated cloud-shadow fill PSO.
    cloud_shadow: Option<Arc<Pipeline>>,
    /// The fog compute set layout (offscreen storage + params UBO + depth + sky-view LUT).
    fog_set_layout: vk::DescriptorSetLayout,
    /// The depth-upscale graphics PSO (fullscreen triangle, depth-write-always, one fragment
    /// sampler + an 8-byte inputSize push): point-upscales the input-extent scene depth into the
    /// display-extent overlay depth so the grid / gizmo occlude correctly under upsampling.
    depth_upscale: Option<Arc<Pipeline>>,
    /// The TAA reactive-coverage graphics PSO: mesh vertex + a constant-1.0 fragment into an r8
    /// target, depth-tested read-only against the scene depth, re-drawing the translucent batches
    /// to mark the reactive mask.
    reactive_coverage: Option<Arc<Pipeline>>,
    reactive_transition: Option<Arc<Pipeline>>,
    /// The analytic ground-grid graphics PSO (fullscreen, depth-tested, alpha-blended, 2×mat4 push).
    grid: Option<Arc<Pipeline>>,
    /// The always-on-top editor-overlay graphics PSO over the [`crate::OverlayVertex`] stream.
    overlay: Option<Arc<Pipeline>>,
    /// The depth-tested editor-overlay graphics PSO, so scene geometry occludes the overlay.
    overlay_depth: Option<Arc<Pipeline>>,
    /// The Lit Wireframe overlay graphics PSO (line polygon mode, depth-tested without write);
    /// `None` on a device without `fill_mode_non_solid`.
    wireframe_overlay: Option<Arc<Pipeline>>,
    /// The motion-vector visualization compute PSO (copy_color-shaped set).
    motion_visualize: Option<Arc<Pipeline>>,
}

mod build;
mod build_overlay;
mod effect_requests;
mod raster_requests;
mod scene_requests;

impl Pipelines {
    /// Builds the cache against the device-global descriptor layouts. The shader dir is resolved
    /// once (`SAFFRON_SHADER_DIR`, else the `shaders/` dir beside the running binary).
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        sample_count: vk::SampleCountFlags,
    ) -> Self {
        // The mesh PSO layout must bind every set the SPIR-V references or pipeline creation is
        // invalid: 0 bindless albedo, 1 lights+clusters+shadows, 2 per-instance+joints+mat,
        // 3 IBL+probes, 4 AO+contact+SSGI, 5 DDGI, 6 TLAS (RT), 7 ReSTIR (RT).
        let mut set_layouts = vec![
            descriptors.bindless_set_layout(),
            descriptors.light_set_layout(),
            descriptors.instance_set_layout(),
            descriptors.ibl_set_layout(),
            descriptors.ssao_mesh_set_layout(),
            descriptors.ddgi_mesh_set_layout(),
        ];
        let rt_enabled = matches!(
            (
                descriptors.rt_mesh_set_layout(),
                descriptors.restir_mesh_set_layout(),
            ),
            (Some(_), Some(_))
        );
        if let (Some(rt), Some(restir)) = (
            descriptors.rt_mesh_set_layout(),
            descriptors.restir_mesh_set_layout(),
        ) {
            set_layouts.push(rt);
            set_layouts.push(restir);
        }

        Self {
            resources: Arc::clone(device.resources()),
            shader_dir: resolve_shader_dir(),
            set_layouts,
            rt_enabled,
            fill_mode_non_solid: device.capabilities.fill_mode_non_solid,
            sample_count,
            cache: HashMap::new(),
            pipelines_created: 0,
            light_cull: None,
            skin: None,
            morph: None,
            tess_factor: None,
            tess_scan: None,
            tess_finalize: None,
            tess_args: None,
            tessellate: None,
            cluster_set_layout: descriptors.cluster_set_layout(),
            gtao: None,
            ao_blur: None,
            contact: None,
            ssgi: None,
            ssgi_blur: None,
            ssgi_accum: None,
            dfao_accum: None,
            ssr: None,
            copy_color: None,
            ddgi_trace: None,
            ddgi_blend_irr: None,
            ddgi_blend_dist: None,
            ddgi_border: None,
            dfao: None,
            specocc: None,
            gdf_cull: None,
            gi_occluder_scatter: None,
            gdf_composite: None,
            restir_initial: None,
            restir_reuse: None,
            restir_resolve: None,
            hzb_copy: None,
            scene_visibility: None,
            wind_deform: None,
            wind_interact: None,
            vsm_demand: None,
            vsm_demand_compact: None,
            scene_traversal: None,
            scene_bin_count: None,
            scene_bin_seed: None,
            scene_bin_scatter: None,
            scene_micro_count: None,
            scene_micro_scan: None,
            scene_micro_scatter: None,
            depth_prepass_executor: None,
            shadow_depth_executor: None,
            gbuffer_executor: None,
            motion_executor: None,
            transparent_keys: None,
            radix_histogram: None,
            radix_scan: None,
            radix_scatter: None,
            transparent_reorder: None,
            hzb_reduce: None,
            gi_resolve: None,
            taa: None,
            fxaa: None,
            bloom: None,
            tonemap: None,
            lut_bake: None,
            tonemap_set_layout: descriptors.tonemap_set_layout(),
            fog: None,
            fog_inject: None,
            fog_integrate: None,
            aerial: None,
            cloud_weather: None,
            cloud_debug: None,
            cloud_raymarch: None,
            cloud_reconstruct: None,
            cloud_upscale: None,
            cloud_shadow: None,
            fog_set_layout: descriptors.fog_set_layout(),
            depth_upscale: None,
            reactive_coverage: None,
            reactive_transition: None,
            grid: None,
            overlay: None,
            overlay_depth: None,
            wireframe_overlay: None,
            motion_visualize: None,
        }
    }

    /// Re-targets the sample-count-baked PSOs to a new MSAA sample count, dropping every stale one
    /// so the next request rebuilds it. The caller must have idled the GPU.
    pub fn set_sample_count(&mut self, sample_count: vk::SampleCountFlags) {
        if self.sample_count == sample_count {
            return;
        }
        self.sample_count = sample_count;
        self.cache.clear();
        // The G-buffer / shadow / motion PSOs are always 1×, since they feed post-resolve targets.
        self.depth_prepass_executor = None;
    }

    /// The MSAA sample count the sample-count-baked PSOs currently target.
    pub fn sample_count(&self) -> vk::SampleCountFlags {
        self.sample_count
    }

    /// Number of distinct mesh PSOs the cache holds — inspectable to verify übershader
    /// reuse (many materials, few PSOs).
    pub fn pipeline_count(&self) -> u32 {
        self.cache.len() as u32
    }

    /// Total PSOs ever compiled (the cache only grows, so this equals
    /// [`Pipelines::pipeline_count`]).
    pub fn pipelines_created(&self) -> u32 {
        self.pipelines_created
    }

    /// Builds + caches one screen-space compute PSO, returning the cached `Arc` on a
    /// hit. The `which` slot selects the field to memoize. Returns `None` on a build
    /// failure (logged) — that effect's pass is skipped this frame.
    fn request_screen_compute(
        &mut self,
        which: ScreenCompute,
        shader: &str,
        layout: vk::DescriptorSetLayout,
        push_size: u32,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = self.screen_slot(which) {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(shader, layout, push_size) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                *self.screen_slot_mut(which) = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request {shader}: {err}");
                None
            }
        }
    }

    fn screen_slot(&self, which: ScreenCompute) -> Option<&Arc<Pipeline>> {
        match which {
            ScreenCompute::Gtao => self.gtao.as_ref(),
            ScreenCompute::AoBlur => self.ao_blur.as_ref(),
            ScreenCompute::Contact => self.contact.as_ref(),
            ScreenCompute::Ssgi => self.ssgi.as_ref(),
            ScreenCompute::SsgiBlur => self.ssgi_blur.as_ref(),
            ScreenCompute::SsgiAccum => self.ssgi_accum.as_ref(),
            ScreenCompute::DfaoAccum => self.dfao_accum.as_ref(),
            ScreenCompute::Ssr => self.ssr.as_ref(),
            ScreenCompute::CopyColor => self.copy_color.as_ref(),
            ScreenCompute::DdgiBlendIrr => self.ddgi_blend_irr.as_ref(),
            ScreenCompute::DdgiBlendDist => self.ddgi_blend_dist.as_ref(),
            ScreenCompute::DdgiBorder => self.ddgi_border.as_ref(),
            ScreenCompute::RestirInitial => self.restir_initial.as_ref(),
            ScreenCompute::RestirReuse => self.restir_reuse.as_ref(),
            ScreenCompute::GiResolve => self.gi_resolve.as_ref(),
        }
    }

    fn screen_slot_mut(&mut self, which: ScreenCompute) -> &mut Option<Arc<Pipeline>> {
        match which {
            ScreenCompute::Gtao => &mut self.gtao,
            ScreenCompute::AoBlur => &mut self.ao_blur,
            ScreenCompute::Contact => &mut self.contact,
            ScreenCompute::Ssgi => &mut self.ssgi,
            ScreenCompute::SsgiBlur => &mut self.ssgi_blur,
            ScreenCompute::SsgiAccum => &mut self.ssgi_accum,
            ScreenCompute::DfaoAccum => &mut self.dfao_accum,
            ScreenCompute::Ssr => &mut self.ssr,
            ScreenCompute::CopyColor => &mut self.copy_color,
            ScreenCompute::DdgiBlendIrr => &mut self.ddgi_blend_irr,
            ScreenCompute::DdgiBlendDist => &mut self.ddgi_blend_dist,
            ScreenCompute::DdgiBorder => &mut self.ddgi_border,
            ScreenCompute::RestirInitial => &mut self.restir_initial,
            ScreenCompute::RestirReuse => &mut self.restir_reuse,
            ScreenCompute::GiResolve => &mut self.gi_resolve,
        }
    }

    /// Builds a compute PSO from `shader` over `set_layout` with an optional
    /// compute-stage push of `push_size` bytes (0 = none). Entry point `computeMain`.
    pub(crate) fn build_compute(
        &self,
        shader: &str,
        set_layout: vk::DescriptorSetLayout,
        push_size: u32,
    ) -> Result<Pipeline> {
        self.build_compute_multi(shader, &[set_layout], push_size)
    }

    /// Builds a compute PSO whose layout declares several descriptor sets (the single-set
    /// [`Pipelines::build_compute`] is the one-element case). The DDGI trace binds the bindless
    /// SDF array (set 0) + the light set (set 1) + the DDGI trace set (set 2).
    fn build_compute_multi(
        &self,
        shader: &str,
        set_layouts: &[vk::DescriptorSetLayout],
        push_size: u32,
    ) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module(shader)?;

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(push_size)];
        let mut layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(set_layouts);
        if push_size > 0 {
            layout_info = layout_info.push_constant_ranges(&push_constant);
        }
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = match checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (compute)",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The module was loaded above; freed once here.
                unsafe { raw.destroy_shader_module(module, None) };
                return Err(err);
            }
        };

        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"computeMain");
        let pipeline_info = [vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout)];
        // SAFETY: the ash seam. The create-info outlives the call; on failure both the
        // layout and the module are freed.
        let created = unsafe {
            raw.create_compute_pipelines(vk::PipelineCache::null(), &pipeline_info, None)
        };
        // SAFETY: the ash seam. The module is consumed by creation; free it now.
        unsafe { raw.destroy_shader_module(module, None) };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_compute_pipelines",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Loads a SPIR-V shader module from the runtime shader dir (or an absolute path
    /// for a codegen'd material shader).
    fn load_shader_module(&self, shader: &str) -> Result<vk::ShaderModule> {
        let path = if Path::new(shader).is_absolute() {
            PathBuf::from(shader)
        } else {
            // `shaders/mesh.spv` → `<shader_dir>/mesh.spv`: the dir already *is* the
            // shaders dir, so a `shaders/` prefix is stripped.
            self.shader_dir
                .join(shader.strip_prefix("shaders/").unwrap_or(shader))
        };
        // On a device without ray tracing, prefer the `_nort` variant when one exists (the
        // übershader): it omits the RT descriptor sets the RT-less pipeline layout also omits,
        // so the shader interface matches — required by MoltenVK's argument-buffer backend.
        let path = if self.rt_enabled {
            path
        } else {
            let nort = nort_variant_path(&path);
            if nort.is_file() { nort } else { path }
        };
        let bytes = std::fs::read(&path)
            .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            return Err(Error::ShaderLoad(format!(
                "invalid SPIR-V size for '{}' ({} bytes)",
                path.display(),
                bytes.len()
            )));
        }
        // ash wants the code as `&[u32]`; reinterpret the 4-aligned byte buffer.
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: the ash seam. The code slice outlives the call; the module is freed
        // by the caller after pipeline creation.
        checked(
            unsafe { self.resources.device().create_shader_module(&info, None) },
            "create_shader_module (mesh)",
        )
    }
}

/// The straight-alpha over blend attachment the grid + overlay PSOs share
/// (`srcAlpha`/`1-srcAlpha` color, `one`/`1-srcAlpha` alpha).
fn alpha_blend_attachment() -> vk::PipelineColorBlendAttachmentState {
    vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)
}

/// The `_nort` (ray-tracing-off) sibling of a compiled SPIR-V path: `…/mesh.spv` →
/// `…/mesh_nort.spv`. Only the übershader emits one; for any other path the sibling does not exist
/// and the caller falls back to the base file.
fn nort_variant_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("spv");
    path.with_file_name(format!("{stem}_nort.{ext}"))
}

/// The runtime shader directory: the `SAFFRON_SHADER_DIR` override, else the `shaders/` dir beside
/// the running binary, else walking up from the binary to find one — the test binary runs from
/// `target/<profile>/deps/`, one level below the `shaders/` the xtask emits.
pub(crate) fn resolve_shader_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAFFRON_SHADER_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        if let Some(executable_dir) = exe.parent() {
            let bundled = executable_dir.join("..").join("Resources").join("shaders");
            if bundled.is_dir() {
                return bundled;
            }
        }
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(candidate) = dir {
            let shaders = candidate.join("shaders");
            if shaders.is_dir() {
                return shaders;
            }
            dir = candidate.parent().map(Path::to_path_buf);
        }
    }
    PathBuf::from("shaders")
}

#[cfg(test)]
mod tests;
