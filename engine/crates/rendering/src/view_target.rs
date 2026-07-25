//! The per-view offscreen render targets: the scene color (RGBA16F) and depth (D32)
//! images the scene + depth-prepass passes write, plus the thin G-buffer + screen-space
//! effect chain (AO / contact / SSGI maps + history + the per-view descriptor sets that
//! bind them), all sized to the viewport.
//!
//! This carries the full screen-space effect chain for every editor pane. The
//! screen-space images + their per-view sets live here (not on the device-shared
//! [`crate::ssao::Ssao`]) so a view switch never binds another view's images —
//! README §2's per-view borrow split applied to compute sets. The active view's targets
//! are borrowed `&mut self.views[active]` once per frame with `&Device` separate.

use ash::vk;

use crate::descriptors::{BLOOM_PASSES_PER_FRAME, Descriptors};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::pipelines::{DEPTH_FORMAT, OFFSCREEN_COLOR_FORMAT};
use crate::resources::{Buffer, Image, ImageDesc};
use crate::restir::RestirView;
use crate::ssao::{AO_FORMAT, G_NORMAL_FORMAT, ROUGHNESS_FORMAT, Ssao, mesh_set_layout};
use crate::{Device, Result};

/// One frame-in-flight's viewport shm-publish capture target: a BGRA8 image the
/// post-processed offscreen blits into (the GPU does the `RGBA16F`→BGRA8 conversion) and
/// a host-visible mapped staging buffer the BGRA8 is copied into. The blit + copy are
/// recorded into the *frame's* command buffer, so the frame's in-flight fence covers the
/// readback — no per-slot fence, no separate submit, no synchronous stall. `valid` marks
/// that a readback was recorded into this slot, so a slot whose fence has signalled holds
/// a completed frame's bytes.
pub struct ShmCaptureSlot {
    /// The `B8G8R8A8_UNORM` blit destination (TRANSFER_DST + TRANSFER_SRC, optimal).
    pub image: Image,
    /// The host-visible + mapped staging buffer holding the tightly-packed BGRA8 result.
    pub staging: Buffer,
    /// The extent the image + staging were sized for; a mismatch triggers a recreate.
    pub extent: vk::Extent2D,
    /// True once a readback was recorded into this slot — its staging holds a frame's
    /// bytes once the slot's frame fence has signalled.
    pub valid: bool,
}

/// The per-frame-in-flight ring of [`ShmCaptureSlot`]s for one view. Frame N records its
/// readback into `slots[N % MAX_FRAMES_IN_FLIGHT]`; the bytes are published from that same
/// slot `MAX_FRAMES_IN_FLIGHT` frames later, after its frame fence has signalled — so a
/// frame's copy never clobbers a still-being-read staging buffer (pipelined). Created
/// lazily on the first publish, recreated only on an extent
/// change, so the steady-state shm path allocates nothing per frame.
#[derive(Default)]
pub struct ShmCapture {
    /// One capture slot per frame-in-flight; `None` until lazily created.
    pub slots: [Option<ShmCaptureSlot>; MAX_FRAMES_IN_FLIGHT],
}

/// One editor pane's viewport-sized scene targets + screen-space effect chain.
///
/// `offscreen` is the linear-HDR scene color shown in the Viewport panel — created
/// `COLOR_ATTACHMENT | SAMPLED | TRANSFER_SRC | STORAGE` so the scene pass writes it,
/// post passes sample/store it, and capture reads it back. `depth` is the scene depth
/// the depth-prepass lays down and the scene pass tests against. The G-buffer
/// (`g_normal`/`g_depth`) + the AO/contact/SSGI maps feed the screen-space chain; the
/// per-view descriptor sets (`gtao_set`, …, `mesh_set`) bind this view's images so a
/// view switch never aliases another view's targets. `generation` bumps on every
/// recreate so consumers (descriptor rewrites) can detect a resize.
pub struct ViewTarget {
    /// The scene color render target (linear HDR, RGBA16F).
    pub offscreen: Image,
    /// The scene depth buffer (D32), sized to the viewport.
    pub depth: Image,
    /// The view's ping-pong HZB pyramids (occlusion visibility); rebuilt with the
    /// input extent under the resize idle wait.
    pub hzb_pyramid: Option<crate::HzbPyramid>,
    /// The view's instance-visibility lists (cull/retest/traversal/binning), sized to
    /// the world's instance capacity; recreated under an idle wait on growth.
    pub visibility_view: Option<crate::SceneVisibilityView>,

    /// The thin G-buffer: view normal (rgb) + view-Z (.a), the screen-space chain's
    /// shared input. `None` until the screen-space targets are built.
    pub g_normal: Option<Image>,
    /// The G-buffer's second target: per-pixel roughness (R8), the specular-occlusion prepass's
    /// cone-angle input. Written alongside `g_normal` by the gbuffer prepass.
    pub g_roughness: Option<Image>,
    /// The G-buffer prepass's own depth scratch.
    pub g_depth: Option<Image>,
    /// The raw GTAO trace output (r8).
    pub ao_raw: Option<Image>,
    /// The denoised AO map the scene samples (r8).
    pub ao_map: Option<Image>,
    /// The directional contact-shadow map the scene samples (r8).
    pub contact_map: Option<Image>,
    /// The raw one-bounce SSGI trace output (rgba16f).
    pub ssgi_map: Option<Image>,
    /// The screen-space reflection trace output (rgba16f): rgb = reflected radiance,
    /// a = hit confidence. The mesh blends it over the prefiltered-env specular.
    pub ssr_map: Option<Image>,
    /// `ssgi_map` after the bilateral blur — what the scene reads (rgba16f).
    pub ssgi_denoised: Option<Image>,
    /// `ssgi_denoised` after temporal accumulation (rgba16f). Sampled once motion is on.
    pub ssgi_resolved: Option<Image>,
    /// The raw half-res DFAO sky-visibility cone-trace output (rgba16f, r = [0,1] visibility).
    pub dfao_raw: Option<Image>,
    /// `dfao_raw` after the bilateral upsample to full res (rgba16f).
    pub dfao_denoised: Option<Image>,
    /// `dfao_denoised` after temporal accumulation — what the mesh samples (rgba16f, set 4
    /// binding 5).
    pub dfao_resolved: Option<Image>,
    /// DFAO temporal history (rgba16f), ping-pong sharing the SSGI/TAA parity.
    pub dfao_history: [Option<Image>; 2],
    /// The raw half-res specular reflection-occlusion cone-trace output (rgba16f, r = [0,1]).
    pub specocc_raw: Option<Image>,
    /// `specocc_raw` after the bilateral upsample to full res — what the mesh samples (rgba16f,
    /// set 4 binding 6). Specular occlusion is view-dependent, so it is spatial-only (no temporal
    /// accumulation): the half-res trace + this bilateral upsample are its whole denoise.
    pub specocc_denoised: Option<Image>,
    /// The half-res screen-space indirect-diffuse resolve output (rgba16f: rgb = indirect diffuse
    /// irradiance, a = view-Z for the denoiser). Additive until the fragment cutover samples it.
    pub gi_indirect: Option<Image>,
    /// The persistent previous-frame linear-HDR color SSGI gathers from (rgba16f).
    pub prev_color: Option<Image>,
    /// SSGI temporal history (rgba16f), ping-pong sharing TAA's parity.
    pub ssgi_history: [Option<Image>; 2],
    /// Reduced cloud scatter/transmittance accumulation, ping-ponging with the existing temporal
    /// history parity. Half width and half height gives one quarter of the full pixel count.
    pub cloud_reduced: [Option<Image>; 2],
    /// Reduced cloud transmittance-weighted mean front depth (r16f).
    pub cloud_reduced_depth: Option<Image>,
    /// Full-resolution premultiplied cloud scatter and transmittance (rgba16f), consumed by the
    /// atmosphere ledger.
    pub cloud_full_color: Option<Image>,
    /// Full-resolution cloud mean front depth (r32f), consumed by the atmosphere ledger.
    pub cloud_full_depth: Option<Image>,

    /// The screen-space motion-vector target (rg16f): per-pixel `prevUv - curUv`, built
    /// when TAA or SSGI is on. `None` until the temporal targets are built.
    pub motion: Option<Image>,
    /// The motion prepass's own depth scratch (D32).
    pub motion_depth: Option<Image>,
    /// TAA's two ping-pong history color images (display-format rgba16f), built when TAA
    /// is on.
    pub history: [Option<Image>; 2],
    /// TAA pixel-lock ping-pong (display-extent rgba16f, `history_index` parity): r = remaining
    /// lock lifetime in frames, g = the luma the lock was created at (its break test). A lock
    /// pins a display pixel to accumulated history through a jitter cycle so thin, sub-input-pixel
    /// features survive; it breaks on a large luma disagreement or a disocclusion. Built with TAA.
    pub lock: [Option<Image>; 2],
    /// TAA reactive coverage mask (input-extent r8): translucent / particle fragments raise it so
    /// the resolve biases those pixels toward the current frame (history reprojects poorly through
    /// alpha-blended content). Written by the reactive-coverage pass, read by the TAA resolve.
    pub reactive: Option<Image>,
    /// The display-extent overlay depth: a point-upscale of the input-extent scene [`depth`] so
    /// the grid / gizmo (drawn on the display-extent resolved color) depth-test correctly under
    /// temporal upsampling. Built whenever TAA/FXAA/no-AA can run (i.e. always with AA targets).
    pub depth_display: Option<Image>,
    /// The multisampled scene color + depth the scene renders into when MSAA is active
    /// (resolved into `offscreen` / `depth`). `None` when MSAA is off.
    pub msaa_color: Option<Image>,
    /// The multisampled scene depth (resolve-into-`depth`).
    pub msaa_depth: Option<Image>,
    /// The 1× scratch the scene renders into when FXAA or TAA is active (a compute pass
    /// then resolves it into `offscreen`). `None` when neither is on.
    pub scratch: Option<Image>,

    /// This frame writes `ssgi_history[history_index]`, reads the other.
    pub history_index: usize,
    /// False on the first frame / after a resize (no temporal history yet).
    pub history_valid: bool,

    /// Last frame's camera viewProj (this view's own), driving the motion prepass's camera
    /// reprojection. Invalid until the first frame stores one. Per-view so a re-activated
    /// view reprojects against its own last frame.
    pub prev_view_proj: saffron_geometry::glam::Mat4,
    /// False until the first frame stores `prev_view_proj`.
    pub prev_view_proj_valid: bool,
    /// The Halton jitter phase index, advanced once per rendered frame while TAA is active.
    /// Per-view for the same reason as `prev_view_proj`: a re-activated view restarts cleanly.
    pub jitter_index: u32,
    /// This frame's sub-pixel jitter offset (NDC), applied to the scene view-projection as a
    /// clip-space translation. Zero while TAA is inactive.
    pub jitter: saffron_geometry::glam::Vec2,
    /// Last frame's jitter offset (NDC), rolled from `jitter` at the frame tail.
    pub prev_jitter: saffron_geometry::glam::Vec2,

    /// gtao: g_normal + ao_raw (compute2).
    pub gtao_set: vk::DescriptorSet,
    /// ao_blur: ao_raw + g_normal + ao_map (compute3).
    pub ao_blur_set: vk::DescriptorSet,
    /// contact: g_normal + contact_map (compute2).
    pub contact_set: vk::DescriptorSet,
    /// ssgi: g_normal + prev_color + ssgi_map (compute3).
    pub ssgi_set: vk::DescriptorSet,
    /// ssr: g_normal + prev_color + ssr_map (compute3).
    pub ssr_set: vk::DescriptorSet,
    /// ssgi_blur: ssgi_map + g_normal + ssgi_denoised (compute3).
    pub ssgi_blur_set: vk::DescriptorSet,
    /// dfao trace (set 2 of the trace pipeline): g_normal + dfao_raw (compute2).
    pub dfao_set: vk::DescriptorSet,
    /// dfao-blur: dfao_raw + g_normal + dfao_denoised (compute3, reuses the ssgi-blur PSO).
    pub dfao_blur_set: vk::DescriptorSet,
    /// dfao-accum (taa-shape: 3 samplers + 2 storage), ping-pong by `history_index` (reuses the
    /// ssgi-accum PSO).
    pub dfao_accum_sets: [vk::DescriptorSet; 2],
    /// specocc trace (set 2 of the trace pipeline): g_normal + g_roughness + specocc_raw (compute3).
    pub specocc_set: vk::DescriptorSet,
    /// specocc-blur: specocc_raw + g_normal + specocc_denoised (compute3, reuses the ssgi-blur PSO).
    pub specocc_blur_set: vk::DescriptorSet,
    /// copy_color: offscreen + prev_color (compute2).
    pub copy_color_set: vk::DescriptorSet,
    /// scene-resolve: input-extent scratch sampler + display-extent offscreen storage (compute2,
    /// the copy_color layout). The no-AA / MSAA path's normalized-UV upscale scratch → offscreen.
    pub scene_resolve_set: vk::DescriptorSet,
    /// depth-upscale: input-extent scene depth sampler (the depth_upscale graphics layout, one
    /// fragment sampler) feeding the display-extent overlay depth pass.
    pub depth_upscale_set: vk::DescriptorSet,
    /// gi-resolve per-frame-slot sets (the single `gi_resolve_layout`). Image bindings (G-buffer,
    /// output, dfao, IBL cube, DDGI atlases) are stable and written once; each slot binds its own
    /// `gi_params_ubos[i]` at b2, whose contents are memcpy'd per frame — so no per-frame descriptor
    /// rewrite, no descriptor-in-flight hazard.
    pub gi_resolve_sets: [vk::DescriptorSet; MAX_FRAMES_IN_FLIGHT],
    /// Per-frame-slot `GiParams` UBOs (host-visible, persistently mapped), one per gi-resolve set.
    pub gi_params_ubos: Vec<Buffer>,
    /// ssgi-accum (taa-shape: 3 samplers + 2 storage), ping-pong by `history_index`.
    pub ssgi_accum_sets: [vk::DescriptorSet; 2],
    /// fxaa: scratch source sampler + offscreen storage (compute2-shape, fxaa layout).
    pub fxaa_set: vk::DescriptorSet,
    /// taa (taa-shape: 3 samplers scratch/history/motion + 2 storage offscreen/history),
    /// ping-pong by `history_index`.
    pub taa_sets: [vk::DescriptorSet; 2],
    /// set 4 in the mesh pipeline: ao_map + contact_map + resolved SSGI.
    pub mesh_set: vk::DescriptorSet,
    /// The mandatory tonemap set: binding 0 = the offscreen color as a storage image
    /// (GENERAL), binding 1 = the per-view grade UBO (a dynamic-offset UBO). Rewritten when the
    /// offscreen recreates.
    pub tonemap_set: vk::DescriptorSet,
    /// The per-view grade uniform: `MAX_FRAMES_IN_FLIGHT` `GradeUniform` slices, each aligned to
    /// [`grade_ubo_stride`](ViewTarget::grade_ubo_stride), host-visible + persistently mapped. The
    /// renderer memcpy's this frame's slice each frame; the tonemap dispatch selects it by a dynamic
    /// offset, so binding 1 is written once (no per-frame descriptor rewrite). Allocated in
    /// [`ViewTarget::new`], outlives resizes.
    pub grade_ubo: Buffer,
    /// The aligned byte stride of one `GradeUniform` slice in [`ViewTarget::grade_ubo`] — the
    /// dynamic offset for frame `f` is `f * grade_ubo_stride`.
    pub grade_ubo_stride: u64,
    /// The height-fog composite set: binding 0 = the offscreen color as a storage image (GENERAL),
    /// binding 1 = the per-view `FogParams` UBO (a dynamic-offset slice), binding 2 = the scene
    /// depth, binding 3 = the sky-view LUT. Rewritten (bindings 0/2) when the targets recreate; the
    /// LUT (3) is written once by the renderer.
    pub fog_set: vk::DescriptorSet,
    /// The per-view fog uniform: `MAX_FRAMES_IN_FLIGHT` `FogParams` slices, each aligned to
    /// [`fog_ubo_stride`](ViewTarget::fog_ubo_stride), host-visible + persistently mapped. The
    /// renderer writes this frame's slice each frame; the fog dispatch selects it by a dynamic
    /// offset. Allocated in [`ViewTarget::new`], outlives resizes.
    pub fog_ubo: Buffer,
    /// The aligned byte stride of one `FogParams` slice in [`ViewTarget::fog_ubo`].
    pub fog_ubo_stride: u64,
    /// The bloom pyramid sets — one per pass, allocated per frame-in-flight because the bloom mip
    /// images come from the per-slot transient pool. Indexed `slot * BLOOM_PASSES_PER_FRAME + pass`
    /// and rewritten each frame in [`ViewTarget::write_bloom_sets`] once that frame's transient mips
    /// are acquired.
    pub bloom_sets: Vec<vk::DescriptorSet>,
    /// motion-vector visualization: motion sampler + offscreen storage (compute2-shape).
    /// Bound by `write_aa_sets`; the visualize pass runs only when the motion target exists.
    pub motion_vis_set: vk::DescriptorSet,

    /// This view's ReSTIR DI reservoirs + radiance + sets + temporal state, sized to the
    /// viewport. Rides alongside the view so two views never read each other's reservoirs
    /// (README §2). Inert on a software device.
    pub restir: RestirView,

    /// The render size the UI panel last requested for this view (device pixels), `0` until
    /// the view has been sized at least once. A view's desired size is set out-of-band (the
    /// `set-viewport-size` control command, the host window resize); the offscreen is
    /// recreated to match. Read to tell whether a not-yet-shown view (the asset-preview pane
    /// before it is opened) has been seeded.
    pub desired_width: u32,
    /// The render height the UI panel last requested for this view. See [`ViewTarget::desired_width`].
    pub desired_height: u32,
    /// Dynamic-resolution factor in `(0, 1]`: the render targets are sized to
    /// `round(desired * render_scale)` while the published frame stays at the desired
    /// (native) size — the present blit upscales. `1.0` renders at native resolution. The
    /// frame-budget controller lowers this to hold the budget on weak hardware.
    pub render_scale: f32,

    /// Bumped whenever the targets are recreated (a resize).
    pub generation: u32,

    /// The per-frame-in-flight BGRA8 shm-publish capture ring, allocated lazily on the
    /// first shm readback and recreated only on an extent change. Empty until the view is
    /// first published (the asset-preview pane before it is shown never allocates one). The
    /// [`Image`]/[`Buffer`] slots Drop at renderer teardown after `wait_idle`.
    pub shm_capture: ShmCapture,
}

impl ViewTarget {
    /// Creates the offscreen color + depth images at `(width, height)`. The
    /// screen-space images + sets are not built here — [`ViewTarget::allocate_screen_space_sets`]
    /// allocates the per-view sets once, and [`ViewTarget::build_screen_space`] (re)creates
    /// the images + writes the sets (at init + every resize).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if either image/view cannot be created.
    pub fn new(device: &Device, width: u32, height: u32) -> Result<Self> {
        let extent = vk::Extent2D { width, height };
        let resources = device.resources();

        let offscreen = Image::new(
            resources,
            &ImageDesc::color_2d(
                extent,
                OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::STORAGE,
            ),
        )?;

        let depth_desc = ImageDesc {
            extent,
            format: DEPTH_FORMAT,
            usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            aspect: vk::ImageAspectFlags::DEPTH,
            view_type: vk::ImageViewType::TYPE_2D,
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        };
        let depth = Image::new(resources, &depth_desc)?;

        // The per-view grade UBO: one aligned `GradeUniform` slice per frame-in-flight, host-visible
        // + persistently mapped. Bound once as a dynamic-offset UBO; the renderer writes this frame's
        // slice each frame and the dispatch selects it by `frame * stride`.
        let grade_ubo_stride = align_up(
            size_of::<crate::GradeUniform>() as u64,
            device
                .capabilities
                .min_uniform_buffer_offset_alignment
                .max(1),
        );
        let grade_ubo = Buffer::new(
            resources,
            grade_ubo_stride * MAX_FRAMES_IN_FLIGHT as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        // The per-view fog UBO: one aligned `FogParams` slice per frame-in-flight, host-visible +
        // persistently mapped. Bound once as a dynamic-offset UBO; the renderer writes this frame's
        // slice each frame and the dispatch selects it by `frame * stride`.
        let fog_ubo_stride = align_up(
            size_of::<crate::renderer::FogParams>() as u64,
            device
                .capabilities
                .min_uniform_buffer_offset_alignment
                .max(1),
        );
        let fog_ubo = Buffer::new(
            resources,
            fog_ubo_stride * MAX_FRAMES_IN_FLIGHT as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        Ok(Self {
            offscreen,
            depth,
            hzb_pyramid: None,
            visibility_view: None,
            g_normal: None,
            g_roughness: None,
            g_depth: None,
            ao_raw: None,
            ao_map: None,
            contact_map: None,
            ssgi_map: None,
            ssr_map: None,
            ssgi_denoised: None,
            ssgi_resolved: None,
            dfao_raw: None,
            dfao_denoised: None,
            dfao_resolved: None,
            dfao_history: [None, None],
            specocc_raw: None,
            specocc_denoised: None,
            gi_indirect: None,
            prev_color: None,
            ssgi_history: [None, None],
            cloud_reduced: [None, None],
            cloud_reduced_depth: None,
            cloud_full_color: None,
            cloud_full_depth: None,
            motion: None,
            motion_depth: None,
            history: [None, None],
            lock: [None, None],
            reactive: None,
            depth_display: None,
            msaa_color: None,
            msaa_depth: None,
            scratch: None,
            history_index: 0,
            history_valid: false,
            prev_view_proj: saffron_geometry::glam::Mat4::IDENTITY,
            prev_view_proj_valid: false,
            jitter_index: 0,
            jitter: saffron_geometry::glam::Vec2::ZERO,
            prev_jitter: saffron_geometry::glam::Vec2::ZERO,
            gtao_set: vk::DescriptorSet::null(),
            ao_blur_set: vk::DescriptorSet::null(),
            contact_set: vk::DescriptorSet::null(),
            ssgi_set: vk::DescriptorSet::null(),
            ssr_set: vk::DescriptorSet::null(),
            ssgi_blur_set: vk::DescriptorSet::null(),
            dfao_set: vk::DescriptorSet::null(),
            dfao_blur_set: vk::DescriptorSet::null(),
            dfao_accum_sets: [vk::DescriptorSet::null(); 2],
            specocc_set: vk::DescriptorSet::null(),
            specocc_blur_set: vk::DescriptorSet::null(),
            copy_color_set: vk::DescriptorSet::null(),
            scene_resolve_set: vk::DescriptorSet::null(),
            depth_upscale_set: vk::DescriptorSet::null(),
            gi_resolve_sets: [vk::DescriptorSet::null(); MAX_FRAMES_IN_FLIGHT],
            gi_params_ubos: Vec::new(),
            motion_vis_set: vk::DescriptorSet::null(),
            ssgi_accum_sets: [vk::DescriptorSet::null(); 2],
            fxaa_set: vk::DescriptorSet::null(),
            bloom_sets: Vec::new(),
            taa_sets: [vk::DescriptorSet::null(); 2],
            mesh_set: vk::DescriptorSet::null(),
            tonemap_set: vk::DescriptorSet::null(),
            grade_ubo,
            grade_ubo_stride,
            fog_set: vk::DescriptorSet::null(),
            fog_ubo,
            fog_ubo_stride,
            restir: RestirView::new(),
            desired_width: width,
            desired_height: height,
            render_scale: 1.0,
            generation: 1,
            shm_capture: ShmCapture::default(),
        })
    }

    /// Ensures frame slot `slot`'s BGRA8 shm-capture target exists at `extent`, (re)creating
    /// it on an extent change. The caller has waited this
    /// slot's frame fence, so the previous target is idle and freed when replaced; recreating
    /// drops `valid` (no completed bytes at the new size yet). Returns [`crate::Error::Vk`]
    /// if the device lacks BLIT_SRC on the offscreen format or BLIT_DST on BGRA8 (optimal
    /// tiling), or any allocation fails.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] on an unsupported blit format or a failing allocation.
    pub fn ensure_shm_capture(
        &mut self,
        device: &Device,
        slot: usize,
        extent: vk::Extent2D,
    ) -> Result<()> {
        if let Some(capture) = self.shm_capture.slots[slot].as_ref()
            && capture.extent == extent
        {
            return Ok(());
        }
        require_shm_blit_support(device, self.offscreen.format)?;

        let resources = device.resources();
        // No view: a TRANSFER-only image cannot back an image view, and the blit/copy
        // address it by handle + layout, never through a view.
        let image = Image::new_no_view(
            resources,
            &ImageDesc::color_2d(
                extent,
                vk::Format::B8G8R8A8_UNORM,
                vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC,
            ),
        )?;
        let bytes = vk::DeviceSize::from(extent.width) * vk::DeviceSize::from(extent.height) * 4;
        let staging = Buffer::new(
            resources,
            bytes,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        self.shm_capture.slots[slot] = Some(ShmCaptureSlot {
            image,
            staging,
            extent,
            valid: false,
        });
        Ok(())
    }

    /// Teardown hook for the shm-capture ring. The slots own no raw handles beyond their
    /// [`Image`]/[`Buffer`] (which Drop), so this only drops the ring; kept as the explicit
    /// teardown seam the renderer calls under `wait_idle`.
    pub fn destroy(&mut self, _device: &Device) {
        self.shm_capture = ShmCapture::default();
    }

    /// Allocates this view's per-view screen-space descriptor sets once. The sets are
    /// rewritten by
    /// [`ViewTarget::build_screen_space`] whenever the images recreate; allocating
    /// them once (not per resize) avoids churning the pool.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if any `vkAllocateDescriptorSets` fails.
    pub fn allocate_screen_space_sets(
        &mut self,
        descriptors: &Descriptors,
        ssao: &Ssao,
    ) -> Result<()> {
        self.gtao_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ao_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.contact_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ssgi_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.ssr_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.ssgi_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.dfao_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.dfao_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.dfao_accum_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        // Specular occlusion: the trace's I/O set is the compute3 shape (two samplers — the
        // G-buffer + roughness — plus the storage image), unlike DFAO's compute2 trace.
        self.specocc_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.specocc_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.copy_color_set = descriptors.allocate_set(ssao.compute2_layout())?;
        // The scene-resolve copy (input scratch -> display offscreen) shares the copy_color
        // compute2 shape; the depth-upscale set is the single fragment-sampler graphics layout.
        self.scene_resolve_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.depth_upscale_set = descriptors.allocate_set(descriptors.depth_upscale_layout())?;
        for slot in &mut self.gi_resolve_sets {
            *slot = descriptors.allocate_set(ssao.gi_resolve_layout())?;
        }
        self.motion_vis_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ssgi_accum_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        self.fxaa_set = descriptors.allocate_set(descriptors.fxaa_set_layout())?;
        self.taa_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        self.mesh_set = descriptors.allocate_set(mesh_set_layout(descriptors))?;
        self.tonemap_set = descriptors.allocate_set(descriptors.tonemap_set_layout())?;
        self.fog_set = descriptors.allocate_set(descriptors.fog_set_layout())?;
        // Bloom binds one source/target pair per pyramid pass, allocated per frame-in-flight so a
        // slot's sets are only rewritten `MAX_FRAMES_IN_FLIGHT` frames after their last GPU use
        // (the transient mip images they bind are themselves per-slot).
        self.bloom_sets.clear();
        for _ in 0..(BLOOM_PASSES_PER_FRAME * MAX_FRAMES_IN_FLIGHT) {
            self.bloom_sets
                .push(descriptors.allocate_set(descriptors.bloom_set_layout())?);
        }
        Ok(())
    }

    /// (Re)creates the screen-space images at the current viewport extent and writes
    /// every per-view set to bind them, transitioning the mesh-sampled maps + prevColor
    /// to `SHADER_READ_ONLY_OPTIMAL` so set 4 is valid even before the passes first run
    /// (each read is gated by its enable flag in the übershader). Resets the SSGI
    /// history validity (a resize invalidates the reprojection). `ssao` supplies the
    /// device-shared nearest G-buffer sampler the set writes bind.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_screen_space(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        ssao: &Ssao,
    ) -> Result<()> {
        // The whole screen-space / G-buffer chain rasterises at the INPUT (render) extent.
        let extent = self.scaled_render_extent();
        if extent.width == 0 || extent.height == 0 {
            return Ok(());
        }
        let resources = device.resources();

        let storage_sampled = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::STORAGE;
        // The G-buffer is a color attachment + sampled only (never a storage image).
        let g_normal = Image::new(
            resources,
            &ImageDesc::color_2d(
                extent,
                G_NORMAL_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            ),
        )?;
        // The G-buffer's roughness target (R8): a color attachment + sampled, like g_normal.
        let g_roughness = Image::new(
            resources,
            &ImageDesc::color_2d(
                extent,
                ROUGHNESS_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            ),
        )?;
        let g_depth = Image::new(
            resources,
            &ImageDesc {
                extent,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;
        // SSGI + GTAO trace into half-resolution targets (`ao_raw`, `ssgi_map`); the bilateral
        // `ao-blur` / `ssgi-blur` passes upsample them back to full-res against the full-res
        // G-buffer depth. Halving the ray-march raster is the bulk of the screen-space GI cost.
        // Round up so an odd extent still covers every full-res pixel after the 2x upsample.
        let half_extent = vk::Extent2D {
            width: extent.width.div_ceil(2).max(1),
            height: extent.height.div_ceil(2).max(1),
        };
        let display_extent = self.published_extent();
        let cloud_extent = vk::Extent2D {
            width: display_extent.width.div_ceil(2).max(1),
            height: display_extent.height.div_ceil(2).max(1),
        };
        let ao_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, AO_FORMAT, storage_sampled),
        )?;
        let ao_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, AO_FORMAT, storage_sampled),
        )?;
        let contact_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, AO_FORMAT, storage_sampled),
        )?;
        let ssgi_map = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssr_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssgi_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssgi_resolved = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        // DFAO: the cone trace runs at half res (like GTAO/SSGI), then a bilateral upsample
        // (`dfao_denoised`) + temporal accumulation (`dfao_resolved` + history) bring it to full
        // res and denoise it across frames.
        let dfao_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let dfao_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let dfao_resolved = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut dfao_history_0 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut dfao_history_1 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        // Specular occlusion is spatial-only (view-dependent — no temporal reuse): a half-res
        // trace + a full-res bilateral upsample, both rgba16f.
        let specocc_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let specocc_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut prev_color = Image::new(
            resources,
            &ImageDesc::color_2d(extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut ssgi_history_0 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut ssgi_history_1 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_0 = Image::new(
            resources,
            &ImageDesc::color_2d(cloud_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_1 = Image::new(
            resources,
            &ImageDesc::color_2d(cloud_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_depth = Image::new(
            resources,
            &ImageDesc::color_2d(
                cloud_extent,
                vk::Format::R16_SFLOAT,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE,
            ),
        )?;
        let mut cloud_full_color = Image::new(
            resources,
            &ImageDesc::color_2d(display_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_full_depth = Image::new(
            resources,
            &ImageDesc::color_2d(
                display_extent,
                vk::Format::R32_SFLOAT,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE,
            ),
        )?;
        let mut gi_indirect = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut ao_map = ao_map;
        let mut contact_map = contact_map;
        let mut ssgi_map = ssgi_map;
        let mut ssr_map = ssr_map;
        let mut ssgi_denoised = ssgi_denoised;
        let mut ssgi_resolved = ssgi_resolved;
        let mut dfao_raw = dfao_raw;
        let mut dfao_denoised = dfao_denoised;
        let mut dfao_resolved = dfao_resolved;
        let mut specocc_raw = specocc_raw;
        let mut specocc_denoised = specocc_denoised;

        // Transition the mesh-sampled maps + prevColor + the SSGI history to
        // ShaderReadOnly so their descriptors are valid even before the passes run (the
        // shader gates each read), and so the SSGI / mesh samplers + the graph's seed
        // layout agree. A one-time init transition. The
        // storage-only scratch (ao_raw, ssgi_map written first by their producing pass)
        // stay UNDEFINED until the graph transitions them — except ssgi_map, also read
        // as a sampler by ssgi_blur, so it is seeded too.
        let read_only: [&Image; 22] = [
            &ao_map,
            &contact_map,
            &ssgi_map,
            &ssr_map,
            &ssgi_denoised,
            &ssgi_resolved,
            &dfao_raw,
            &dfao_denoised,
            &dfao_resolved,
            &dfao_history_0,
            &dfao_history_1,
            &specocc_raw,
            &specocc_denoised,
            &prev_color,
            &ssgi_history_0,
            &ssgi_history_1,
            &cloud_reduced_0,
            &cloud_reduced_1,
            &cloud_reduced_depth,
            &cloud_full_color,
            &cloud_full_depth,
            &gi_indirect,
        ];
        initialize_screen_space_layouts(device, &read_only)?;
        for image in [
            &mut ao_map,
            &mut contact_map,
            &mut ssgi_map,
            &mut ssr_map,
            &mut ssgi_denoised,
            &mut ssgi_resolved,
            &mut dfao_raw,
            &mut dfao_denoised,
            &mut dfao_resolved,
            &mut dfao_history_0,
            &mut dfao_history_1,
            &mut specocc_raw,
            &mut specocc_denoised,
            &mut prev_color,
            &mut ssgi_history_0,
            &mut ssgi_history_1,
            &mut cloud_reduced_0,
            &mut cloud_reduced_1,
            &mut cloud_reduced_depth,
            &mut cloud_full_color,
            &mut cloud_full_depth,
            &mut gi_indirect,
        ] {
            image.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }

        // The gi-resolve half-res indirect-diffuse output + its per-frame-slot params UBOs (mapped,
        // memcpy'd per frame — the descriptor sets stay stable so there is no in-flight hazard).
        let gi_params_size = size_of::<crate::ssao::GiParams>() as vk::DeviceSize;
        let mut gi_params_ubos = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            gi_params_ubos.push(Buffer::new(
                resources,
                gi_params_size,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }

        self.g_normal = Some(g_normal);
        self.g_roughness = Some(g_roughness);
        self.g_depth = Some(g_depth);
        self.ao_raw = Some(ao_raw);
        self.ao_map = Some(ao_map);
        self.contact_map = Some(contact_map);
        self.ssgi_map = Some(ssgi_map);
        self.ssr_map = Some(ssr_map);
        self.ssgi_denoised = Some(ssgi_denoised);
        self.ssgi_resolved = Some(ssgi_resolved);
        self.dfao_raw = Some(dfao_raw);
        self.dfao_denoised = Some(dfao_denoised);
        self.dfao_resolved = Some(dfao_resolved);
        self.dfao_history = [Some(dfao_history_0), Some(dfao_history_1)];
        self.specocc_raw = Some(specocc_raw);
        self.specocc_denoised = Some(specocc_denoised);
        self.gi_indirect = Some(gi_indirect);
        self.gi_params_ubos = gi_params_ubos;
        self.prev_color = Some(prev_color);
        self.ssgi_history = [Some(ssgi_history_0), Some(ssgi_history_1)];
        self.cloud_reduced = [Some(cloud_reduced_0), Some(cloud_reduced_1)];
        self.cloud_reduced_depth = Some(cloud_reduced_depth);
        self.cloud_full_color = Some(cloud_full_color);
        self.cloud_full_depth = Some(cloud_full_depth);
        // A resize invalidates the temporal reprojection; the next frame re-seeds.
        self.history_valid = false;
        self.history_index = 0;

        self.write_screen_space_sets(device, descriptors, ssao.nearest_sampler());
        Ok(())
    }

    /// (Re)creates the AA targets for the active mode — the motion-vector target + its
    /// depth scratch (built when TAA *or* SSGI is on, since both need it), TAA's two
    /// ping-pong history images (when TAA is on), the FXAA/TAA 1× scratch (when either is
    /// on), and the MSAA multisampled scene color + depth (when MSAA is on) — then rewrites
    /// the FXAA + TAA sets and repoints the ssgi-accum binding 2 + mesh set-4 SSGI sampler
    /// for the new mode. Resets the temporal validity (a mode change / resize invalidates
    /// reprojection). Call after [`ViewTarget::build_screen_space`] (it depends on the
    /// freshly built SSGI maps) at init, every resize, and every AA change.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_aa_targets(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
    ) -> Result<()> {
        self.build_aa_targets_impl(device, descriptors, aa, false)
    }

    /// Rebuilds the AA targets for a render-scale-only change (dynamic resolution): recreates the
    /// INPUT-extent members (motion, its depth, the scene scratch, the reactive mask) while
    /// PRESERVING the DISPLAY-extent TAA history + lock ping-pong and `history_valid`. The resolve
    /// resamples the (now differently-sized) input into the fixed display grid every frame and
    /// motion reprojects in resolution-independent UV space, so the accumulator rides an input
    /// change — flushing it would flicker at every budget step (the bug this variant prevents).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_aa_targets_preserving_temporal(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
    ) -> Result<()> {
        self.build_aa_targets_impl(device, descriptors, aa, true)
    }

    fn build_aa_targets_impl(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
        preserve_temporal: bool,
    ) -> Result<()> {
        // Two extent classes: the scene / motion / MSAA targets rasterise at INPUT extent; the
        // history + overlay depth live at DISPLAY extent (where the resolve reconstructs).
        let input = self.scaled_render_extent();
        let display = self.published_extent();
        // Drop the previous mode's INPUT-extent targets; rebuilt below for the active mode. The
        // DISPLAY-extent history + lock are preserved on a scale-only change (see below).
        self.motion = None;
        self.motion_depth = None;
        self.reactive = None;
        self.depth_display = None;
        self.scratch = None;
        self.msaa_color = None;
        self.msaa_depth = None;
        if !preserve_temporal {
            // A mode change / resize invalidates the temporal reprojection + ping-pong parity, and
            // restarts the jitter cycle (seed phase 0 so the first frame is already jittered). The
            // jitter is a fraction of an INPUT pixel — the scene renders at input extent.
            self.history = [None, None];
            self.lock = [None, None];
            self.history_valid = false;
            self.history_index = 0;
            self.prev_view_proj_valid = false;
            self.jitter_index = 0;
            self.jitter = crate::jitter_offset(0, input.width, input.height);
            self.prev_jitter = self.jitter;
        }
        if input.width == 0 || input.height == 0 || display.width == 0 || display.height == 0 {
            return Ok(());
        }
        let resources = device.resources();
        let storage_sampled = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::STORAGE;

        // The motion target is built whenever the screen-space chain exists,
        // unconditionally: both TAA and SSGI reproject
        // through it, and which one runs is gated per frame, not by the target's existence.
        let need_motion = self.ssgi_resolved.is_some();
        if need_motion {
            self.motion = Some(Image::new(
                resources,
                &ImageDesc::color_2d(
                    input,
                    crate::MOTION_FORMAT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                ),
            )?);
            self.motion_depth = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: DEPTH_FORMAT,
                    // SAMPLED so the TAA resolve can read it for closest-depth velocity dilation.
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                },
            )?);
        }

        // TAA's two DISPLAY-extent ping-pong history images + the lock ping-pong (storage +
        // sampled) — the reconstruction accumulator + reconstruction state live at display
        // resolution. On a render-scale-only change these are PRESERVED (`preserve_temporal`): the
        // display extent is unchanged, so the accumulated history rides the input-extent change.
        if aa.taa() {
            if !preserve_temporal {
                let mut history_0 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                let mut history_1 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                // The history images rest ShaderReadOnly so their sampler bindings are valid
                // before the first TAA write (`history_valid` gates the actual blend).
                initialize_screen_space_layouts(device, &[&history_0, &history_1])?;
                history_0.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                history_1.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                self.history = [Some(history_0), Some(history_1)];

                // The pixel-lock ping-pong (DISPLAY extent, same parity as history): storage +
                // sampled, resting ShaderReadOnly so slot 7's binding is valid before the first write.
                let mut lock_0 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                let mut lock_1 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                initialize_screen_space_layouts(device, &[&lock_0, &lock_1])?;
                lock_0.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                lock_1.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                self.lock = [Some(lock_0), Some(lock_1)];
            }

            // The reactive coverage mask (INPUT extent, r8): the coverage pass writes it, the
            // resolve samples it. Rest ShaderReadOnly so slot 6's binding is valid before the
            // first coverage write (a scene with no translucent content leaves it cleared to 0).
            let mut reactive = Image::new(
                resources,
                &ImageDesc::color_2d(
                    input,
                    crate::REACTIVE_FORMAT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                ),
            )?;
            initialize_screen_space_layouts(device, &[&reactive])?;
            reactive.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            self.reactive = Some(reactive);
        }

        // The INPUT-extent scene-color scratch: the scene always rasterises into it, and the
        // resolve stage (FXAA / TAA, or the no-AA copy) always writes the DISPLAY-extent offscreen
        // — so a graphics pass never mixes attachment extents. Allocated unconditionally.
        self.scratch = Some(Image::new(
            resources,
            &ImageDesc::color_2d(
                input,
                OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            ),
        )?);

        // The DISPLAY-extent overlay depth: point-upscaled from the input-extent scene depth each
        // frame (the depth-upscale pass) so the grid / gizmo depth-test on the display grid. Built
        // whenever AA targets exist (the overlays run in every AA mode).
        self.depth_display = Some(Image::new(
            resources,
            &ImageDesc {
                extent: display,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?);

        // The MSAA multisampled scene color + depth (INPUT extent, resolved into scratch / depth).
        if aa.msaa() {
            self.msaa_color = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: OFFSCREEN_COLOR_FORMAT,
                    usage: vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    aspect: vk::ImageAspectFlags::COLOR,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: aa.sample_count(),
                },
            )?);
            self.msaa_depth = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: DEPTH_FORMAT,
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: aa.sample_count(),
                },
            )?);
        }

        self.write_aa_sets(device, descriptors, aa);
        Ok(())
    }

    /// Writes the FXAA + TAA sets and repoints the ssgi-accum binding 2 (motion) + the mesh
    /// set-4 SSGI sampler for the active AA mode. The TAA / FXAA scene input is the scratch
    /// image when built, else the offscreen as a valid placeholder (the set is unused until
    /// that mode turns on + rebinds).
    fn write_aa_sets(&self, device: &Device, descriptors: &Descriptors, aa: crate::Aa) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let scene_input = self
            .scratch
            .as_ref()
            .map_or(self.offscreen.view(), Image::view);
        let offscreen = self.offscreen.view();
        let motion = self.aa_view(&self.motion);
        let motion_depth = self.aa_view(&self.motion_depth);
        let reactive = self.aa_view(&self.reactive);
        let ssgi_denoised = self.view_of(&self.ssgi_denoised);
        let ssgi_resolved = self.view_of(&self.ssgi_resolved);
        let dfao_denoised = self.view_of(&self.dfao_denoised);
        let dfao_resolved = self.view_of(&self.dfao_resolved);

        let mut plan: Vec<Binding> = vec![
            // fxaa: scratch source sampler -> offscreen storage.
            Binding::sampled(self.fxaa_set, 0, linear, scene_input),
            Binding::storage(self.fxaa_set, 1, offscreen),
            // motion-vector visualization: motion sampler -> offscreen storage. `motion` is
            // the offscreen placeholder when not built; the visualize pass runs only when the
            // real target exists, so the placeholder is never sampled.
            Binding::sampled(self.motion_vis_set, 0, linear, motion),
            Binding::storage(self.motion_vis_set, 1, offscreen),
            // mesh set 4 binding 2: the SSGI map the scene samples — the temporally
            // resolved map when TAA is on, the spatially denoised map otherwise.
            Binding::sampled_image(
                self.mesh_set,
                2,
                if aa.taa() {
                    ssgi_resolved
                } else {
                    ssgi_denoised
                },
            ),
            // Scene-resolve copy (no-AA / MSAA path): the input-extent scratch sampler -> the
            // display-extent offscreen storage, a normalized-UV upscale dispatched at display.
            Binding::sampled(self.scene_resolve_set, 0, linear, scene_input),
            Binding::storage(self.scene_resolve_set, 1, offscreen),
            // Depth-upscale: the input-extent scene depth sampler feeding the display-extent
            // overlay depth (the fragment point-samples it per display pixel).
            Binding::sampled(self.depth_upscale_set, 0, linear, self.depth.view()),
        ];
        // TAA parities: parity p reads scratch/history[1-p]/motion, writes offscreen +
        // history[p]. The ssgi-accum binding 2 (motion) is rebound to the real motion
        // target now that it exists (build_screen_space seeded denoised as a placeholder).
        for p in 0..2usize {
            let taa = self.taa_sets[p];
            plan.push(Binding::sampled(taa, 0, linear, scene_input));
            plan.push(Binding::sampled(
                taa,
                1,
                linear,
                self.taa_history_view(1 - p),
            ));
            plan.push(Binding::sampled(taa, 2, linear, motion));
            plan.push(Binding::storage(taa, 3, offscreen));
            plan.push(Binding::storage(taa, 4, self.taa_history_view(p)));
            // Motion-prepass depth for closest-depth velocity dilation (placeholder when TAA
            // is off, never sampled until the mode turns on and rebinds).
            plan.push(Binding::sampled(taa, 5, linear, motion_depth));
            // Phase 3: reactive coverage (6), the previous lock read at the reprojected UV (7,
            // the opposite parity), and this frame's lock write (8, this parity).
            plan.push(Binding::sampled(taa, 6, linear, reactive));
            plan.push(Binding::sampled(taa, 7, linear, self.taa_lock_view(1 - p)));
            plan.push(Binding::storage(taa, 8, self.taa_lock_view(p)));

            let accum = self.ssgi_accum_sets[p];
            plan.push(Binding::sampled(accum, 0, linear, ssgi_denoised));
            plan.push(Binding::sampled(accum, 1, linear, self.history_view(1 - p)));
            plan.push(Binding::sampled(accum, 2, linear, motion));
            plan.push(Binding::storage(accum, 3, ssgi_resolved));
            plan.push(Binding::storage(accum, 4, self.history_view(p)));

            // The dfao-accum motion binding (2) is likewise rebound to the real motion target.
            let dfao = self.dfao_accum_sets[p];
            plan.push(Binding::sampled(dfao, 0, linear, dfao_denoised));
            plan.push(Binding::sampled(
                dfao,
                1,
                linear,
                self.dfao_history_view(1 - p),
            ));
            plan.push(Binding::sampled(dfao, 2, linear, motion));
            plan.push(Binding::storage(dfao, 3, dfao_resolved));
            plan.push(Binding::storage(dfao, 4, self.dfao_history_view(p)));
        }

        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: the ash seam. The sets/views/samplers outlive the renderer; host access
        // to these per-view sets is single-threaded at the (idle) build point.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// Rewrites this frame slot's bloom sets to bind the passed `(source, target)` view pairs — one
    /// pair per pyramid pass, in graph order (downsamples, then upsamples, then the composite). The
    /// bloom mip images come from the per-frame-in-flight transient pool, so only slot `frame`'s
    /// sets are touched; that slot's previous GPU use finished `MAX_FRAMES_IN_FLIGHT` frames ago
    /// (its fence was waited at frame begin), so the update is hazard-free. The source binds through
    /// the linear clamp sampler (binding 0, `SHADER_READ_ONLY`), the target as a storage image
    /// (binding 1, `GENERAL`).
    pub fn write_bloom_sets(
        &self,
        device: &Device,
        descriptors: &Descriptors,
        frame: usize,
        pairs: &[(vk::ImageView, vk::ImageView)],
        composite: BloomCompositeBindings,
    ) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let base = frame * BLOOM_PASSES_PER_FRAME;
        let last = pairs.len().saturating_sub(1);
        let mut plan: Vec<Binding> = Vec::with_capacity(pairs.len() * 4);
        for (i, &(source, target)) in pairs.iter().enumerate() {
            let set = self.bloom_sets[base + i];
            plan.push(Binding::sampled(set, 0, linear, source));
            plan.push(Binding::storage(set, 1, target));
            // Only the composite (last) pass samples the dirt mask + streak; the earlier passes bind
            // the harmless fallback so every set is complete (the shader statically references both).
            let (dirt, streak) = if i == last {
                (composite.dirt, composite.streak)
            } else {
                (composite.fallback, composite.fallback)
            };
            plan.push(Binding::sampled(set, 2, linear, dirt));
            plan.push(Binding::sampled(set, 3, linear, streak));
        }
        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: the ash seam. Only slot `frame`'s sets are written; that slot's prior use has
        // signalled its fence, so no in-flight command buffer references these sets.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// The bloom set for pass `pass` in this frame slot (`slot * BLOOM_PASSES_PER_FRAME + pass`).
    pub fn bloom_set(&self, frame: usize, pass: usize) -> vk::DescriptorSet {
        self.bloom_sets[frame * BLOOM_PASSES_PER_FRAME + pass]
    }

    /// The view handle of a built AA `Option<Image>` (motion), or — when not built (the
    /// mode is off) — the offscreen as a valid placeholder so the set is complete.
    fn aa_view(&self, image: &Option<Image>) -> vk::ImageView {
        image.as_ref().map_or(self.offscreen.view(), Image::view)
    }

    /// The view handle of TAA history slot `i`, or the offscreen as a placeholder when TAA
    /// is off (the set is unused until TAA turns on + rebinds).
    fn taa_history_view(&self, i: usize) -> vk::ImageView {
        self.history[i]
            .as_ref()
            .map_or(self.offscreen.view(), Image::view)
    }

    /// The view handle of TAA pixel-lock slot `i`, or the offscreen as a placeholder when TAA
    /// is off (same lifetime + parity as the history ping-pong).
    fn taa_lock_view(&self, i: usize) -> vk::ImageView {
        self.lock[i]
            .as_ref()
            .map_or(self.offscreen.view(), Image::view)
    }

    /// Flips the temporal ping-pong parity + marks the history valid after a frame's TAA /
    /// SSGI accumulation consumed this frame's parity. The next frame reprojects through the
    /// buffer just written.
    pub fn flip_history(&mut self) {
        self.history_valid = true;
        self.history_index = 1 - self.history_index;
    }

    /// Records this frame's camera viewProj as the per-view previous frame for next frame's
    /// motion reprojection.
    pub fn store_prev_view_proj(&mut self, view_proj: saffron_geometry::glam::Mat4) {
        self.prev_view_proj = view_proj;
        self.prev_view_proj_valid = true;
    }

    /// Advances the Halton jitter cycle for the next frame: rolls this frame's offset into
    /// `prev_jitter`, steps the phase index, and recomputes `jitter` for the new index at the
    /// current extent. Called at the frame tail only while TAA is active — mirroring
    /// `store_prev_view_proj`, which records this frame's matrix as next frame's previous.
    pub fn advance_jitter(&mut self) {
        self.prev_jitter = self.jitter;
        // The jitter is a fraction of an INPUT pixel — the scene renders at input extent — and the
        // cycle length scales with the upscale ratio so a heavier upscale eventually covers every
        // display pixel with a jittered input sample.
        let input = self.scaled_render_extent();
        let display = self.published_extent();
        let phases = crate::jitter_phase_count(input, display);
        self.jitter_index = (self.jitter_index + 1) % phases;
        self.jitter = crate::jitter_offset(self.jitter_index, input.width, input.height);
    }

    /// Writes every per-view screen-space set to bind this view's freshly built images.
    fn write_screen_space_sets(
        &self,
        device: &Device,
        descriptors: &Descriptors,
        nearest: vk::Sampler,
    ) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let g_normal = self.view_of(&self.g_normal);
        let g_roughness = self.view_of(&self.g_roughness);
        let ao_raw = self.view_of(&self.ao_raw);
        let ao_map = self.view_of(&self.ao_map);
        let contact_map = self.view_of(&self.contact_map);
        let ssgi_map = self.view_of(&self.ssgi_map);
        let ssr_map = self.view_of(&self.ssr_map);
        let ssgi_denoised = self.view_of(&self.ssgi_denoised);
        let ssgi_resolved = self.view_of(&self.ssgi_resolved);
        let dfao_raw = self.view_of(&self.dfao_raw);
        let dfao_denoised = self.view_of(&self.dfao_denoised);
        let dfao_resolved = self.view_of(&self.dfao_resolved);
        let specocc_raw = self.view_of(&self.specocc_raw);
        let specocc_denoised = self.view_of(&self.specocc_denoised);
        let gi_indirect = self.view_of(&self.gi_indirect);
        let prev_color = self.view_of(&self.prev_color);
        let offscreen = self.offscreen.view();
        let depth = self.depth.view();
        let cloud_full_color = self.view_of(&self.cloud_full_color);
        let cloud_full_depth = self.view_of(&self.cloud_full_depth);

        // Each binding is a `(set, binding, kind)`. A sampler binding pairs a sampler +
        // a view (ShaderReadOnly); a storage binding is a view only (GENERAL). The whole
        // plan is one literal so the `DescriptorImageInfo` arena (filled below) parallels
        // it — keeping every borrow valid for the single `update_descriptor_sets` call.
        let mut plan: Vec<Binding> = vec![
            // gtao: g_normal -> ao_raw
            Binding::sampled(self.gtao_set, 0, nearest, g_normal),
            Binding::storage(self.gtao_set, 1, ao_raw),
            // ao_blur: ao_raw + g_normal -> ao_map. ao_raw is half-res, so a LINEAR sampler
            // bilinearly upsamples it; the depth-weighted taps then keep edges crisp.
            Binding::sampled(self.ao_blur_set, 0, linear, ao_raw),
            Binding::sampled(self.ao_blur_set, 1, nearest, g_normal),
            Binding::storage(self.ao_blur_set, 2, ao_map),
            // contact: g_normal -> contact_map
            Binding::sampled(self.contact_set, 0, nearest, g_normal),
            Binding::storage(self.contact_set, 1, contact_map),
            // ssgi: g_normal + prev_color -> ssgi_map
            Binding::sampled(self.ssgi_set, 0, nearest, g_normal),
            Binding::sampled(self.ssgi_set, 1, linear, prev_color),
            Binding::storage(self.ssgi_set, 2, ssgi_map),
            // ssr: g_normal + prev_color -> ssr_map
            Binding::sampled(self.ssr_set, 0, nearest, g_normal),
            Binding::sampled(self.ssr_set, 1, linear, prev_color),
            Binding::storage(self.ssr_set, 2, ssr_map),
            // ssgi_blur: ssgi_map + g_normal -> ssgi_denoised. ssgi_map is half-res, so a LINEAR
            // sampler bilinearly upsamples it; the depth-weighted taps keep edges crisp.
            Binding::sampled(self.ssgi_blur_set, 0, linear, ssgi_map),
            Binding::sampled(self.ssgi_blur_set, 1, nearest, g_normal),
            Binding::storage(self.ssgi_blur_set, 2, ssgi_denoised),
            // dfao trace (set 2): g_normal -> dfao_raw (the GDF sky-visibility cone trace).
            Binding::sampled(self.dfao_set, 0, nearest, g_normal),
            Binding::storage(self.dfao_set, 1, dfao_raw),
            // dfao-blur: dfao_raw + g_normal -> dfao_denoised. dfao_raw is half-res, so a LINEAR
            // sampler bilinearly upsamples it (reuses the ssgi-blur PSO shape).
            Binding::sampled(self.dfao_blur_set, 0, linear, dfao_raw),
            Binding::sampled(self.dfao_blur_set, 1, nearest, g_normal),
            Binding::storage(self.dfao_blur_set, 2, dfao_denoised),
            // specocc trace (set 2): g_normal + g_roughness -> specocc_raw (the GDF reflection
            // occlusion cone trace). Compute3 shape: two nearest-sampled inputs + one storage out.
            Binding::sampled(self.specocc_set, 0, nearest, g_normal),
            Binding::sampled(self.specocc_set, 1, nearest, g_roughness),
            Binding::storage(self.specocc_set, 2, specocc_raw),
            // specocc-blur: specocc_raw + g_normal -> specocc_denoised. specocc_raw is half-res, so
            // a LINEAR sampler bilinearly upsamples it (reuses the ssgi-blur PSO shape).
            Binding::sampled(self.specocc_blur_set, 0, linear, specocc_raw),
            Binding::sampled(self.specocc_blur_set, 1, nearest, g_normal),
            Binding::storage(self.specocc_blur_set, 2, specocc_denoised),
            // copy_color: offscreen -> prev_color
            Binding::sampled(self.copy_color_set, 0, linear, offscreen),
            Binding::storage(self.copy_color_set, 1, prev_color),
            // mesh set 4: AO + contact + denoised SSGI (all linear-sampled). Without motion
            // it samples the spatially denoised map — the accum pass is off.
            Binding::sampled_image(self.mesh_set, 0, ao_map),
            Binding::sampled_image(self.mesh_set, 1, contact_map),
            Binding::sampled_image(self.mesh_set, 2, ssgi_denoised),
            Binding::sampled_image(self.mesh_set, 3, ssr_map),
            Binding::sampled_image(self.mesh_set, 4, prev_color),
            // mesh set 4 binding 5: the temporally-resolved DFAO sky-visibility. The accum runs
            // whenever motion runs (TAA / SSGI / DFAO all force it on), so the mesh always samples
            // the resolved map; on a frame with no valid history it equals the spatial result.
            Binding::sampled_image(self.mesh_set, 5, dfao_resolved),
            // mesh set 4 binding 6: the spatially-denoised specular reflection-occlusion. It is
            // view-dependent, so it is not temporally accumulated (surface-motion reprojection would
            // smear it); the half-res trace + bilateral upsample are its whole denoise.
            Binding::sampled_image(self.mesh_set, 6, specocc_denoised),
            // mesh set 4 binding 7: the half-res screen-space indirect-diffuse resolve (DDGI + IBL
            // diffuse × sky-vis), linear-sampled to bilinearly upsample. Replaces the fragment's own
            // per-pixel DDGI cage + IBL-diffuse resolve.
            Binding::sampled_image(self.mesh_set, 7, gi_indirect),
            // The mandatory tonemap set: binding 0 = the offscreen color as a storage
            // image (GENERAL).
            Binding::storage(self.tonemap_set, 0, offscreen),
            // The height-fog set: binding 0 = the offscreen color (storage, GENERAL), binding 2 =
            // the scene depth (point-sampled; the graph transitions it to ShaderReadOnly). Binding 1
            // (the fog params UBO) is a dynamic-offset write below; binding 3 (the sky-view LUT) is
            // written once by the renderer.
            Binding::storage(self.fog_set, 0, offscreen),
            Binding::sampled(self.fog_set, 2, nearest, depth),
            Binding::sampled(self.fog_set, 8, linear, cloud_full_color),
            Binding::sampled(self.fog_set, 9, nearest, cloud_full_depth),
        ];
        // gi-resolve view-local image bindings, into every per-frame set (the shared IBL cube +
        // DDGI atlases at b3/b5/b6 are written from the renderer, which owns those sub-states; the
        // b2 params UBO is a buffer write, done after this image update). dfao is linear-sampled so
        // the half-res sky-visibility upsamples cleanly.
        for &set in &self.gi_resolve_sets {
            plan.push(Binding::sampled(set, 0, nearest, g_normal));
            plan.push(Binding::storage(set, 1, gi_indirect));
            // b4 = the spatially-denoised DFAO (available in the screen-space chain where gi-resolve
            // runs), linear-sampled for the half-res upsample. (Temporal `dfao_resolved` is only final
            // after the later accum pass; the spatial result matches it on a converged static frame.)
            plan.push(Binding::sampled(set, 4, linear, dfao_denoised));
        }
        // b2: each slot binds its own mapped GiParams UBO (a buffer write, its own update call).
        for (i, set) in self.gi_resolve_sets.iter().enumerate() {
            descriptors.write_uniform_buffer(
                *set,
                2,
                self.gi_params_ubos[i].handle(),
                self.gi_params_ubos[i].size(),
            );
        }
        // ssgi-accum parities: parity p reads ssgi_history[1-p], writes ssgi_history[p].
        // Without motion, binding 2 (motion) gets the denoised map as a neutral placeholder
        // so the set is complete (the accum pass is off without motion).
        for p in 0..2usize {
            let set = self.ssgi_accum_sets[p];
            plan.push(Binding::sampled(set, 0, linear, ssgi_denoised));
            plan.push(Binding::sampled(set, 1, linear, self.history_view(1 - p)));
            plan.push(Binding::sampled(set, 2, linear, ssgi_denoised));
            plan.push(Binding::storage(set, 3, ssgi_resolved));
            plan.push(Binding::storage(set, 4, self.history_view(p)));

            // dfao-accum parities: parity p reads dfao_history[1-p], writes dfao_history[p].
            // Binding 2 (motion) is the denoised placeholder here; write_aa_sets rebinds it to
            // the real motion target once it is built.
            let dfao = self.dfao_accum_sets[p];
            plan.push(Binding::sampled(dfao, 0, linear, dfao_denoised));
            plan.push(Binding::sampled(
                dfao,
                1,
                linear,
                self.dfao_history_view(1 - p),
            ));
            plan.push(Binding::sampled(dfao, 2, linear, dfao_denoised));
            plan.push(Binding::storage(dfao, 3, dfao_resolved));
            plan.push(Binding::storage(dfao, 4, self.dfao_history_view(p)));
        }

        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: the ash seam. The sets/views/samplers outlive the renderer; host
        // access to these per-view sets is single-threaded at the (idle) build point.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        // The tonemap set's binding 1: the grade UBO as one dynamic-offset slice (the per-frame slice
        // is chosen by the dispatch's dynamic offset, so this is written once and reused every frame).
        descriptors.write_dynamic_uniform_buffer(
            self.tonemap_set,
            1,
            self.grade_ubo.handle(),
            self.grade_ubo_stride,
        );

        // The fog set's binding 1: the fog params UBO as one dynamic-offset slice (the per-frame
        // slice is chosen by the dispatch's dynamic offset, so this is written once and reused).
        descriptors.write_dynamic_uniform_buffer(
            self.fog_set,
            1,
            self.fog_ubo.handle(),
            self.fog_ubo_stride,
        );
    }

    /// Writes `uniform` into frame slot `frame`'s slice of the grade UBO (persistently mapped). The
    /// tonemap dispatch then binds the set with a `frame * grade_ubo_stride` dynamic offset, so this
    /// frame's grade is read without a descriptor rewrite. Called each frame before the tonemap pass.
    pub fn write_grade(&mut self, frame: usize, uniform: &crate::GradeUniform) {
        let offset = self.grade_ubo_stride as usize * frame;
        let src = bytemuck::bytes_of(uniform);
        let dst = self.grade_ubo.mapped_bytes().expect("grade UBO is MAPPED");
        dst[offset..offset + src.len()].copy_from_slice(src);
    }

    /// The dynamic offset for frame slot `frame`'s grade-UBO slice — supplied to the tonemap set bind.
    #[must_use]
    pub fn grade_ubo_offset(&self, frame: usize) -> u32 {
        (self.grade_ubo_stride * frame as u64) as u32
    }

    /// Writes `params` into frame slot `frame`'s slice of the fog UBO (persistently mapped). The fog
    /// dispatch then binds the set with a `frame * fog_ubo_stride` dynamic offset. Called each frame
    /// before the fog pass.
    pub(crate) fn write_fog(&mut self, frame: usize, params: &crate::renderer::FogParams) {
        let offset = self.fog_ubo_stride as usize * frame;
        let src = bytemuck::bytes_of(params);
        let dst = self.fog_ubo.mapped_bytes().expect("fog UBO is MAPPED");
        dst[offset..offset + src.len()].copy_from_slice(src);
    }

    /// The dynamic offset for frame slot `frame`'s fog-UBO slice — supplied to the fog set bind.
    #[must_use]
    pub fn fog_ubo_offset(&self, frame: usize) -> u32 {
        (self.fog_ubo_stride * frame as u64) as u32
    }

    /// Writes the sky-view LUT `view` into binding 3 of the fog set with `sampler`. Called once at
    /// renderer init (the LUT image is allocated once and reused across bakes), so this binding
    /// persists across resizes (the fog set is allocated once, never reallocated).
    pub fn write_fog_sky_lut(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(3)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the froxel integration volume `view` into binding 4 of the fog set with `sampler` (the
    /// volumetric composite's trilinear sample). The volume is fixed-size and never reallocated, so
    /// this binding persists across resizes; the composite reads it only in `fog.mode == volumetric`.
    pub fn write_fog_integration(
        &self,
        device: &Device,
        sampler: vk::Sampler,
        view: vk::ImageView,
    ) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(4)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the aerial-perspective volume `view` into binding 5 of the fog set with `sampler` (the
    /// composite's trilinear AP sample). The volume is fixed-size and never reallocated, so this binding
    /// persists across resizes; the composite reads it only when the atmosphere is live + AP authored.
    pub fn write_fog_aerial(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.fog_set)
            .dst_binding(5)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the atmosphere transmittance and multiscatter LUTs into fog bindings 6 and 7.
    pub fn write_fog_atmosphere_luts(
        &self,
        device: &Device,
        sampler: vk::Sampler,
        transmittance: vk::ImageView,
        multi_scatter: vk::ImageView,
    ) {
        let infos = [transmittance, multi_scatter].map(|view| {
            [vk::DescriptorImageInfo {
                sampler,
                image_view: view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            }]
        });
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.fog_set)
                .dst_binding(6)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[0]),
            vk::WriteDescriptorSet::default()
                .dst_set(self.fog_set)
                .dst_binding(7)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[1]),
        ];
        unsafe { device.raw().update_descriptor_sets(&writes, &[]) };
    }

    /// Writes the creative-look 3D LUT `view` into binding 2 of the tonemap set with `sampler`. Called
    /// at view build with the identity default and rewritten (idled) when a creative look is assigned.
    /// The set is allocated once and never reallocated, so this binding persists across resizes.
    pub fn write_tonemap_lut(&self, device: &Device, sampler: vk::Sampler, view: vk::ImageView) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.tonemap_set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
        // (idle) build/assign point, so no in-flight command buffer references this set.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Writes the *shared* gi-resolve bindings into every per-frame set — b3 the sky SH buffer,
    /// b5 the DDGI irradiance atlas, b6 the DDGI distance-moment atlas. Separate
    /// from [`ViewTarget::write_screen_space_sets`] because the IBL + DDGI sub-states are owned by
    /// the renderer (not visible at view build). The renderer calls this once they are ready, and
    /// re-calls it on IBL rebake / DDGI rebuild (the views change) — the same triggers the mesh
    /// sets use. All three are sampled `SHADER_READ_ONLY`, matching how the mesh samples them.
    #[allow(clippy::too_many_arguments)]
    pub fn write_gi_resolve_shared(
        &self,
        device: &Device,
        frame: usize,
        sky_sh: vk::Buffer,
        sky_sh_size: vk::DeviceSize,
        ddgi_irradiance: vk::ImageView,
        ddgi_distance: vk::ImageView,
        ddgi_sampler: vk::Sampler,
    ) {
        // Write ONLY this frame's slot — the other slots may be bound by an in-flight command buffer
        // (writing them would trip VUID-vkUpdateDescriptorSets-None-03047). This frame's slot was last
        // used `MAX_FRAMES_IN_FLIGHT` frames ago, whose fence `begin_frame` already waited, so it is
        // free. Every slot converges to the current views over that many frames.
        let set = self.gi_resolve_sets[frame];
        if set == vk::DescriptorSet::null() {
            return; // sets not allocated yet (screen-space not built)
        }
        let plan: [Binding; 2] = [
            Binding::sampled(set, 5, ddgi_sampler, ddgi_irradiance),
            Binding::sampled(set, 6, ddgi_sampler, ddgi_distance),
        ];
        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let sh_info = [vk::DescriptorBufferInfo::default()
            .buffer(sky_sh)
            .offset(0)
            .range(sky_sh_size)];
        let mut writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&sh_info),
        );
        // SAFETY: the ash seam. Sets/views/samplers are valid; the renderer calls this at a
        // post-fence point where the sets are not in use by an in-flight frame.
        unsafe { device.raw().update_descriptor_sets(&writes, &[]) };
    }

    /// The view handle of an `Option<Image>`, or null if it is not built (the
    /// screen-space set writes never run before `build_screen_space`, so this is
    /// always populated when used).
    fn view_of(&self, image: &Option<Image>) -> vk::ImageView {
        image.as_ref().map_or(vk::ImageView::null(), Image::view)
    }

    /// The view handle of SSGI history slot `i`.
    fn history_view(&self, i: usize) -> vk::ImageView {
        self.ssgi_history[i]
            .as_ref()
            .map_or(vk::ImageView::null(), Image::view)
    }

    /// The view handle of DFAO history slot `i`.
    fn dfao_history_view(&self, i: usize) -> vk::ImageView {
        self.dfao_history[i]
            .as_ref()
            .map_or(vk::ImageView::null(), Image::view)
    }

    /// Whether the screen-space chain is built (the G-buffer image exists).
    pub fn screen_space_ready(&self) -> bool {
        self.g_normal.is_some()
    }

    /// Recreates the targets at a new size (a viewport resize), bumping
    /// [`ViewTarget::generation`]. The caller idles the GPU before this so the old
    /// images are no longer read; the old [`Image`]s Drop when replaced. The
    /// screen-space images are rebuilt + the sets rewritten by the caller after this
    /// via [`ViewTarget::build_screen_space`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if recreation fails (the old targets are left
    /// in place on failure).
    pub fn resize(
        &mut self,
        device: &Device,
        input: vk::Extent2D,
        display: vk::Extent2D,
    ) -> Result<()> {
        let resources = device.resources();
        // Recreate only the class whose extent changed: the offscreen is the DISPLAY-extent resolve
        // output, the scene depth is INPUT extent. A render-scale-only change moves the input class
        // alone, so the offscreen (and every descriptor set that binds it — tonemap, resolve) is
        // left intact; only the input-extent depth is rebuilt.
        if self.offscreen.extent != display {
            self.offscreen = Image::new(
                resources,
                &ImageDesc::color_2d(
                    display,
                    OFFSCREEN_COLOR_FORMAT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::STORAGE,
                ),
            )?;
        }
        if self.depth.extent == input {
            self.generation += 1;
            return Ok(());
        }
        let depth = Image::new(
            resources,
            &ImageDesc {
                extent: input,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;
        self.depth = depth;
        self.generation += 1;
        Ok(())
    }

    /// The DISPLAY extent — the last requested viewport size, independent of
    /// [`ViewTarget::render_scale`]. The offscreen resolve output, TAA history, tonemap,
    /// overlays, shm capture, and the present blit all live here; the scene renders smaller
    /// (see [`ViewTarget::scaled_render_extent`]) and the resolve reconstructs up to this.
    pub fn published_extent(&self) -> vk::Extent2D {
        vk::Extent2D {
            width: self.desired_width.max(1),
            height: self.desired_height.max(1),
        }
    }

    /// The INPUT (render) extent for the current desired size + [`ViewTarget::render_scale`]:
    /// `round(desired * scale)`, clamped to at least 1px. The scene color scratch, depth,
    /// motion, and the whole G-buffer / SSGI / DFAO / ReSTIR chain rasterise here; the TAA
    /// resolve reconstructs them up to [`ViewTarget::published_extent`].
    pub fn scaled_render_extent(&self) -> vk::Extent2D {
        let scale = self.render_scale.clamp(0.1, 1.0);
        vk::Extent2D {
            width: ((self.desired_width as f32 * scale).round() as u32).max(1),
            height: ((self.desired_height as f32 * scale).round() as u32).max(1),
        }
    }
}

/// The composite pass's extra sampled inputs written into every bloom set (bindings 2/3): the
/// lens-dirt `mask` and the anamorphic `streak` buffer bind on the composite (last) pass, and the
/// `fallback` (the renderer's 1×1 white) binds on the earlier passes that never sample them.
#[derive(Clone, Copy)]
pub struct BloomCompositeBindings {
    /// The lens-dirt mask view (a texture asset, or the white fallback when no dirt is set).
    pub dirt: vk::ImageView,
    /// The anamorphic streak buffer view (transient, or the white fallback when streaks are off).
    pub streak: vk::ImageView,
    /// The harmless fallback for the non-composite passes' bindings 2/3 (the 1×1 white view).
    pub fallback: vk::ImageView,
}

/// Rounds `value` up to the next multiple of `align` (a power of two ≥ 1) — the dynamic-UBO slice
/// stride against `minUniformBufferOffsetAlignment`.
fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

/// One descriptor-set image binding for the screen-space set writes: a sampled image
/// (sampler + view, ShaderReadOnly) or a storage image (view only, GENERAL). Collected
/// into a flat plan so the [`vk::DescriptorImageInfo`] arena parallels the writes.
struct Binding {
    set: vk::DescriptorSet,
    binding: u32,
    kind: vk::DescriptorType,
    sampler: Option<vk::Sampler>,
    view: vk::ImageView,
}

impl Binding {
    fn sampled(
        set: vk::DescriptorSet,
        binding: u32,
        sampler: vk::Sampler,
        view: vk::ImageView,
    ) -> Self {
        Self {
            set,
            binding,
            kind: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            sampler: Some(sampler),
            view,
        }
    }

    fn sampled_image(set: vk::DescriptorSet, binding: u32, view: vk::ImageView) -> Self {
        Self {
            set,
            binding,
            kind: vk::DescriptorType::SAMPLED_IMAGE,
            sampler: None,
            view,
        }
    }

    fn storage(set: vk::DescriptorSet, binding: u32, view: vk::ImageView) -> Self {
        Self {
            set,
            binding,
            kind: vk::DescriptorType::STORAGE_IMAGE,
            sampler: None,
            view,
        }
    }

    fn kind(&self) -> vk::DescriptorType {
        self.kind
    }

    fn info(&self) -> vk::DescriptorImageInfo {
        match self.kind {
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER => vk::DescriptorImageInfo::default()
                .sampler(self.sampler.expect("combined image sampler"))
                .image_view(self.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorType::SAMPLED_IMAGE => vk::DescriptorImageInfo::default()
                .image_view(self.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorType::STORAGE_IMAGE => vk::DescriptorImageInfo::default()
                .image_view(self.view)
                .image_layout(vk::ImageLayout::GENERAL),
            _ => unreachable!("image binding descriptor type"),
        }
    }
}

/// Crate-internal alias of [`initialize_screen_space_layouts`] for sub-state outside this
/// module (the per-view ReSTIR build seeds its resolved-radiance image's resting layout the
/// same way).
pub(crate) fn initialize_read_only_layouts(device: &Device, images: &[&Image]) -> Result<()> {
    initialize_screen_space_layouts(device, images)
}

/// Verifies the device supports the shm-capture blit: BLIT_SRC on the offscreen format and
/// BLIT_DST on `B8G8R8A8_UNORM`, both in optimal tiling. The shm-publish blit assumes
/// this (NVIDIA satisfies it); there is no CPU fallback by design.
fn require_shm_blit_support(device: &Device, src_format: vk::Format) -> Result<()> {
    // SAFETY: the ash seam. The format-property queries are read-only.
    let src = unsafe {
        device
            .instance()
            .get_physical_device_format_properties(device.physical_device(), src_format)
    };
    let dst = unsafe {
        device.instance().get_physical_device_format_properties(
            device.physical_device(),
            vk::Format::B8G8R8A8_UNORM,
        )
    };
    if !src
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::BLIT_SRC)
        || !dst
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::BLIT_DST)
    {
        return Err(crate::Error::Vk {
            context: "shm capture: offscreen lacks BLIT_SRC or BGRA8 lacks BLIT_DST",
            result: vk::Result::ERROR_FORMAT_NOT_SUPPORTED,
        });
    }
    Ok(())
}

/// One `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` init transition over `images` so their
/// descriptors are valid before any pass runs. A one-shot submit + wait at the
/// (idle) build point.
fn initialize_screen_space_layouts(device: &Device, images: &[&Image]) -> Result<()> {
    use crate::checked;
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "ssao init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(
        unsafe { raw.allocate_command_buffers(&alloc) },
        "ssao init cmd",
    )?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "ssao init fence",
    )?;

    let result = (|| -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let barriers: Vec<vk::ImageMemoryBarrier2> = images
            .iter()
            .map(|image| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                    .src_access_mask(vk::AccessFlags2::empty())
                    .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.handle())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
            })
            .collect();
        // SAFETY: the ash seam. The barriers reference images this device created.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "ssao init begin")?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "ssao init end")?;
        }
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. The queue is touched single-threaded at the build point.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "ssao init submit")?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "ssao init wait",
            )?;
        }
        Ok(())
    })();

    // SAFETY: the ash seam. The fence was waited (or the submit never happened), so the
    // pool/fence are idle and destroyed exactly once.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use std::sync::{Arc, Mutex};

    /// Builds two views, each with its own screen-space targets + per-view sets, and
    /// asserts no cross-view aliasing: each view's sets are distinct handles, each view
    /// owns distinct images, and building the second view does not disturb the first
    /// view's sets — switching the active view binds *this* view's images, never the
    /// other's. Also asserts the build + teardown is validation-clean. Skips when no
    /// Vulkan device is present.
    #[test]
    fn per_view_screen_space_sets_never_alias_across_views() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let ssao = Ssao::new(&device).expect("Ssao");

        let mut view_a = ViewTarget::new(&device, 32, 32).expect("view a");
        view_a
            .allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc a");
        view_a
            .build_screen_space(&device, &descriptors, &ssao)
            .expect("build a");
        let mut view_b = ViewTarget::new(&device, 48, 24).expect("view b");
        view_b
            .allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc b");
        view_b
            .build_screen_space(&device, &descriptors, &ssao)
            .expect("build b");

        // The two views' per-view sets are distinct handles — a switch never binds the
        // other view's set.
        assert_ne!(view_a.gtao_set, view_b.gtao_set);
        assert_ne!(view_a.mesh_set, view_b.mesh_set);
        assert_ne!(view_a.ssgi_set, view_b.ssgi_set);
        // Each view owns distinct screen-space images — its sets bind its own targets.
        let a_g = view_a.g_normal.as_ref().unwrap().handle();
        let b_g = view_b.g_normal.as_ref().unwrap().handle();
        assert_ne!(a_g, b_g, "each view has its own G-buffer image");
        let a_ssgi = view_a.ssgi_denoised.as_ref().unwrap().handle();
        let b_ssgi = view_b.ssgi_denoised.as_ref().unwrap().handle();
        assert_ne!(a_ssgi, b_ssgi);

        drop(view_a);
        drop(view_b);
        drop(ssao);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the per-view screen-space build + teardown must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The SSGI history validity resets on a view resize: a fresh build leaves it false
    /// (no temporal history yet), and rebuilding after a resize re-resets it even if a
    /// frame had set it true (the reprojection is stale). Skips when no Vulkan device is
    /// present.
    #[test]
    fn ssgi_history_validity_resets_on_resize() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let ssao = Ssao::new(&device).expect("Ssao");

        let mut view = ViewTarget::new(&device, 32, 32).expect("view");
        view.allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("build");
        assert!(!view.history_valid, "fresh build has no temporal history");

        // A frame validates the history; a resize must invalidate it again.
        view.history_valid = true;
        view.history_index = 1;
        view.desired_width = 64;
        view.desired_height = 48;
        let ext = vk::Extent2D {
            width: 64,
            height: 48,
        };
        view.resize(&device, ext, ext).expect("resize");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("rebuild");
        assert!(
            !view.history_valid,
            "a resize invalidates the SSGI reprojection history"
        );
        assert_eq!(view.history_index, 0, "the ping-pong parity resets too");

        drop(view);
        drop(ssao);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);
    }

    /// `build_aa_targets` creates the per-mode AA targets: the motion target + its depth ride
    /// with SSGI; the INPUT-extent scene scratch and the DISPLAY-extent overlay depth are built
    /// in every mode (the scene always rasterises into scratch, the resolve reconstructs to the
    /// display offscreen); TAA adds the two DISPLAY-extent history images; MSAA adds the
    /// multisampled scene color + depth. The build + descriptor set writes + teardown are
    /// validation-clean across every mode (a GPU gate the toolbox can run — no ray tracing, no
    /// present). Skips when no Vulkan device is present.
    #[test]
    fn build_aa_targets_per_mode_is_validation_clean() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let ssao = Ssao::new(&device).expect("Ssao");
        let supported = device.supported_sample_counts(OFFSCREEN_COLOR_FORMAT, DEPTH_FORMAT);

        let mut view = ViewTarget::new(&device, 32, 32).expect("view");
        view.allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("build screen-space");

        // Off: motion (SSGI feeds it), the input scratch + display overlay depth (always built),
        // but no TAA history / MSAA.
        let mut aa = crate::Aa::new(supported);
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("build off");
        assert!(view.motion.is_some(), "the motion target rides with SSGI");
        assert!(view.motion_depth.is_some());
        assert!(
            view.scratch.is_some(),
            "the scene always renders into the input scratch"
        );
        assert!(
            view.depth_display.is_some(),
            "the display-extent overlay depth is always built"
        );
        assert!(view.history[0].is_none() && view.history[1].is_none());
        assert!(view.msaa_color.is_none());

        // TAA: motion + its depth + two history + scratch.
        aa.set(0, false, true);
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("build taa");
        assert!(view.motion.is_some(), "TAA builds the motion target");
        assert!(view.motion_depth.is_some());
        assert!(
            view.history[0].is_some() && view.history[1].is_some(),
            "TAA builds the two ping-pong history images"
        );
        assert!(view.scratch.is_some(), "TAA renders the scene into scratch");
        assert!(view.msaa_color.is_none(), "TAA is not MSAA");
        assert!(!view.history_valid, "a fresh build has no temporal history");

        // FXAA: scratch + motion (SSGI feeds it), but no TAA history.
        aa.set(0, true, false);
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("build fxaa");
        assert!(
            view.scratch.is_some(),
            "FXAA renders the scene into scratch"
        );
        assert!(
            view.history[0].is_none() && view.history[1].is_none(),
            "FXAA has no TAA history"
        );
        assert!(view.msaa_color.is_none());

        // MSAA (only when the device supports a count > 1; llvmpipe does).
        if supported.contains(vk::SampleCountFlags::TYPE_4)
            || supported.contains(vk::SampleCountFlags::TYPE_2)
        {
            aa.set(4, false, false);
            view.build_aa_targets(&device, &descriptors, aa)
                .expect("build msaa");
            assert!(
                view.msaa_color.is_some(),
                "MSAA builds the multisampled color"
            );
            assert!(view.msaa_depth.is_some());
            assert!(
                view.scratch.is_some(),
                "the scene always renders into the input scratch (MSAA resolves into it)"
            );
            assert!(view.history[0].is_none() && view.history[1].is_none());
        }

        drop(view);
        drop(ssao);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the per-mode AA target build must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The temporal bookkeeping: a fresh AA build has no history and no prev-viewProj; a
    /// frame's `store_prev_view_proj` + `flip_history` mark them valid and toggle the parity;
    /// a rebuild (resize / AA change) re-invalidates both (the reprojection is stale).
    /// Skips when no device.
    #[test]
    fn taa_history_and_prev_view_proj_invalidate_on_rebuild() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let ssao = Ssao::new(&device).expect("Ssao");
        let supported = device.supported_sample_counts(OFFSCREEN_COLOR_FORMAT, DEPTH_FORMAT);
        let mut aa = crate::Aa::new(supported);
        aa.set(0, false, true);

        let mut view = ViewTarget::new(&device, 32, 32).expect("view");
        view.allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("build screen-space");
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("build taa");
        assert!(!view.history_valid, "fresh build: no temporal history");
        assert!(!view.prev_view_proj_valid, "fresh build: no prev viewProj");
        assert_eq!(view.history_index, 0);

        // A frame consumes the parity + records its viewProj.
        view.store_prev_view_proj(saffron_geometry::glam::Mat4::IDENTITY);
        view.flip_history();
        assert!(view.history_valid, "a frame validates the history");
        assert!(view.prev_view_proj_valid);
        assert_eq!(view.history_index, 1, "the ping-pong parity flipped");

        // A resize rebuilds the screen-space + AA targets and re-invalidates everything.
        view.desired_width = 64;
        view.desired_height = 48;
        let ext = vk::Extent2D {
            width: 64,
            height: 48,
        };
        view.resize(&device, ext, ext).expect("resize");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("rebuild screen-space");
        view.build_aa_targets(&device, &descriptors, aa)
            .expect("rebuild taa");
        assert!(
            !view.history_valid,
            "a resize invalidates the TAA reprojection history"
        );
        assert!(
            !view.prev_view_proj_valid,
            "a resize invalidates the prev viewProj"
        );
        assert_eq!(view.history_index, 0, "the parity resets too");

        drop(view);
        drop(ssao);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);
    }
}
