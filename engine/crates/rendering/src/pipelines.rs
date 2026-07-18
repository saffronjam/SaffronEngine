//! The übershader PSO cache: one shader backs every renderable, and a typed
//! [`PsoKey`] (unlit / skinned / wireframe / sample-count) selects a cached
//! [`Pipeline`]. [`Pipelines::request_mesh_pipeline`] builds-and-caches on first
//! request and returns the shared [`Arc`].
//!
//! The cache keys by a [`PsoKey`] struct so the key is matchable, not a stringly-typed
//! concat — one cache, one key shape.
//!
//! # Variants
//!
//! - **unlit** — a `vk::Bool32` specialization constant (id 0) baked into the fragment
//!   stage, so one PSO is the lit branch and another the unlit branch.
//! - **skinned** — binds `vertexMainSkinned` and adds the [`VertexSkin`] stream on
//!   binding 1 (joints + weights); the base layout (binding 0) is untouched, so the
//!   unskinned PSO is unchanged.
//! - **wireframe** — `vk::PolygonMode::LINE`, gated on the `fill_mode_non_solid`
//!   capability (a software device may lack it; the request falls back to fill).

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ash::vk;
use saffron_geometry::{Vertex, VertexSkin};

use crate::descriptors::Descriptors;
use crate::gpu_types::Material;
use crate::meshlet_raster::MeshletPush;
use crate::resources::{DeviceResources, Pipeline};
use crate::{Device, Error, Result, checked};

/// The offscreen color attachment format every mesh PSO renders into.
pub const OFFSCREEN_COLOR_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// The depth attachment format.
pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

/// Which screen-space compute PSO slot a request memoizes — the closed set dispatched
/// by [`Pipelines::request_screen_compute`].
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
    RestirResolve,
    GiResolve,
}

/// The typed mesh-PSO cache key. One übershader backs every renderable; this tuple is
/// the full permutation set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PsoKey {
    /// The shader the PSO compiles (the übershader, or a codegen'd material `.spv`).
    pub shader: String,
    /// The unlit fragment permutation (a distinct spec-constant value).
    pub unlit: bool,
    /// The skinned vertex permutation (binds the `VertexSkin` stream).
    pub skinned: bool,
    /// The wireframe rasterizer permutation (`PolygonMode::LINE`).
    pub wireframe: bool,
    /// The translucent permutation: straight-alpha blending enabled, depth-write off (the
    /// opaque permutation writes depth with blending off).
    pub blend: bool,
    /// The alpha-to-coverage permutation (a masked material under MSAA): the multisample state
    /// enables A2C and the fragment sharpens the cutout into per-sample coverage. Derived as
    /// `masked && sample_count > 1`, so a masked material at 1× shares the opaque PSO.
    pub alpha_to_coverage: bool,
    /// The MSAA sample count the PSO's multisample state matches.
    pub sample_count: vk::SampleCountFlags,
}

/// The übershader PSO cache + the lazy thumbnail/preview pipelines.
///
/// Built once in [`Pipelines::new`], then mutated through its own `&mut self` methods
/// (the cache map grows; the lazy pipelines populate on first use). Owns an
/// [`Arc`]`<`[`DeviceResources`]`>` so its handles free without a live `&Device`;
/// every cached [`Pipeline`] is itself a Drop type holding the same `Arc`.
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

    /// The vertex-only depth pre-pass PSO (binds sets 0/1/2, the viewProj push), built
    /// lazily on the first request and reused.
    depth_prepass: Option<Arc<Pipeline>>,

    /// The vertex-only, depth-biased shadow depth PSO (the directional + spot shadow
    /// maps), built lazily.
    shadow_depth: Option<Arc<Pipeline>>,

    /// The color (distance) + depth point-shadow cube-face PSO, built lazily.
    point_shadow: Option<Arc<Pipeline>>,

    /// The clustered light-cull compute PSO, built lazily.
    light_cull: Option<Arc<Pipeline>>,

    /// The compute skinning PSO (skin set layout, a 16-byte push), built lazily.
    skin: Option<Arc<Pipeline>>,

    /// The compute morph PSO (morph set layout, a 20-byte push), built lazily.
    morph: Option<Arc<Pipeline>>,

    /// The adaptive-tessellation Phase-3 compute PSOs (factor / scan / finalize / args), built lazily.
    /// `tess_factor` binds the shared bindless set 0 (its min/max pyramid tap) + the edge storage set 1;
    /// scan / finalize / args each take a single storage-buffer set.
    tess_factor: Option<Arc<Pipeline>>,
    tess_scan: Option<Arc<Pipeline>>,
    tess_finalize: Option<Arc<Pipeline>>,
    tess_args: Option<Arc<Pipeline>>,

    /// The Phase-4 amplifying emit PSO (`tessellate.slang`, bindless set 0 + the emit set 1, a 64-byte
    /// push), built lazily.
    tessellate: Option<Arc<Pipeline>>,

    /// The `VK_EXT_mesh_shader` meshlet raster PSO (task+mesh+`fragmentMain`), built lazily when the
    /// mesh-shader path is enabled. Bakes the MSAA sample count, so `set_sample_count` clears it.
    meshlet: Option<Arc<Pipeline>>,

    /// The cluster compute set layout the cull PSO binds (set 0).
    cluster_set_layout: vk::DescriptorSetLayout,

    /// The thin G-buffer prepass PSO (view normal + view-Z), built lazily on the first
    /// request and reused.
    gbuffer: Option<Arc<Pipeline>>,

    /// The screen-space compute PSOs (gtao / ao-blur / contact / ssgi / ssgi-blur /
    /// ssgi-accum / copy-color), built lazily once their set layouts are known.
    gtao: Option<Arc<Pipeline>>,
    ao_blur: Option<Arc<Pipeline>>,
    contact: Option<Arc<Pipeline>>,
    ssgi: Option<Arc<Pipeline>>,
    ssgi_blur: Option<Arc<Pipeline>>,
    ssgi_accum: Option<Arc<Pipeline>>,
    /// The DFAO temporal-accumulation compute PSO. Its own kernel (`dfao_accum.spv`), a clamp-free
    /// EMA — unlike `ssgi_accum`, DFAO sky visibility is a rotating low-frequency Monte-Carlo
    /// estimate that must be averaged (not neighborhood-clamped) to converge on a static surface.
    dfao_accum: Option<Arc<Pipeline>>,
    ssr: Option<Arc<Pipeline>>,
    copy_color: Option<Arc<Pipeline>>,

    /// The four DDGI compute PSOs (trace / blend-irradiance / blend-distance / border), built
    /// lazily once the DDGI sub-state's set layouts are known. The trace is a three-set pipeline
    /// (the bindless bricks + the light set, via the shared `sdf` module, + the DDGI trace set), so
    /// it carries its own field rather than riding the single-set `ScreenCompute` table.
    ddgi_trace: Option<Arc<Pipeline>>,
    ddgi_blend_irr: Option<Arc<Pipeline>>,
    ddgi_blend_dist: Option<Arc<Pipeline>>,
    ddgi_border: Option<Arc<Pipeline>>,

    /// The DFAO sky-visibility cone-trace PSO — a three-set pipeline (the bindless bricks + the
    /// light set, via the shared `sdf` module for the GDF clipmap, + this pass's I/O set), so it
    /// carries its own field rather than riding the single-set `ScreenCompute` table. The blur
    /// stage reuses the SSGI blur PSO (`ssgi_blur`); the temporal accumulation uses the dedicated
    /// clamp-free `dfao_accum` PSO.
    dfao: Option<Arc<Pipeline>>,

    /// The specular reflection-occlusion cone-trace PSO — a three-set pipeline mirroring
    /// [`Pipelines::dfao`] (the bindless bricks + the light set for the GDF clipmap, via the shared
    /// `sdf` module, + this pass's I/O set), so it carries its own field rather than riding the
    /// single-set `ScreenCompute` table. Its I/O set is the compute3 shape (two samplers: the
    /// G-buffer + roughness, plus the storage image). It is spatial-only: the half-res trace +
    /// bilateral upsample (`ssgi_blur`) denoise it; the view-dependent reflection term is not
    /// temporally reprojected (surface motion would smear it), so there is no accumulation stage.
    specocc: Option<Arc<Pipeline>>,

    /// The two Global-SDF compute PSOs (cull / composite), both two-set pipelines (the bindless
    /// brick array set 0 + the GDF cull/composite set 1), built lazily once those layouts are known.
    gdf_cull: Option<Arc<Pipeline>>,
    gdf_composite: Option<Arc<Pipeline>>,

    /// The three ReSTIR DI compute PSOs (initial candidate sampling / temporal+spatial
    /// reuse / resolve incl. the TLAS visibility ray), built lazily once the device-shared
    /// [`crate::Restir`] set layouts are known. RT-only (the resolve needs ray-query).
    restir_initial: Option<Arc<Pipeline>>,
    restir_reuse: Option<Arc<Pipeline>>,
    restir_resolve: Option<Arc<Pipeline>>,
    /// The screen-space indirect-diffuse resolve PSO (`gi_resolve.spv`, the bespoke single set).
    gi_resolve: Option<Arc<Pipeline>>,

    /// The motion-vector prepass PSO (instanced, depth-tested, rg16f motion), built
    /// lazily.
    motion: Option<Arc<Pipeline>>,
    /// The TAA resolve compute PSO (taa-shape set layout + motion depth, a 48-byte push),
    /// built lazily.
    taa: Option<Arc<Pipeline>>,
    /// The FXAA edge-blur compute PSO (fxaa set layout, no push), built lazily.
    fxaa: Option<Arc<Pipeline>>,

    /// The bloom pyramid compute PSO (bloom set layout, a 32-byte push). One PSO drives all three
    /// passes — downsample / tent-upsample / composite — via the push `pass`/`karis` fields.
    bloom: Option<Arc<Pipeline>>,

    /// The mandatory tonemap compute PSO (tonemap set layout, an 8-byte exposure+mode push),
    /// built lazily.
    tonemap: Option<Arc<Pipeline>>,
    /// The look-bake compute PSO (reuses the tonemap set layout — a `STORAGE_IMAGE`/UBO/sampler triple
    /// binds a 3D output image the same as the 2D tonemap target — with an 8-byte size+mode push),
    /// built lazily on the first `bake-look`.
    lut_bake: Option<Arc<Pipeline>>,
    /// The tonemap compute set layout (one storage image) the tonemap PSO binds (set 0).
    tonemap_set_layout: vk::DescriptorSetLayout,
    /// The analytic height-fog composite compute PSO (fog set layout, no push), built lazily.
    fog: Option<Arc<Pipeline>>,
    /// The froxel volumetric-fog injection compute PSO (mesh light set 0 + fog volume set 1 + a
    /// 64-byte medium push), built lazily.
    fog_inject: Option<Arc<Pipeline>>,
    /// The froxel volumetric-fog integration compute PSO (fog integrate set, no push), built lazily.
    fog_integrate: Option<Arc<Pipeline>>,
    /// The aerial-perspective fill compute PSO (AP fill set: atmosphere LUTs + params + storage volume,
    /// no push), built lazily.
    aerial: Option<Arc<Pipeline>>,
    /// The fog compute set layout (offscreen storage + params UBO + depth + sky-view LUT).
    fog_set_layout: vk::DescriptorSetLayout,
    /// The depth-upscale graphics PSO (fullscreen triangle, depth-write-always, one fragment
    /// sampler + an 8-byte inputSize push): point-upscales the input-extent scene depth into the
    /// display-extent overlay depth so the grid / gizmo occlude correctly under upsampling.
    depth_upscale: Option<Arc<Pipeline>>,
    /// The TAA reactive-coverage graphics PSO (mesh vertex + a constant-1.0 fragment into an r8
    /// target, depth-tested read-only against the scene depth, set 2 + a viewProj push): re-draws
    /// the translucent batches to mark the reactive mask. Built lazily when TAA first resolves.
    reactive_coverage: Option<Arc<Pipeline>>,
    /// The analytic ground-grid graphics PSO (fullscreen, depth-tested, alpha-blended,
    /// a 2×mat4 push), built lazily.
    grid: Option<Arc<Pipeline>>,
    /// The always-on-top editor-overlay graphics PSO (the [`crate::OverlayVertex`]
    /// stream, alpha-blended, no depth test), built lazily.
    overlay: Option<Arc<Pipeline>>,
    /// The depth-tested editor-overlay graphics PSO (same as `overlay` but depth-tested
    /// so scene geometry occludes it), built lazily.
    overlay_depth: Option<Arc<Pipeline>>,
    /// The Lit Wireframe overlay graphics PSO (line polygon mode, depth-tested without write),
    /// built lazily; `None` on a device without `fill_mode_non_solid`.
    wireframe_overlay: Option<Arc<Pipeline>>,
    /// The motion-vector visualization compute PSO (copy_color-shaped set), built lazily.
    motion_visualize: Option<Arc<Pipeline>>,
}

impl Pipelines {
    /// Builds the cache front door against the device-global descriptor layouts.
    ///
    /// `sample_count` is the MSAA target sample count the mesh PSOs match;
    /// `descriptors` supplies the set layouts the pipeline layout
    /// binds. The shader dir is resolved once (`SAFFRON_SHADER_DIR` override, else the
    /// `shaders/` dir beside the running binary) so a build-and-cache never re-walks it.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        sample_count: vk::SampleCountFlags,
    ) -> Self {
        // The übershader declares descriptor sets 0-5 (and 6-7 under RT). The mesh PSO
        // layout must bind every one the SPIR-V references or pipeline creation is
        // invalid, so the full list is assembled here from the device-global layouts:
        //   0 bindless albedo · 1 lights+clusters+shadows · 2 per-instance+joints+mat
        //   3 IBL+probes · 4 AO+contact+SSGI · 5 DDGI · 6 TLAS (RT) · 7 ReSTIR (RT)
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
            depth_prepass: None,
            shadow_depth: None,
            point_shadow: None,
            light_cull: None,
            skin: None,
            morph: None,
            tess_factor: None,
            tess_scan: None,
            tess_finalize: None,
            tess_args: None,
            tessellate: None,
            meshlet: None,
            cluster_set_layout: descriptors.cluster_set_layout(),
            gbuffer: None,
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
            gdf_composite: None,
            restir_initial: None,
            restir_reuse: None,
            restir_resolve: None,
            gi_resolve: None,
            motion: None,
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
            fog_set_layout: descriptors.fog_set_layout(),
            depth_upscale: None,
            reactive_coverage: None,
            grid: None,
            overlay: None,
            overlay_depth: None,
            wireframe_overlay: None,
            motion_visualize: None,
        }
    }

    /// Re-targets the mesh + depth-prepass PSOs to a new MSAA sample count, clearing every
    /// sample-count-baked PSO so the next request rebuilds it. The mesh PSOs (cache),
    /// depth-prepass, and motion PSO bake the count; the screen-space + shadow PSOs are
    /// always 1× and untouched. The caller has idled the GPU (an AA change).
    pub fn set_sample_count(&mut self, sample_count: vk::SampleCountFlags) {
        if self.sample_count == sample_count {
            return;
        }
        self.sample_count = sample_count;
        // The mesh cache keys by sample count, so stale entries would never be hit again;
        // clear them so the dropped `Arc`s free once the GPU is idle (the caller's job).
        self.cache.clear();
        // The depth-prepass PSO bakes the count too — drop it to rebuild lazily. The
        // G-buffer / shadow / motion PSOs are always 1× (they feed post-resolve targets),
        // so they are untouched.
        self.depth_prepass = None;
        // The meshlet raster PSO bakes the count in its multisample state — drop it to rebuild.
        self.meshlet = None;
    }

    /// The MSAA sample count the sample-count-baked PSOs currently target.
    pub fn sample_count(&self) -> vk::SampleCountFlags {
        self.sample_count
    }

    /// The PSO-cache front door: returns the mesh pipeline for `material` + the
    /// skinned/wireframe permutation, building and caching it on first request and
    /// returning the shared [`Arc`] on a cache hit.
    ///
    /// Wireframe is gated on `fill_mode_non_solid`: an unsupported device falls back
    /// to the fill PSO (so the key never names a permutation the device cannot make).
    /// A build failure is logged and returns `None`.
    pub fn request_mesh_pipeline(
        &mut self,
        material: &Material,
        skinned: bool,
        wireframe: bool,
    ) -> Option<Arc<Pipeline>> {
        let wireframe = wireframe && self.fill_mode_non_solid;
        // A2C only exists under MSAA; a masked material at 1× is a plain runtime discard on the
        // opaque PSO, so it must not mint a separate cache entry.
        let alpha_to_coverage =
            material.masked && self.sample_count != vk::SampleCountFlags::TYPE_1;
        let key = PsoKey {
            shader: material.shader.clone(),
            unlit: material.unlit,
            skinned,
            wireframe,
            blend: material.blend,
            alpha_to_coverage,
            sample_count: self.sample_count,
        };
        if let Some(pipeline) = self.cache.get(&key) {
            return Some(Arc::clone(pipeline));
        }
        match self.build_mesh_pipeline(&key) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cache.insert(key, Arc::clone(&pipeline));
                // A non-zero count on a steady-state frame is a PSO-compile hitch.
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_mesh_pipeline: {err}");
                None
            }
        }
    }

    /// The vertex-only depth pre-pass PSO, built and cached on first request. It binds
    /// sets 0/1/2 (the same prefix as the mesh layout) and the viewProj push, writes no
    /// color, and depth-tests `LESS` with depth writes on. Returns `None` on a build
    /// failure (logged).
    pub fn request_depth_prepass(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.depth_prepass {
            return Some(Arc::clone(pipeline));
        }
        match self.build_depth_prepass() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.depth_prepass = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_depth_prepass: {err}");
                None
            }
        }
    }

    /// The vertex-only, depth-biased shadow depth PSO, built and cached on first request.
    /// Like the depth pre-pass but always single-sampled (the shadow map is 1×) and
    /// depth-biased (dynamic, set per shadow pass) to kill acne. Binds sets 0/1/2 + the
    /// light-space viewProj push. Returns `None` on a build failure (logged).
    pub fn request_shadow_depth(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.shadow_depth {
            return Some(Arc::clone(pipeline));
        }
        match self.build_shadow_depth() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.shadow_depth = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_shadow_depth: {err}");
                None
            }
        }
    }

    /// The point-shadow cube-face PSO (color distance + depth), built and cached on first
    /// request. Renders world distance-to-light into one cube face; the push carries the
    /// face viewProj (mat4) + the light world position (vec4), in the VERTEX|FRAGMENT
    /// stages. Returns `None` on a build failure (logged).
    pub fn request_point_shadow(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.point_shadow {
            return Some(Arc::clone(pipeline));
        }
        match self.build_point_shadow() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.point_shadow = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_point_shadow: {err}");
                None
            }
        }
    }

    /// The clustered light-cull compute PSO, built and cached on first request. Binds the
    /// cluster compute set (set 0: params UBO + light list + cluster lists) and dispatches
    /// one invocation per froxel. Returns `None` on a build failure (logged).
    pub fn request_light_cull(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.light_cull {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/light_cull.spv", self.cluster_set_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.light_cull = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_light_cull: {err}");
                None
            }
        }
    }

    /// The compute skinning PSO, built and cached on first request. Binds the skin set
    /// layout (four storage buffers: static vertices, skin, palette, deformed output) +
    /// a 16-byte push, and deforms one vertex per invocation. `skin_set_layout` is owned
    /// by [`crate::skinning::Skinning`]. Returns `None` on a build failure (logged).
    pub fn request_skin(
        &mut self,
        skin_set_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.skin {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/skin.spv", skin_set_layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.skin = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_skin: {err}");
                None
            }
        }
    }

    /// The compute morph PSO (morph set layout, a 20-byte `MorphPush`), built + cached on
    /// first request. Returns `None` on a build failure (logged).
    pub fn request_morph(
        &mut self,
        morph_set_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.morph {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/morph.spv", morph_set_layout, 24) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.morph = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_morph: {err}");
                None
            }
        }
    }

    /// The adaptive-tessellation factor PSO (bindless set 0 for the min/max pyramid + the edge
    /// storage-buffer set 1, a 128-byte push: mvp(64) + camPosLocal(16) + viewport(8) + 10 scalars(40)),
    /// built + cached. `None` on failure.
    pub fn request_tess_factor(
        &mut self,
        bindless: vk::DescriptorSetLayout,
        factor: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_factor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_factor.spv", &[bindless, factor], 128) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_factor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_factor: {err}");
                None
            }
        }
    }

    /// The predict/scan PSO (5 storage buffers, a 32-byte push), built + cached.
    pub fn request_tess_scan(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_scan {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_scan.spv", &[layout], 32) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_scan = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_scan: {err}");
                None
            }
        }
    }

    /// The per-instance finalize PSO (3 storage buffers, a 32-byte push), built + cached.
    pub fn request_tess_finalize(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_finalize {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_finalize.spv", &[layout], 32) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_finalize = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_finalize: {err}");
                None
            }
        }
    }

    /// The global dispatch-args PSO (2 storage buffers, no push), built + cached.
    pub fn request_tess_args(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_args {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_args.spv", &[layout], 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_args = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_args: {err}");
                None
            }
        }
    }

    /// The Phase-4 amplifying emit PSO (`tessellate.spv`, bindless set 0 + the emit set 1, a 64-byte
    /// push), built + cached.
    pub fn request_tessellate(
        &mut self,
        bindless: vk::DescriptorSetLayout,
        emit: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tessellate {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tessellate.spv", &[bindless, emit], 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tessellate = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tessellate: {err}");
                None
            }
        }
    }

    /// The thin G-buffer prepass PSO (view normal rgb + view-Z in .a), built and cached
    /// on first request. Instanced (sets 0/1/2), single-sampled (the G-buffer is
    /// post-resolve), depth `LESS` + write, the `viewProj + view` push. Returns `None`
    /// on a build failure (logged).
    pub fn request_gbuffer(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gbuffer {
            return Some(Arc::clone(pipeline));
        }
        match self.build_gbuffer() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gbuffer = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gbuffer: {err}");
                None
            }
        }
    }

    /// The GTAO compute PSO (compute2 layout, an 80-byte push), built + cached on first
    /// request.
    pub fn request_gtao(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Gtao, "shaders/gtao.spv", layout, 80)
    }

    /// The AO bilateral-blur compute PSO (compute3 layout, no push).
    pub fn request_ao_blur(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::AoBlur, "shaders/ao_blur.spv", layout, 0)
    }

    /// The directional contact-shadow compute PSO (compute2 layout, a 160-byte push).
    pub fn request_contact(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Contact, "shaders/contact.spv", layout, 160)
    }

    /// The one-bounce SSGI trace compute PSO (compute3 layout, a 144-byte push).
    pub fn request_ssgi(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Ssgi, "shaders/ssgi.spv", layout, 160)
    }

    /// The screen-space indirect-diffuse resolve compute PSO (the bespoke `gi_resolve` single-set
    /// layout; no push — its params ride a UBO in the set, being 224 bytes).
    pub fn request_gi_resolve(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::GiResolve,
            "shaders/gi_resolve.spv",
            layout,
            0,
        )
    }

    /// The SSGI bilateral-blur compute PSO (compute3 layout, no push).
    pub fn request_ssgi_blur(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::SsgiBlur, "shaders/ssgi_blur.spv", layout, 0)
    }

    /// The SSGI temporal-accumulation compute PSO (taa-shape layout, a 16-byte push).
    pub fn request_ssgi_accum(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::SsgiAccum,
            "shaders/ssgi_accum.spv",
            layout,
            16,
        )
    }

    /// The DFAO temporal-accumulation compute PSO (taa-shape layout, a 16-byte push). Its own
    /// clamp-free EMA kernel — DFAO sky visibility is a rotating low-frequency Monte-Carlo estimate
    /// that must be averaged (not neighborhood-clamped like SSGI radiance) to converge on a static
    /// surface, so it does not reuse `ssgi_accum`.
    pub fn request_dfao_accum(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DfaoAccum,
            "shaders/dfao_accum.spv",
            layout,
            16,
        )
    }

    /// The screen-space reflection trace compute PSO (compute3 layout, a 160-byte push,
    /// same shape as the SSGI trace; the shared `params2` slot is unused here).
    pub fn request_ssr(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Ssr, "shaders/ssr.spv", layout, 160)
    }

    /// The SSGI prev-color history-copy compute PSO (compute2 layout, no push).
    pub fn request_copy_color(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::CopyColor,
            "shaders/copy_color.spv",
            layout,
            0,
        )
    }

    /// The DDGI trace compute PSO. A three-set pipeline: set 0 = the bindless brick array, set 1 =
    /// the light set (the per-mesh SDF instance list + the GDF cascade clipmap + params, via the
    /// shared `sdf` module), set 2 = `trace_layout` (the albedo cache + prev-irradiance samplers +
    /// the ray-image storage). The 112-byte push carries the probe grid + volume + sun/sky + scroll.
    pub fn request_ddgi_trace(
        &mut self,
        trace_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.ddgi_trace {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], trace_layout];
        match self.build_compute_multi("shaders/ddgi_trace.spv", &set_layouts, 112) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.ddgi_trace = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_ddgi_trace: {err}");
                None
            }
        }
    }

    /// The DFAO sky-visibility cone-trace PSO. A three-set pipeline mirroring the DDGI trace:
    /// set 0 = the bindless brick array, set 1 = the light set (the GDF cascade clipmap + params
    /// at bindings 9/10, via the shared `sdf` module), set 2 = `io_layout` (the compute2 shape:
    /// the G-buffer sampler + the half-res sky-visibility storage). A 144-byte push (the camera
    /// inverses + the frame index).
    pub fn request_dfao(&mut self, io_layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.dfao {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], io_layout];
        match self.build_compute_multi("shaders/dfao.spv", &set_layouts, 144) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.dfao = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_dfao: {err}");
                None
            }
        }
    }

    /// The specular reflection-occlusion cone-trace PSO. A three-set pipeline mirroring
    /// [`Pipelines::request_dfao`]: set 0 = the bindless brick array, set 1 = the light set (the GDF
    /// cascade clipmap + params at bindings 9/10, via the shared `sdf` module), set 2 = `io_layout`
    /// (the compute3 shape: the G-buffer + roughness samplers + the half-res occlusion storage). A
    /// 144-byte push (the camera inverses + the frame index).
    pub fn request_specocc(&mut self, io_layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.specocc {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], io_layout];
        match self.build_compute_multi("shaders/specocc.spv", &set_layouts, 144) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.specocc = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_specocc: {err}");
                None
            }
        }
    }

    /// The DDGI blend-irradiance compute PSO (its set layout, an 80-byte push).
    pub fn request_ddgi_blend_irr(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBlendIrr,
            "shaders/ddgi_blend_irradiance.spv",
            layout,
            80,
        )
    }

    /// The DDGI blend-distance compute PSO (its set layout, an 80-byte push).
    pub fn request_ddgi_blend_dist(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBlendDist,
            "shaders/ddgi_blend_distance.spv",
            layout,
            80,
        )
    }

    /// The DDGI octahedral-border compute PSO (its set layout, a 32-byte push).
    pub fn request_ddgi_border(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBorder,
            "shaders/ddgi_border.spv",
            layout,
            32,
        )
    }

    /// The Global-SDF cull compute PSO: set 0 = the bindless brick array (for the shared
    /// `SdfInstance` struct), set 1 = `gdf_layout` (instances + cull list), a 64-byte push.
    pub fn request_gdf_cull(
        &mut self,
        gdf_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gdf_cull {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], gdf_layout];
        match self.build_compute_multi("shaders/gdf_cull.spv", &set_layouts, 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gdf_cull = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gdf_cull: {err}");
                None
            }
        }
    }

    /// The Global-SDF composite compute PSO: set 0 = the bindless brick array (the `sampleMdfBrick`
    /// taps), set 1 = `gdf_layout` (instances + cull list + cascade storage images), a 48-byte push.
    pub fn request_gdf_composite(
        &mut self,
        gdf_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gdf_composite {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], gdf_layout];
        match self.build_compute_multi("shaders/gdf_composite.spv", &set_layouts, 48) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gdf_composite = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gdf_composite: {err}");
                None
            }
        }
    }

    /// The ReSTIR initial-candidate-sampling compute PSO (its set layout, a 176-byte push).
    pub fn request_restir_initial(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::RestirInitial,
            "shaders/restir_initial.spv",
            layout,
            crate::RESTIR_INITIAL_PUSH_SIZE,
        )
    }

    /// The ReSTIR temporal+spatial reuse compute PSO (its set layout, a 160-byte push).
    pub fn request_restir_reuse(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::RestirReuse,
            "shaders/restir_reuse.spv",
            layout,
            crate::RESTIR_REUSE_PUSH_SIZE,
        )
    }

    /// The ReSTIR resolve compute PSO (its set layout incl. the TLAS binding, a 160-byte
    /// push). RT-only — the resolve traces one visibility ray via the TLAS.
    pub fn request_restir_resolve(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::RestirResolve,
            "shaders/restir_resolve.spv",
            layout,
            crate::RESTIR_RESOLVE_PUSH_SIZE,
        )
    }

    /// The motion-vector prepass PSO (instanced scene, depth-tested, rg16f motion from
    /// cur/prev camera reprojection), built and cached on first request. Two vertex
    /// bindings (cur + prev position), sets 0/1/2, a 2×mat4 push. Built single-sampled but
    /// re-dropped on a sample-count change so it tracks the active count. Returns `None` on
    /// a build failure (logged).
    pub fn request_motion(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.motion {
            return Some(Arc::clone(pipeline));
        }
        match self.build_motion() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.motion = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_motion: {err}");
                None
            }
        }
    }

    /// The TAA resolve compute PSO (the taa-shape set layout: 3 samplers + 2 storage + the
    /// motion-depth sampler, a 48-byte push), built and cached on first request.
    pub fn request_taa(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.taa {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/taa.spv",
            layout,
            size_of::<crate::TaaPush>() as u32,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.taa = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_taa: {err}");
                None
            }
        }
    }

    /// The FXAA edge-blur compute PSO (the fxaa set layout: source sampler + offscreen
    /// storage, no push), built and cached on first request.
    pub fn request_fxaa(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fxaa {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/fxaa.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fxaa = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fxaa: {err}");
                None
            }
        }
    }

    /// The bloom pyramid compute PSO (bloom set layout, a 32-byte [`crate::BloomPush`]), built and
    /// cached on first request. One PSO covers the downsample / upsample / composite passes.
    pub fn request_bloom(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.bloom {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/bloom.spv",
            layout,
            size_of::<crate::BloomPush>() as u32,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.bloom = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_bloom: {err}");
                None
            }
        }
    }

    /// The mandatory tonemap compute PSO (tonemap set layout, an 8-byte exposure+mode push),
    /// built and cached on first request. Returns `None`
    /// only on a build failure (logged) — the tonemap is otherwise always present.
    pub fn request_tonemap(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tonemap {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/tonemap.spv", self.tonemap_set_layout, 8) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tonemap = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tonemap: {err}");
                None
            }
        }
    }

    /// The analytic height-fog composite compute PSO (`height_fog.spv`, the fog set layout, no
    /// push), built and cached on first use. Returns `None` on a build failure (logged).
    pub fn request_fog(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/height_fog.spv", self.fog_set_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog: {err}");
                None
            }
        }
    }

    /// The froxel fog-injection compute PSO (`fog_inject.spv`): set 0 is the mesh light set layout
    /// (globals/lights/clusters/cluster params + shadow maps, reused verbatim), set 1 is the fog
    /// volume set (scatter storage image + grid UBO), plus a 64-byte medium push. Built and cached on
    /// first use. Returns `None` on a build failure (logged).
    pub fn request_fog_inject(
        &mut self,
        volume_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog_inject {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[1], volume_layout];
        match self.build_compute_multi("shaders/fog_inject.spv", &set_layouts, 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog_inject = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog_inject: {err}");
                None
            }
        }
    }

    /// The froxel fog-integration compute PSO (`fog_integrate.spv`, the integrate set layout, no
    /// push), built and cached on first use. Returns `None` on a build failure (logged).
    pub fn request_fog_integrate(
        &mut self,
        integrate_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog_integrate {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/fog_integrate.spv", integrate_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog_integrate = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog_integrate: {err}");
                None
            }
        }
    }

    /// The aerial-perspective fill compute PSO (`aerial_perspective.spv`, the AP fill set, no push),
    /// built lazily and resolved only while an active atmosphere + authored AP arm the fill.
    pub fn request_aerial(
        &mut self,
        fill_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.aerial {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/aerial_perspective.spv", fill_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.aerial = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_aerial: {err}");
                None
            }
        }
    }

    /// The look-bake compute PSO (folds grade + view transform + creative LUT into a `33³` table),
    /// built and cached on the first `bake-look`. Returns `None` on a build failure (logged).
    pub fn request_lut_bake(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.lut_bake {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/lut_bake.spv", self.tonemap_set_layout, 8) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.lut_bake = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_lut_bake: {err}");
                None
            }
        }
    }

    /// The analytic ground-grid graphics PSO (fullscreen triangle, depth-tested without
    /// writing, alpha-blended, a 2×mat4 vertex+fragment push), built and cached on first
    /// request. Single-sampled (the grid draws on the 1× resolved color after tonemap).
    /// Returns `None` on a build failure (logged).
    /// The depth-upscale graphics PSO (point-upscales the input-extent scene depth into the
    /// display-extent overlay depth), built and cached on first request. `layout` is
    /// [`crate::Descriptors::depth_upscale_layout`]. Returns `None` on a build failure (logged).
    pub fn request_depth_upscale(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.depth_upscale {
            return Some(Arc::clone(pipeline));
        }
        match self.build_depth_upscale(layout) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.depth_upscale = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_depth_upscale: {err}");
                None
            }
        }
    }

    /// The TAA reactive-coverage graphics PSO (marks the translucent batches into the r8 reactive
    /// mask), built and cached on first request. Returns `None` on a build failure (logged).
    pub fn request_reactive_coverage(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.reactive_coverage {
            return Some(Arc::clone(pipeline));
        }
        match self.build_reactive_coverage() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.reactive_coverage = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_reactive_coverage: {err}");
                None
            }
        }
    }

    pub fn request_grid(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.grid {
            return Some(Arc::clone(pipeline));
        }
        match self.build_grid() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.grid = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_grid: {err}");
                None
            }
        }
    }

    /// The Lit Wireframe overlay PSO (line polygon mode, depth-tested without write), built
    /// and cached on first request. Returns `None` when the device lacks
    /// `fill_mode_non_solid` (the mode then falls back to plain Lit) or on a build failure
    /// (logged).
    pub fn request_wireframe_overlay(&mut self) -> Option<Arc<Pipeline>> {
        if !self.fill_mode_non_solid {
            return None;
        }
        if let Some(pipeline) = &self.wireframe_overlay {
            return Some(Arc::clone(pipeline));
        }
        match self.build_wireframe_overlay() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.wireframe_overlay = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_wireframe_overlay: {err}");
                None
            }
        }
    }

    /// The motion-vector visualization compute PSO, built and cached on first request. Binds
    /// the copy_color-shaped set (one sampler + one storage image); `layout` is
    /// [`crate::Ssao::compute2_layout`]. Returns `None` on a build failure (logged).
    pub fn request_motion_visualize(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.motion_visualize {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/motion_visualize.spv", layout, 8) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.motion_visualize = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_motion_visualize: {err}");
                None
            }
        }
    }

    /// The always-on-top editor-overlay graphics PSO (the [`crate::OverlayVertex`]
    /// stream, alpha-blended, no depth test, single-sampled, no descriptor sets), built
    /// and cached on first request. Returns `None` on a build failure (logged).
    pub fn request_overlay(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.overlay {
            return Some(Arc::clone(pipeline));
        }
        match self.build_overlay(false) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.overlay = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_overlay: {err}");
                None
            }
        }
    }

    /// The depth-tested editor-overlay graphics PSO (same as `overlay` but depth-tested
    /// so scene geometry occludes it — camera frustums, etc.), built and cached on first
    /// request. Returns `None` on a build failure (logged).
    pub fn request_overlay_depth(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.overlay_depth {
            return Some(Arc::clone(pipeline));
        }
        match self.build_overlay(true) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.overlay_depth = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_overlay_depth: {err}");
                None
            }
        }
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
            ScreenCompute::RestirResolve => self.restir_resolve.as_ref(),
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
            ScreenCompute::RestirResolve => &mut self.restir_resolve,
            ScreenCompute::GiResolve => &mut self.gi_resolve,
        }
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

    /// Builds one mesh PSO for `key`: loads the shader, sets the unlit spec constant,
    /// selects the vertex entry/input for the skinned variant, the wireframe polygon
    /// mode, and the dynamic-rendering color/depth formats.
    fn build_mesh_pipeline(&self, key: &PsoKey) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module(&key.shader)?;
        // Free the shader module however the rest of this function returns.
        let result = self.build_mesh_pipeline_with_module(raw, key, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing
        // it after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_mesh_pipeline_with_module(
        &self,
        raw: &ash::Device,
        key: &PsoKey,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        // Fragment spec constants: id 0 = unlit branch, id 1 = alpha-to-coverage (masked+MSAA,
        // the fragment sharpens the cutout into per-sample coverage when set), id 2 = translucent
        // (the blend permutation — gates off the opaque G-buffer's screen-space terms and uses
        // world-space indirect instead; see mesh.slang / lighting.slang).
        let unlit_value: vk::Bool32 = u32::from(key.unlit);
        let a2c_value: vk::Bool32 = u32::from(key.alpha_to_coverage);
        let translucent_value: vk::Bool32 = u32::from(key.blend);
        let mut spec_data = [0u8; 12];
        spec_data[0..4].copy_from_slice(&unlit_value.to_ne_bytes());
        spec_data[4..8].copy_from_slice(&a2c_value.to_ne_bytes());
        spec_data[8..12].copy_from_slice(&translucent_value.to_ne_bytes());
        let spec_entries = [
            vk::SpecializationMapEntry::default()
                .constant_id(0)
                .offset(0)
                .size(std::mem::size_of::<vk::Bool32>()),
            vk::SpecializationMapEntry::default()
                .constant_id(1)
                .offset(4)
                .size(std::mem::size_of::<vk::Bool32>()),
            vk::SpecializationMapEntry::default()
                .constant_id(2)
                .offset(8)
                .size(std::mem::size_of::<vk::Bool32>()),
        ];
        let spec_info = vk::SpecializationInfo::default()
            .map_entries(&spec_entries)
            .data(&spec_data);

        let vertex_entry: &CStr = if key.skinned {
            c"vertexMainSkinned"
        } else {
            c"vertexMain"
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(vertex_entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain")
                .specialization_info(&spec_info),
        ];

        // Binding 0: the base Vertex stream. Binding 1: the VertexSkin stream, added
        // only for the skinned variant (the unskinned layout is untouched).
        let bindings = [
            vk::VertexInputBindingDescription::default()
                .binding(0)
                .stride(size_of::<Vertex>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX),
            vk::VertexInputBindingDescription::default()
                .binding(1)
                .stride(size_of::<VertexSkin>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX),
        ];
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(offset_of_vertex_position()),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(offset_of_vertex_normal()),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(offset_of_vertex_uv0()),
            vk::VertexInputAttributeDescription::default()
                .location(3)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(offset_of_vertex_tangent()),
            vk::VertexInputAttributeDescription::default()
                .location(4)
                .binding(1)
                .format(vk::Format::R16G16B16A16_UINT)
                .offset(offset_of_skin_joints()),
            vk::VertexInputAttributeDescription::default()
                .location(5)
                .binding(1)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(offset_of_skin_weights()),
        ];
        let (binding_count, attribute_count) = if key.skinned { (2, 6) } else { (1, 4) };
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings[..binding_count])
            .vertex_attribute_descriptions(&attributes[..attribute_count]);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);

        let polygon_mode = if key.wireframe {
            vk::PolygonMode::LINE
        } else {
            vk::PolygonMode::FILL
        };
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(polygon_mode)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);

        // Alpha-to-coverage: a masked material under MSAA turns its sharpened cutout alpha into
        // a per-sample coverage mask — anti-aliased foliage edges, order-independent, no blending.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(key.sample_count)
            .alpha_to_coverage_enable(key.alpha_to_coverage);

        // Translucent geometry tests against the opaque depth but does not write it (so
        // stacked translucent layers all shade); opaque/masked writes depth as usual.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(!key.blend)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);

        // Opaque/masked: blend off, depth-written. Translucent: straight-alpha `over`
        // (`src.rgb*src.a + dst.rgb*(1-src.a)`), the shader emitting `surf.opacity` in alpha.
        let blend_attachment = [if key.blend {
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)
        } else {
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA)
        }];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);

        // Backface culling is dynamic (set per submesh by the scene pass — two-sided materials cull
        // NONE, solid geometry culls BACK); the baked `cull_mode(NONE)` above is the fallback.
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::CULL_MODE,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<saffron_geometry::glam::Mat4>() as u32)]; // viewProj

        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&self.set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call (owned by the
        // descriptors sub-state); the layout is owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (mesh)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);

        // SAFETY: the ash seam. The create-info chain outlives the call; the cache
        // (`VK_NULL_HANDLE`) is the no-cache path. On failure the layout is freed.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above and is freed
                // exactly once on the error path.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (mesh)",
                    result,
                });
            }
        };

        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// The `VK_EXT_mesh_shader` meshlet raster PSO (task+mesh from `meshlet.spv`, the übershader
    /// `fragmentMain` from `mesh.spv`), built lazily and cached. `meshlet_set_layout` is set 8 (the
    /// meshlet decomposition + base vertex stream). Built only when the mesh-shader path is enabled;
    /// `None` on a build failure (logged).
    pub fn request_meshlet(
        &mut self,
        meshlet_set_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.meshlet {
            return Some(Arc::clone(pipeline));
        }
        match self.build_meshlet_pipeline(meshlet_set_layout) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.meshlet = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_meshlet: {err}");
                None
            }
        }
    }

    fn build_meshlet_pipeline(
        &self,
        meshlet_set_layout: vk::DescriptorSetLayout,
    ) -> Result<Pipeline> {
        let raw = self.resources.device();
        // Two modules: the task/mesh stages from meshlet.spv, the fragment from the übershader.
        let meshlet_module = self.load_shader_module("shaders/meshlet.spv")?;
        let mesh_module = match self.load_shader_module("shaders/mesh.spv") {
            Ok(m) => m,
            Err(err) => {
                // SAFETY: the ash seam. The meshlet module was created above; freed once here.
                unsafe { raw.destroy_shader_module(meshlet_module, None) };
                return Err(err);
            }
        };
        let result = self.build_meshlet_pipeline_with_modules(
            raw,
            meshlet_module,
            mesh_module,
            meshlet_set_layout,
        );
        // SAFETY: the ash seam. Both modules are consumed by pipeline creation; freed after.
        unsafe {
            raw.destroy_shader_module(meshlet_module, None);
            raw.destroy_shader_module(mesh_module, None);
        }
        result
    }

    fn build_meshlet_pipeline_with_modules(
        &self,
        raw: &ash::Device,
        meshlet_module: vk::ShaderModule,
        mesh_module: vk::ShaderModule,
        meshlet_set_layout: vk::DescriptorSetLayout,
    ) -> Result<Pipeline> {
        // The meshlet path is the opaque-lit permutation: unlit / a2c / translucent all off.
        let spec_data = [0u8; 12];
        let spec_entries = [
            vk::SpecializationMapEntry::default()
                .constant_id(0)
                .offset(0)
                .size(4),
            vk::SpecializationMapEntry::default()
                .constant_id(1)
                .offset(4)
                .size(4),
            vk::SpecializationMapEntry::default()
                .constant_id(2)
                .offset(8)
                .size(4),
        ];
        let spec_info = vk::SpecializationInfo::default()
            .map_entries(&spec_entries)
            .data(&spec_data);

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::TASK_EXT)
                .module(meshlet_module)
                .name(c"taskMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::MESH_EXT)
                .module(meshlet_module)
                .name(c"meshMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(mesh_module)
                .name(c"fragmentMain")
                .specialization_info(&spec_info),
        ];

        // No vertex-input / input-assembly state: a mesh shader fetches its own geometry.
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.sample_count);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // Layout: the übershader sets 0–7 (identical, so the frame's sets rebind here) + set 8
        // (meshlet geometry) + the mesh/task push (viewProj + instance/meshlet/vertex indices).
        // `meshlet.slang` fixes the geometry set at index 8; when the device lacks RT (so the
        // übershader list stops at set 5) sets 6/7 are padded with an unused layout so the geometry
        // set still lands at 8. The übershader `fragmentMain` never references the padded sets.
        let mut set_layouts = self.set_layouts.clone();
        while set_layouts.len() < 8 {
            set_layouts.push(self.set_layouts[0]);
        }
        set_layouts.push(meshlet_set_layout);
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::MESH_EXT | vk::ShaderStageFlags::TASK_EXT)
            .offset(0)
            .size(size_of::<MeshletPush>() as u32)];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by the Pipeline.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (meshlet)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the layout frees.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once on the error path.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (meshlet)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the vertex-only depth pre-pass PSO from the übershader's `vertexMain`:
    /// binding 0 = the base [`Vertex`] stream (position/normal/uv0), no color, depth
    /// `LESS` + write, sets 0/1/2, the viewProj push.
    fn build_depth_prepass(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_depth_prepass_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_depth_prepass_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        // Vertex writes depth; the fragment does only the alpha-clip discard (masked materials must
        // not write depth at cutout texels). Opaque materials fall straight through — depth-only.
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"depthPrepassFragment"),
        ];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.sample_count);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        // No color attachments — depth only.
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<saffron_geometry::glam::Mat4>() as u32)];
        // The depth pre-pass binds the same set prefix as the mesh layout (0 bindless,
        // 1 light, 2 instance) so the viewProj push + instance read match the scene pass.
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (depth-prepass)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (depth-prepass)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the TAA reactive-coverage PSO from `mesh.spv` (`vertexMain` +
    /// `reactiveCoverageFragment`): binding 0 = the base [`Vertex`] stream (skinned batches bind
    /// their deformed buffer, same as the depth prepass), one `R8_UNORM` color the constant-1.0
    /// fragment writes, depth `LESS_OR_EQUAL` **read-only** (test against the resolved scene depth
    /// so occluded translucents don't mark), single-sampled (the reactive target + TAA scratch are
    /// 1×), cull off (translucents are often double-sided), sets 0/1/2 + the viewProj push.
    fn build_reactive_coverage(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_reactive_coverage_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it after
        // creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_reactive_coverage_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"reactiveCoverageFragment"),
        ];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // The reactive mask + the TAA scratch it feeds are always single-sampled (TAA and MSAA
        // are mutually exclusive), so this PSO is not sample-count baked.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Read-only depth test against the resolved scene depth: mark only visible translucent
        // fragments (never write, so the scene depth the later passes read is untouched).
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::R)
            .blend_enable(false)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::REACTIVE_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<saffron_geometry::glam::Mat4>() as u32)];
        // Same set prefix as the mesh layout (0 bindless, 1 light, 2 instance); the coverage pass
        // binds only set 2 + the viewProj push, matching the depth prepass.
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by the
        // returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (reactive-coverage)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the layout is
        // freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (reactive-coverage)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the thin G-buffer prepass PSO from `gbuffer.slang`: binding 0 = the base
    /// [`Vertex`] stream, two colors (`R16G16B16A16_SFLOAT` view normal rgb + view-Z, then
    /// `R8_UNORM` roughness), depth `LESS` + write, single-sampled (the G-buffer is post-resolve),
    /// sets 0/1/2, the `viewProj + view` push.
    fn build_gbuffer(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/gbuffer.spv")?;
        let result = self.build_gbuffer_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_gbuffer_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // The G-buffer is always single-sampled (the screen-space effects are post-resolve).
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        // Two color attachments (view normal + view-Z, then roughness); blend disabled on both.
        let blend_attachment = [
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA),
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA),
        ];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::ssao::G_NORMAL_FORMAT, crate::ssao::ROUGHNESS_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // The push is two mat4s (viewProj + view), vertex stage.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)];
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (gbuffer)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (gbuffer)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    fn build_wireframe_overlay(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/wireframe_overlay.spv")?;
        let result = self.build_wireframe_overlay_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    /// Builds the Lit Wireframe overlay PSO: one base vertex stream + the per-instance set,
    /// `PolygonMode::LINE`, depth-tested (`LESS_OR_EQUAL`) without write, single-sampled (it
    /// draws on the 1× resolved color after tonemap), offscreen color, a single `viewProj`
    /// push.
    fn build_wireframe_overlay_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::LINE)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // The overlay draws on the 1× resolved color after tonemap.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Depth-test against the persisted 1× scene depth so hidden edges are occluded; never
        // write (the scene already laid the depth down).
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<saffron_geometry::glam::Mat4>() as u32)]; // viewProj
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by the
        // returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (wireframe-overlay)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (wireframe-overlay)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the motion-vector prepass PSO from `motion.slang`: two vertex bindings (cur
    /// position on binding 0, prev position on binding 1), instanced (sets 0/1/2),
    /// single-sampled (the motion target is 1×), depth `LESS` + write, rg16f color, the
    /// cur/prev viewProj push.
    fn build_motion(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/motion.spv")?;
        let result = self.build_motion_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_motion_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        // Two per-vertex streams: binding 0 is this frame's position (+ normal + uv0),
        // binding 1 the previous frame's position. The `drawIndexed` vertex offset applies
        // to both; a static batch binds the same buffer twice (prevPosition == position).
        let bindings = [
            vk::VertexInputBindingDescription::default()
                .binding(0)
                .stride(size_of::<Vertex>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX),
            vk::VertexInputBindingDescription::default()
                .binding(1)
                .stride(size_of::<Vertex>() as u32)
                .input_rate(vk::VertexInputRate::VERTEX),
        ];
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(offset_of_vertex_position()),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(offset_of_vertex_normal()),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(offset_of_vertex_uv0()),
            // The previous-frame position reads `Vertex::position` off binding 1.
            vk::VertexInputAttributeDescription::default()
                .location(3)
                .binding(1)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(offset_of_vertex_position()),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // Always single-sampled: the motion target is 1×, sampled by the TAA / SSGI resolve.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::aa::MOTION_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)]; // cur + prev viewProj
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (motion)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (motion)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the vertex-only, depth-biased shadow depth PSO from the übershader's
    /// `vertexMain`: binding 0 = the base [`Vertex`] stream, no color, depth `LESS` +
    /// write, dynamic depth-bias, single-sampled, sets 0/1/2, the light-viewProj push.
    fn build_shadow_depth(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_shadow_depth_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_shadow_depth_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(c"vertexMain")];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        // Depth-biased (set dynamically per shadow pass) to remove shadow acne.
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .depth_bias_enable(true)
            .line_width(1.0);
        // The shadow map is never multisampled.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::DEPTH_BIAS,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<saffron_geometry::glam::Mat4>() as u32)];
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (shadow)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (shadow)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the point-shadow cube-face PSO from `point_shadow.slang`: binding 0 = the
    /// base [`Vertex`] stream, one `R32_SFLOAT` color (distance) + depth, depth `LESS` +
    /// write, single-sampled, sets 0/1/2, the (mat4 viewProj + vec4 lightPos) push in the
    /// VERTEX|FRAGMENT stages.
    fn build_point_shadow(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/point_shadow.spv")?;
        let result = self.build_point_shadow_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_point_shadow_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        let vertex_bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = base_vertex_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&vertex_bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::lighting::POINT_SHADOW_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // The push is mat4 viewProj + vec4 lightPos, read in the vertex AND fragment stages.
        let push_size = (size_of::<saffron_geometry::glam::Mat4>()
            + size_of::<saffron_geometry::glam::Vec4>()) as u32;
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(push_size)];
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (point-shadow)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (point-shadow)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds a compute PSO from `shader` over `set_layout` with an optional
    /// compute-stage push of `push_size` bytes (0 = none). Entry point `computeMain`.
    fn build_compute(
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

    /// Builds the analytic ground-grid PSO from `grid.slang`: a fullscreen triangle (no
    /// vertex buffer), depth-tested `LESS_OR_EQUAL` without writing (it emits `SV_Depth`
    /// to occlude against the persisted 1× scene depth), alpha-blended over the resolved
    /// color, single-sampled, the `viewProj + invViewProj` push (vertex+fragment), no
    /// descriptor sets.
    fn build_depth_upscale(&self, set_layout: vk::DescriptorSetLayout) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/depth_upscale.spv")?;
        let result = self.build_depth_upscale_with_module(raw, module, set_layout);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_depth_upscale_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
        set_layout: vk::DescriptorSetLayout,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];
        // Fullscreen triangle — no vertex buffer.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Depth-write-always: the fragment emits the point-sampled input depth as SV_Depth for
        // every display pixel (compare ALWAYS so the write is unconditional). No color attachment.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::ALWAYS);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        // Depth-only: no color attachment, depth attachment format only.
        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        // The push is the input depth extent (for the nearest-texel-centre fetch), fragment-only.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(2 * size_of::<f32>() as u32)];
        let set_layouts = [set_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layout + push range outlive the call; the layout is
        // owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (depth-upscale)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (depth-upscale)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    fn build_grid(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/grid.spv")?;
        let result = self.build_grid_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_grid_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];
        // Fullscreen triangle — no vertex buffer.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // 1× post-resolve, like the overlay.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Test against the persisted 1× scene depth without writing it; the fragment
        // emits SV_Depth so geometry in front of the plane occludes the grid.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [alpha_blend_attachment()];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // The push is viewProj + invViewProj, read in the vertex AND fragment stages.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)];
        let layout_info =
            vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The push range outlives the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (grid)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (grid)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds an editor-overlay PSO from `gizmo_overlay.slang`: the four-attribute
    /// [`crate::OverlayVertex`] stream (position / color / edge / depth), no descriptor
    /// sets, alpha-blended, single-sampled, no depth write. `depth_test` selects the
    /// occluded variant (`LESS_OR_EQUAL` against the scene depth) vs the on-top variant
    /// (no test); both declare the depth format so the PSO stays render-pass compatible
    /// with the overlay pass's depth attachment.
    fn build_overlay(&self, depth_test: bool) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/gizmo_overlay.spv")?;
        let result = self.build_overlay_with_module(raw, module, depth_test);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_overlay_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
        depth_test: bool,
    ) -> Result<Pipeline> {
        use crate::overlay::OverlayVertex;

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMain"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<OverlayVertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, position) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, color) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, edge) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(3)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, depth) as u32),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // The depth-tested variant occludes against the scene depth without touching it
        // (LESS_OR_EQUAL matches the scene pass's compare); the on-top variant never
        // tests. Neither writes depth.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(depth_test)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [alpha_blend_attachment()];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        // Both variants run in the overlay pass, which binds a depth attachment; declare
        // its format so the PSO is render-pass compatible even when depth testing is off.
        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let layout_info = vk::PipelineLayoutCreateInfo::default();
        // SAFETY: the ash seam. The layout is owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (overlay)",
        )?;

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (overlay)",
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

/// The three base-[`Vertex`]-stream attributes (position/normal/uv0 on binding 0) the
/// depth-only PSOs (depth-prepass, shadow, point-shadow) all declare.
fn base_vertex_attributes() -> [vk::VertexInputAttributeDescription; 4] {
    [
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(offset_of_vertex_position()),
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(offset_of_vertex_normal()),
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(offset_of_vertex_uv0()),
        vk::VertexInputAttributeDescription::default()
            .location(3)
            .binding(0)
            .format(vk::Format::R32G32B32A32_SFLOAT)
            .offset(offset_of_vertex_tangent()),
    ]
}

/// The offset of `Vertex::position` (the vertex-input attribute the PSO declares).
fn offset_of_vertex_position() -> u32 {
    std::mem::offset_of!(Vertex, position) as u32
}
fn offset_of_vertex_normal() -> u32 {
    std::mem::offset_of!(Vertex, normal) as u32
}
fn offset_of_vertex_uv0() -> u32 {
    std::mem::offset_of!(Vertex, uv0) as u32
}
fn offset_of_vertex_tangent() -> u32 {
    std::mem::offset_of!(Vertex, tangent) as u32
}
fn offset_of_skin_joints() -> u32 {
    std::mem::offset_of!(VertexSkin, joints) as u32
}
fn offset_of_skin_weights() -> u32 {
    std::mem::offset_of!(VertexSkin, weights) as u32
}

/// Resolves the runtime shader directory: the `SAFFRON_SHADER_DIR` override, else the
/// `shaders/` dir beside the running binary, else walking up from the binary to find
/// one (the test binary runs from `target/<profile>/deps/`, one level below the
/// `shaders/` the xtask emits into `target/<profile>/shaders/`). Shared with the IBL bake
/// (which builds its own transient compute pipelines off the same dir).
/// The `_nort` (ray-tracing-off) sibling of a compiled SPIR-V path: `…/mesh.spv` → `…/mesh_nort.spv`.
/// Only the übershader emits one; for any other path the sibling simply does not exist and the
/// caller falls back to the base file.
fn nort_variant_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("spv");
    path.with_file_name(format!("{stem}_nort.{ext}"))
}

pub(crate) fn resolve_shader_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAFFRON_SHADER_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
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
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use std::sync::Mutex;

    /// Builds a headless device + descriptors + the cache, or skips (no Vulkan ICD).
    fn fixture_or_skip() -> Option<(Device, Descriptors, Pipelines)> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        Some((device, descriptors, pipelines))
    }

    /// A `PsoKey` is the matchable cache key: equal tuples are equal + hash equal, a
    /// differing flag is a distinct key. Runs on any host (the key is GPU-free logic).
    #[test]
    fn pso_key_distinguishes_every_variant() {
        use std::collections::HashSet;
        let base = PsoKey {
            shader: "shaders/mesh.spv".to_string(),
            unlit: false,
            skinned: false,
            wireframe: false,
            blend: false,
            alpha_to_coverage: false,
            sample_count: vk::SampleCountFlags::TYPE_1,
        };
        let mut set = HashSet::new();
        set.insert(base.clone());
        // Re-inserting the identical key does not grow the set (the cache-hit path).
        assert!(!set.insert(base.clone()));
        assert_eq!(set.len(), 1);
        // Each toggled flag is a distinct key.
        for variant in [
            PsoKey {
                unlit: true,
                ..base.clone()
            },
            PsoKey {
                skinned: true,
                ..base.clone()
            },
            PsoKey {
                wireframe: true,
                ..base.clone()
            },
            PsoKey {
                blend: true,
                ..base.clone()
            },
            PsoKey {
                alpha_to_coverage: true,
                ..base.clone()
            },
            PsoKey {
                sample_count: vk::SampleCountFlags::TYPE_4,
                ..base.clone()
            },
        ] {
            assert!(set.insert(variant));
        }
        assert_eq!(set.len(), 7);
    }

    /// The same variant requested twice returns the *same* `Arc` (one PSO, a cache
    /// hit); distinct variants produce distinct entries; `pipeline_count` reflects the
    /// cache size — the phase's übershader-reuse gate. Validation-clean build +
    /// teardown. Skips when no Vulkan device is present.
    #[test]
    fn request_mesh_pipeline_caches_per_variant() {
        let Some((device, _descriptors, mut pipelines)) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();

        let lit = Material::default();
        // First request builds + caches; the second is a cache hit (same Arc).
        let a = pipelines
            .request_mesh_pipeline(&lit, false, false)
            .expect("lit PSO builds on llvmpipe");
        let b = pipelines
            .request_mesh_pipeline(&lit, false, false)
            .expect("second request hits the cache");
        assert!(
            Arc::ptr_eq(&a, &b),
            "the same variant returns the same cached Arc (one PSO)"
        );
        assert_eq!(pipelines.pipeline_count(), 1, "many requests, one PSO");
        assert_eq!(pipelines.pipelines_created(), 1);

        // The unlit permutation is a distinct cache entry.
        let unlit = Material {
            unlit: true,
            ..Material::default()
        };
        let c = pipelines
            .request_mesh_pipeline(&unlit, false, false)
            .expect("unlit PSO builds");
        assert!(!Arc::ptr_eq(&a, &c), "unlit is a distinct PSO");
        assert_eq!(pipelines.pipeline_count(), 2);

        // The skinned permutation is a third distinct entry.
        let _skinned = pipelines
            .request_mesh_pipeline(&lit, true, false)
            .expect("skinned PSO builds");
        assert_eq!(
            pipelines.pipeline_count(),
            3,
            "skinned adds a third distinct PSO"
        );
        assert_eq!(pipelines.pipelines_created(), 3);

        drop(a);
        drop(b);
        drop(c);
        drop(_skinned);
        drop(pipelines);
        drop(_descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the PSO cache build + teardown must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The AA PSOs build on llvmpipe (motion graphics + TAA/FXAA compute), and
    /// `set_sample_count` clears the sample-count-baked cache + drops the depth-prepass so
    /// the next request rebuilds for the new count, while leaving the always-1× motion PSO
    /// intact. Skips when no device.
    #[test]
    fn aa_pipelines_build_and_sample_count_change_clears_the_baked_cache() {
        let Some((device, descriptors, mut pipelines)) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();

        // The three AA PSOs compile on llvmpipe.
        let motion = pipelines.request_motion().expect("motion PSO builds");
        let _taa = pipelines
            .request_taa(descriptors.taa_set_layout())
            .expect("taa PSO builds");
        let _fxaa = pipelines
            .request_fxaa(descriptors.fxaa_set_layout())
            .expect("fxaa PSO builds");

        // A sample-count-baked mesh PSO + the depth-prepass populate the count-keyed cache.
        let lit = Material::default();
        let _mesh = pipelines
            .request_mesh_pipeline(&lit, false, false)
            .expect("mesh PSO builds");
        let _depth = pipelines
            .request_depth_prepass()
            .expect("depth-prepass builds");
        assert_eq!(pipelines.pipeline_count(), 1, "one mesh PSO cached at 1×");
        assert_eq!(pipelines.sample_count(), vk::SampleCountFlags::TYPE_1);

        // Changing the count clears the mesh cache + drops the depth-prepass so they rebuild
        // for the new count; the motion PSO (always 1×) is untouched (same Arc on re-request).
        pipelines.set_sample_count(vk::SampleCountFlags::TYPE_4);
        assert_eq!(pipelines.sample_count(), vk::SampleCountFlags::TYPE_4);
        assert_eq!(
            pipelines.pipeline_count(),
            0,
            "the sample-count change cleared the mesh cache"
        );
        let motion_again = pipelines.request_motion().expect("motion re-request");
        assert!(
            Arc::ptr_eq(&motion, &motion_again),
            "the always-1× motion PSO survives a sample-count change"
        );
        // The next mesh request rebuilds at the new count.
        let _mesh4 = pipelines
            .request_mesh_pipeline(&lit, false, false)
            .expect("mesh PSO rebuilds at 4×");
        assert_eq!(pipelines.pipeline_count(), 1);

        drop(motion);
        drop(motion_again);
        drop(_taa);
        drop(_fxaa);
        drop(_mesh);
        drop(_depth);
        drop(_mesh4);
        drop(pipelines);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the AA PSO build + teardown must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
