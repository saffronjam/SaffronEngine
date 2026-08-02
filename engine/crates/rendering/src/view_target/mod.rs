//! The per-view offscreen render targets: the scene color (RGBA16F) and depth (D32) images, plus
//! the thin G-buffer and screen-space effect chain (AO / contact / SSGI maps + history + the
//! per-view descriptor sets that bind them), all sized to the viewport.
//!
//! The screen-space images and their sets are per-view rather than on the device-shared
//! [`crate::ssao::Ssao`], so a view switch never leaves a set bound to another view's images.

mod aa;
mod post_sets;
mod screen_space;

use ash::vk;

use crate::descriptors::{BLOOM_PASSES_PER_FRAME, Descriptors};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::pipelines::{DEPTH_FORMAT, OFFSCREEN_COLOR_FORMAT};
use crate::resources::{Buffer, Image, ImageDesc};
use crate::restir::RestirView;
use crate::ssao::{
    AO_FORMAT, AO_RAW_FORMAT, G_NORMAL_FORMAT, ROUGHNESS_FORMAT, Ssao, mesh_set_layout,
};
use crate::{Device, Result};

use screen_space::*;

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
    ///. Inert on a software device.
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

    /// (Re)creates every capture slot at `extent`, replacing whatever the ring held.
    ///
    /// **Only call this where the device is idle.** A slot's image and staging buffer are
    /// freed the moment they are replaced, and a readback recorded into them may still be
    /// in flight — freeing under it loses the device. The two seams that qualify both hold
    /// an idle already: the render-extent resize and arming shm publish. `record_shm_copy`
    /// therefore only ever *uses* a slot.
    ///
    /// Recreating drops `valid`: no completed bytes exist at the new size yet.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the device lacks BLIT_SRC on the offscreen format or
    /// BLIT_DST on BGRA8 (optimal tiling), or any allocation fails.
    pub fn size_shm_capture(&mut self, device: &Device, extent: vk::Extent2D) -> Result<()> {
        if extent.width == 0 || extent.height == 0 {
            return Ok(());
        }
        require_shm_blit_support(device, self.offscreen.format)?;
        let resources = device.resources();
        for slot in 0..MAX_FRAMES_IN_FLIGHT {
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
            let bytes =
                vk::DeviceSize::from(extent.width) * vk::DeviceSize::from(extent.height) * 4;
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
        }
        Ok(())
    }

    /// Drops the shm-capture ring. The renderer calls this under `wait_idle`.
    pub fn destroy(&mut self, _device: &Device) {
        self.shm_capture = ShmCapture::default();
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

#[cfg(test)]
mod tests;
