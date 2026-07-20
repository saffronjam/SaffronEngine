//! Dynamic Diffuse Global Illumination: a camera-centered scrolling probe clipmap updated each
//! frame by a software ray trace of the real distance field, sampled in the mesh fragment for
//! multi-bounce indirect.
//!
//! The trace sphere-marches the shared distance oracle — the per-mesh MDF for the near field and
//! the Global Distance Field beyond (the `sdf` module's `sampleField`) — and the sky enters
//! radiance ONLY on a ray miss (the Lumen-style occlusion that makes enclosed interiors go dark
//! by construction). A hit reads a lite per-cell albedo cache (a flat per-cell base color owned by
//! [`crate::GlobalSdf`]) lit by a crude sun+sky term plus last-frame probe irradiance.
//!
//! It owns two octahedral atlases (irradiance rgba16f + distance/moment rg16f), a per-frame ray
//! radiance+distance image, the linear-clamp atlas sampler, and the four descriptor-set layouts +
//! sets the four compute passes bind. Unlike the screen-space sub-state, there is exactly one DDGI
//! volume, so the descriptor sets are device-shared (pool-owned), not per-view.
//!
//! Built once in [`Ddgi::new`], then borrowed `&Ddgi` by the frame-graph build (its handles are
//! immutable after init); the camera-centered placement + sun + temporal state are written
//! through [`Ddgi::set_scene`] / [`Ddgi::set_enabled`] / [`Ddgi::advance_frame`] (`&mut
//! self.ddgi`). The renderer carries the sun/sky as plain fields fed from the scene, the same
//! decoupling as IBL.
//!
//! # The four passes
//!
//! Each declares its storage usages so the render graph derives the `GENERAL` barriers:
//!
//! 1. `ddgi-trace` — GDF/MDF sphere-march (set 0 bindless bricks + set 1 light set + set 2 albedo
//!    cache / prev-irradiance sampler) → ray storage write.
//! 2. `ddgi-blend-irr` — ray sampler → irradiance storage.
//! 3. `ddgi-blend-dist` — ray sampler → distance (moment) storage.
//! 4. `ddgi-border` — the octahedral gutter copy on the irradiance atlas.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{IVec3, IVec4, UVec4, Vec3, Vec4};

use crate::descriptors::Descriptors;
use crate::resources::{Buffer, DeviceResources, Image, ImageDesc};
use crate::{Device, Result, checked};

/// Probes per axis (X) of the camera-centered clipmap.
pub const DDGI_PROBES_X: u32 = 16;
/// Probes per axis (Y).
pub const DDGI_PROBES_Y: u32 = 8;
/// Probes per axis (Z).
pub const DDGI_PROBES_Z: u32 = 16;
/// World-space spacing between adjacent probes (metres) — below typical wall thickness so the
/// Chebyshev leak test actually bounds light bleeding through floors/walls.
pub const DDGI_PROBE_SPACING: f32 = 1.5;
/// Rays traced per probe per frame.
pub const DDGI_RAYS_PER_PROBE: u32 = 64;
/// Octahedral irradiance tile interior size.
pub const DDGI_IRR_INTERIOR: u32 = 8;
/// Octahedral distance (moment) tile interior size.
pub const DDGI_DIST_INTERIOR: u32 = 16;
/// Temporal blend weight — the fraction of last frame's value kept each update.
pub const DDGI_HYSTERESIS: f32 = 0.95;

/// The per-frame ray image format (rgba16f: radiance + hit distance).
pub const DDGI_RAY_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
/// The irradiance atlas format (rgba16f).
pub const DDGI_IRR_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
/// The distance (moment) atlas format (rg16f: mean distance, mean squared distance).
pub const DDGI_DIST_FORMAT: vk::Format = vk::Format::R16G16_SFLOAT;

/// The total probe count across the volume (one octahedral tile per probe).
pub const DDGI_PROBE_TOTAL: u32 = DDGI_PROBES_X * DDGI_PROBES_Y * DDGI_PROBES_Z;

/// Probes the trace re-rays per frame — a round-robin budget. Each frame traces a rolling window
/// of this many probes (offset advancing by the budget), so the whole volume refreshes every
/// `DDGI_PROBE_TOTAL / DDGI_PROBE_BUDGET` frames. Untraced probes keep their last rays, which the
/// (full) blend re-applies — stable on a static volume, a few frames of latency under fast relight
/// or a camera scroll. The cheap blend + border passes stay full-volume; only the expensive ray
/// trace is budgeted.
pub const DDGI_PROBE_BUDGET: u32 = DDGI_PROBE_TOTAL / 4;

/// Frames the round-robin takes to re-ray every probe once (`total / budget`) — the gap between a
/// probe's consecutive traces. The probe-relocation reset accumulates the scroll over this many
/// frames so a probe that scrolled in between budget slices is still flagged newly-exposed on the
/// frame it is finally re-rayed (no ghosting trail). With the budget a quarter of the total this is
/// four frames.
pub const DDGI_PROBE_CYCLE: usize = (DDGI_PROBE_TOTAL / DDGI_PROBE_BUDGET) as usize;

/// The trace push: probe grid + volume + sun + scroll/frame. 112 bytes, matching
/// `ddgi_trace.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TracePush {
    /// `xyz` = probes per axis, `w` = rays per probe.
    pub probe_count: UVec4,
    /// `xyz` = volume min corner, `w` = irradiance tile interior.
    pub volume_min: Vec4,
    /// `xyz` = volume size (`count · spacing`).
    pub volume_extent: Vec4,
    /// `xyz` = direction the sun travels, `w` = sun intensity.
    pub sun_dir: Vec4,
    /// `rgb` = sun color, `w` = frame index (rotates the ray set).
    pub sun_color: Vec4,
    /// `x` = round-robin probe-budget offset.
    pub budget_offset: Vec4,
    /// `xyz` = toroidal scroll base (`wrapMod(snapBase, count)`), `w` = SDF instance count.
    pub scroll_base: IVec4,
}

const _: () = assert!(size_of::<TracePush>() == 112);

/// The blend-irradiance / blend-distance push: probe grid + tile params + a params vec4 + the
/// toroidal scroll geometry (so a freshly scrolled-in probe drops its stale history when re-rayed).
/// 80 bytes, matching `ddgi_blend_irradiance.slang` / `ddgi_blend_distance.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BlendPush {
    /// `xyz` = probes per axis, `w` = rays per probe.
    pub probe_count: UVec4,
    /// `x` = tile interior size, `y` = 1 on the whole-volume first frame, `z` = round-robin offset,
    /// `w` = probe budget.
    pub tile: UVec4,
    /// `x` = hysteresis (history weight), `y` = max distance (distance pass only).
    pub params: Vec4,
    /// `xyz` = toroidal scroll base, `w` = total probe count.
    pub scroll_base: IVec4,
    /// `xyz` = this frame's scroll (`snapBase - prevSnapBase`), `w` reserved.
    pub delta: IVec4,
}

const _: () = assert!(size_of::<BlendPush>() == 80);

/// The octahedral-gutter border-copy push: probe grid + the tile interior size. 32 bytes,
/// matching `ddgi_border.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BorderPush {
    /// `xyz` = probes per axis.
    pub probe_count: UVec4,
    /// `x` = tile interior size.
    pub tile: UVec4,
}

const _: () = assert!(size_of::<BorderPush>() == 32);

/// The irradiance atlas width in texels (`probesX·probesY` tiles of `interior+2`).
pub const fn irradiance_atlas_width() -> u32 {
    DDGI_PROBES_X * DDGI_PROBES_Y * (DDGI_IRR_INTERIOR + 2)
}

/// The irradiance atlas height in texels (`probesZ` tiles of `interior+2`).
pub const fn irradiance_atlas_height() -> u32 {
    DDGI_PROBES_Z * (DDGI_IRR_INTERIOR + 2)
}

/// The distance (moment) atlas width in texels.
pub const fn distance_atlas_width() -> u32 {
    DDGI_PROBES_X * DDGI_PROBES_Y * (DDGI_DIST_INTERIOR + 2)
}

/// The distance (moment) atlas height in texels.
pub const fn distance_atlas_height() -> u32 {
    DDGI_PROBES_Z * (DDGI_DIST_INTERIOR + 2)
}

/// Euclidean modulo (`%` truncates toward zero; a negative cell must wrap to a positive tile).
fn wrap_mod(v: i32, n: i32) -> i32 {
    let r = v % n;
    if r < 0 { r + n } else { r }
}

/// The DDGI sub-state: the two octahedral atlases, the per-frame ray image, the atlas sampler, and
/// the four set layouts + sets the four compute passes (and the mesh set 5) bind.
///
/// Owns an [`Arc`]`<`[`DeviceResources`]`>` so its handles free in [`Drop`] without a live
/// `&Device`. The mesh set-5 *layout* is owned by [`Descriptors`] (the übershader binds it); this
/// struct allocates the mesh *set* against it and writes the atlas samplers into it.
pub struct Ddgi {
    resources: Arc<DeviceResources>,

    /// On by default — the interior-darkening indirect path (the four compute passes).
    pub use_ddgi: bool,
    /// Resources + sets valid. True after [`Ddgi::new`].
    pub ready: bool,
    /// First frame after enable/resize → no temporal blend (a whole-volume history reset).
    history_reset: bool,

    irradiance: Image,
    distance: Image,
    rays: Image,
    /// Rotates the trace ray set + advances the round-robin probe window each frame.
    frame_index: u32,

    volume_min: Vec3,
    volume_extent: Vec3,
    /// The camera-snapped volume min corner in global probe cells (the toroidal base this frame).
    snap_base: IVec3,
    /// A ring of the last [`DDGI_PROBE_CYCLE`] committed snapped bases (the scroll references for
    /// probe relocation): the slot at `frame_index % DDGI_PROBE_CYCLE` holds the base from one full
    /// round-robin cycle ago, the reset reference for a probe re-rayed this frame.
    snap_base_ring: [IVec3; DDGI_PROBE_CYCLE],
    sun_dir: Vec3,
    sun_color: Vec3,
    sun_intensity: f32,

    sampler: vk::Sampler,
    trace_layout: vk::DescriptorSetLayout,
    blend_irr_layout: vk::DescriptorSetLayout,
    blend_dist_layout: vk::DescriptorSetLayout,
    border_layout: vk::DescriptorSetLayout,

    trace_set: vk::DescriptorSet,
    blend_irr_set: vk::DescriptorSet,
    blend_dist_set: vk::DescriptorSet,
    border_set: vk::DescriptorSet,
    mesh_set: vk::DescriptorSet,
}

impl Ddgi {
    /// Allocates the atlases + ray image, the linear-clamp sampler, the four compute set layouts +
    /// the mesh set 5, the five sets, writes the static descriptors (the images never reallocate
    /// after init), and init-clears all three images to zero — parking the two atlases in
    /// `SHADER_READ_ONLY_OPTIMAL` (the resting mesh-sample state) and the ray image in `GENERAL`
    /// (its storage-write layout) so the round-robin warmup reads deterministic zero, never
    /// undefined device memory.
    ///
    /// `ready` is set true on success; `use_ddgi` defaults ON (the trace set 2's albedo-cache
    /// binding is written later by [`Ddgi::bind_gdf_albedo`], before any dispatch).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; already-created handles are freed
    /// before returning on a partial failure (the early-return cleanup mirrors the resource
    /// wrappers' `Drop`).
    pub fn new(device: &Device, descriptors: &Descriptors) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        // The two atlases + the per-frame ray image. The atlases are STORAGE (written by the
        // blend/border compute) + SAMPLED (read by the mesh + the trace's prev-irradiance bind);
        // the ray image is STORAGE (trace writes) + SAMPLED (the blend passes read). All three also
        // carry TRANSFER_DST for the one-shot init clear to zero.
        let storage_sampled = vk::ImageUsageFlags::STORAGE
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_DST;
        let irradiance = make_storage_image(
            &resources,
            irradiance_atlas_width(),
            irradiance_atlas_height(),
            DDGI_IRR_FORMAT,
            storage_sampled,
        )?;
        let distance = make_storage_image(
            &resources,
            distance_atlas_width(),
            distance_atlas_height(),
            DDGI_DIST_FORMAT,
            storage_sampled,
        )?;
        let rays = make_storage_image(
            &resources,
            DDGI_RAYS_PER_PROBE,
            DDGI_PROBE_TOTAL,
            DDGI_RAY_FORMAT,
            storage_sampled,
        )?;

        let sampler = create_linear_clamp_sampler(raw)?;

        // The four compute set layouts + the mesh set 5 layout is owned by Descriptors. Build with
        // the same partial-failure unwind discipline as the resource wrappers: a guard frees what
        // was created so far on any error.
        let layouts = match build_layouts(raw) {
            Ok(layouts) => layouts,
            Err(err) => {
                // SAFETY: the ash seam. Free the sampler created above before returning.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };

        let (trace_set, blend_irr_set, blend_dist_set, border_set, mesh_set) =
            match allocate_sets(descriptors, &layouts) {
                Ok(sets) => sets,
                Err(err) => {
                    // SAFETY: the ash seam. The sets are pool-owned (freed with the pool); free the
                    // layouts + sampler created above before returning.
                    unsafe {
                        layouts.destroy(raw);
                        raw.destroy_sampler(sampler, None);
                    }
                    return Err(err);
                }
            };

        let count = Vec3::new(
            DDGI_PROBES_X as f32,
            DDGI_PROBES_Y as f32,
            DDGI_PROBES_Z as f32,
        );
        let mut ddgi = Self {
            resources,
            use_ddgi: true,
            ready: false,
            history_reset: true,
            irradiance,
            distance,
            rays,
            frame_index: 0,
            volume_min: -count * DDGI_PROBE_SPACING * 0.5,
            volume_extent: count * DDGI_PROBE_SPACING,
            snap_base: IVec3::ZERO,
            snap_base_ring: [IVec3::ZERO; DDGI_PROBE_CYCLE],
            sun_dir: Vec3::new(-0.5, -1.0, -0.3),
            sun_color: Vec3::ONE,
            sun_intensity: 1.0,
            sampler,
            trace_layout: layouts.trace,
            blend_irr_layout: layouts.blend_irr,
            blend_dist_layout: layouts.blend_dist,
            border_layout: layouts.border,
            trace_set,
            blend_irr_set,
            blend_dist_set,
            border_set,
            mesh_set,
        };

        ddgi.write_static_descriptors();
        // On a failure the `?` early-returns, dropping `ddgi` — which frees every owned handle
        // (sampler, layouts, images) in field order.
        ddgi.init_transition_atlases(device)?;
        ddgi.ready = true;
        Ok(ddgi)
    }

    /// The atlas sampler (linear, clamp-to-edge) the mesh + trace passes read with.
    pub fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// The mesh set 5 (irradiance + distance samplers) the scene pass binds when DDGI ran this
    /// frame.
    pub fn mesh_set(&self) -> vk::DescriptorSet {
        self.mesh_set
    }

    /// The per-frame ray image handle + view + tracked layout.
    pub fn rays(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (self.rays.handle(), self.rays.view(), self.rays.layout)
    }

    /// Writes back the ray image's resolved layout after the graph executes.
    pub fn set_rays_layout(&mut self, layout: vk::ImageLayout) {
        self.rays.layout = layout;
    }

    /// The irradiance atlas handle + view + tracked layout.
    pub fn irradiance(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (
            self.irradiance.handle(),
            self.irradiance.view(),
            self.irradiance.layout,
        )
    }

    /// Writes back the irradiance atlas's resolved layout after the graph executes.
    pub fn set_irradiance_layout(&mut self, layout: vk::ImageLayout) {
        self.irradiance.layout = layout;
    }

    /// The distance (moment) atlas handle + view + tracked layout.
    pub fn distance(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (
            self.distance.handle(),
            self.distance.view(),
            self.distance.layout,
        )
    }

    /// Writes back the distance atlas's resolved layout after the graph executes.
    pub fn set_distance_layout(&mut self, layout: vk::ImageLayout) {
        self.distance.layout = layout;
    }

    /// The trace pass's PSO set (set 2: albedo cache + prev-irradiance + ray storage).
    pub fn trace_set(&self) -> vk::DescriptorSet {
        self.trace_set
    }

    /// The blend-irradiance pass's PSO set.
    pub fn blend_irr_set(&self) -> vk::DescriptorSet {
        self.blend_irr_set
    }

    /// The blend-distance pass's PSO set.
    pub fn blend_dist_set(&self) -> vk::DescriptorSet {
        self.blend_dist_set
    }

    /// The border-copy pass's PSO set.
    pub fn border_set(&self) -> vk::DescriptorSet {
        self.border_set
    }

    /// The trace compute set layout (set 2: albedo sampler + prev-irradiance sampler + ray storage).
    pub fn trace_layout(&self) -> vk::DescriptorSetLayout {
        self.trace_layout
    }

    /// The blend-irradiance compute set layout (ray sampler + irradiance storage).
    pub fn blend_irr_layout(&self) -> vk::DescriptorSetLayout {
        self.blend_irr_layout
    }

    /// The blend-distance compute set layout (ray sampler + distance storage).
    pub fn blend_dist_layout(&self) -> vk::DescriptorSetLayout {
        self.blend_dist_layout
    }

    /// The border-copy compute set layout (irradiance storage).
    pub fn border_layout(&self) -> vk::DescriptorSetLayout {
        self.border_layout
    }

    /// Toggles DDGI; turning it on re-converges the probes from scratch by arming a history reset.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.use_ddgi {
            self.history_reset = true;
        }
        self.use_ddgi = enabled;
    }

    /// Arms a temporal history reset — the first frame after an enable or a resize blends with no
    /// history.
    pub fn reset_history(&mut self) {
        self.history_reset = true;
    }

    /// Whether the next frame's blend is the whole-volume first frame (no temporal history) — read
    /// by the blend passes' first-frame push field.
    pub fn history_reset(&self) -> bool {
        self.history_reset
    }

    /// The current trace ray-set frame index.
    pub fn frame_index(&self) -> u32 {
        self.frame_index
    }

    /// The camera-centered volume placement (world-space min corner + size).
    pub fn volume(&self) -> (Vec3, Vec3) {
        (self.volume_min, self.volume_extent)
    }

    /// Whether the four DDGI passes run this frame: on + ready + the PSOs are present. Pure logic,
    /// so the named acceptance test can assert it without a device.
    pub fn wants_ddgi(&self, pipelines_ready: bool) -> bool {
        self.use_ddgi && self.ready && pipelines_ready
    }

    /// Whether DDGI is on and ready (the mesh-sample gate).
    pub fn enabled(&self) -> bool {
        self.use_ddgi && self.ready
    }

    /// The probe-grid uvec4 (`xyz` probes/axis, `w` irradiance interior) folded into the light UBO
    /// so the mesh fragment locates probes.
    pub fn probe_count_ubo(&self) -> UVec4 {
        UVec4::new(
            DDGI_PROBES_X,
            DDGI_PROBES_Y,
            DDGI_PROBES_Z,
            DDGI_IRR_INTERIOR,
        )
    }

    /// The toroidal scroll base (`wrapMod(snapBase, count)`) folded into the light UBO so the mesh
    /// fragment maps a logical probe index to its physical atlas tile.
    pub fn scroll_base_ubo(&self) -> UVec4 {
        let s = self.scroll_base();
        UVec4::new(s.x as u32, s.y as u32, s.z as u32, 0)
    }

    /// Snaps the camera-centered volume to the probe grid and stores the sun for the trace. A
    /// no-op when not ready. The volume origin snaps so the cage centers on the camera without
    /// swimming sub-cell; the toroidal scroll base + the per-frame delta drive probe relocation.
    pub fn set_scene(&mut self, cam_pos: Vec3, sun_dir: Vec3, sun_color: Vec3, sun_intensity: f32) {
        if !self.ready {
            return;
        }
        let count = IVec3::new(
            DDGI_PROBES_X as i32,
            DDGI_PROBES_Y as i32,
            DDGI_PROBES_Z as i32,
        );
        // Snap the min corner to the probe grid: the cage's centre tracks the camera, snapped to a
        // whole cell so the field does not shimmer as the camera creeps.
        let snapped = (cam_pos / DDGI_PROBE_SPACING).round().as_ivec3();
        self.snap_base = snapped - count / 2;
        self.volume_min = self.snap_base.as_vec3() * DDGI_PROBE_SPACING;
        self.volume_extent = count.as_vec3() * DDGI_PROBE_SPACING;
        self.sun_dir = sun_dir;
        self.sun_color = sun_color;
        self.sun_intensity = sun_intensity;
    }

    /// Advances the temporal state after a frame's four passes are recorded: records this frame's
    /// snapped base into the cycle ring (so it becomes the scroll reference one round-robin cycle
    /// later), bumps the trace ray-set / round-robin index, and clears the history-reset flag (so
    /// the next frame blends with history).
    pub fn advance_frame(&mut self) {
        self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE] = self.snap_base;
        self.frame_index = self.frame_index.wrapping_add(1);
        self.history_reset = false;
    }

    /// The toroidal scroll base for this frame (`wrapMod(snapBase, count)` per axis) — the physical
    /// tile of the probe at logical cell 0.
    fn scroll_base(&self) -> IVec3 {
        IVec3::new(
            wrap_mod(self.snap_base.x, DDGI_PROBES_X as i32),
            wrap_mod(self.snap_base.y, DDGI_PROBES_Y as i32),
            wrap_mod(self.snap_base.z, DDGI_PROBES_Z as i32),
        )
    }

    /// This frame's round-robin probe-budget offset (advances by the budget each frame, wrapping).
    fn round_robin_offset(&self) -> u32 {
        self.frame_index.wrapping_mul(DDGI_PROBE_BUDGET) % DDGI_PROBE_TOTAL
    }

    /// The trace push for this frame. `sdf_count` is the renderer's active SDF-instance count (the
    /// near-field MDF loop bound).
    pub fn trace_push(&self, sdf_count: u32) -> TracePush {
        let s = self.scroll_base();
        TracePush {
            probe_count: UVec4::new(
                DDGI_PROBES_X,
                DDGI_PROBES_Y,
                DDGI_PROBES_Z,
                DDGI_RAYS_PER_PROBE,
            ),
            volume_min: self.volume_min.extend(DDGI_IRR_INTERIOR as f32),
            volume_extent: self.volume_extent.extend(0.0),
            sun_dir: self.sun_dir.extend(self.sun_intensity),
            sun_color: self.sun_color.extend(self.frame_index as f32),
            budget_offset: Vec4::new(self.round_robin_offset() as f32, 0.0, 0.0, 0.0),
            scroll_base: IVec4::new(s.x, s.y, s.z, sdf_count as i32),
        }
    }

    /// The blend-irradiance push (tile interior + first-frame flag + round-robin offset/budget +
    /// hysteresis + the scroll geometry for probe relocation).
    pub fn blend_irradiance_push(&self) -> BlendPush {
        self.blend_push(DDGI_IRR_INTERIOR, 0.0)
    }

    /// The blend-distance push (the distance tile interior + the volume diagonal as the moment-
    /// normalization max distance).
    pub fn blend_distance_push(&self) -> BlendPush {
        self.blend_push(DDGI_DIST_INTERIOR, self.volume_extent.length())
    }

    /// Shared blend push (interior + max-distance differ between the two passes).
    fn blend_push(&self, interior: u32, max_dist: f32) -> BlendPush {
        let s = self.scroll_base();
        // Scroll accumulated over one round-robin cycle (this frame's base minus the base a cycle
        // ago, the slot the ring holds for the current frame index): a probe re-rayed this frame
        // was last traced exactly a cycle ago, so this is its newly-exposed reference.
        let cycle_base = self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE];
        let delta = self.snap_base - cycle_base;
        BlendPush {
            probe_count: UVec4::new(
                DDGI_PROBES_X,
                DDGI_PROBES_Y,
                DDGI_PROBES_Z,
                DDGI_RAYS_PER_PROBE,
            ),
            tile: UVec4::new(
                interior,
                u32::from(self.history_reset),
                self.round_robin_offset(),
                DDGI_PROBE_BUDGET,
            ),
            params: Vec4::new(DDGI_HYSTERESIS, max_dist, self.ray_rotation(), 0.0),
            scroll_base: IVec4::new(s.x, s.y, s.z, DDGI_PROBE_TOTAL as i32),
            delta: IVec4::new(delta.x, delta.y, delta.z, 0),
        }
    }

    /// The per-frame golden-ratio rotation the trace applies to its Fibonacci ray set
    /// (`frac(frameIndex · φ)`, `φ` the golden-ratio conjugate). The blend integrates the stored
    /// radiance against the SAME rotated directions, so it must reconstruct the identical rotation.
    fn ray_rotation(&self) -> f32 {
        const GOLDEN_CONJ: f32 = 0.618_034; // golden-ratio conjugate (matches the `.slang` literal)
        (self.frame_index as f32 * GOLDEN_CONJ).fract()
    }

    /// The border-copy push (probe grid + the irradiance tile interior).
    pub fn border_push(&self) -> BorderPush {
        BorderPush {
            probe_count: UVec4::new(DDGI_PROBES_X, DDGI_PROBES_Y, DDGI_PROBES_Z, 0),
            tile: UVec4::new(DDGI_IRR_INTERIOR, 0, 0, 0),
        }
    }

    /// Writes the GDF lite albedo cache into the trace set (set 2, binding 0), sampled with the
    /// GDF's linear-**repeat** sampler so the toroidal `wp / worldExtent` UVW wraps. Called once at
    /// construction, after the [`crate::GlobalSdf`] is built, so the trace's hit radiance has its
    /// albedo source before any dispatch.
    pub fn bind_gdf_albedo(&self, global_sdf: &crate::GlobalSdf) {
        let raw = self.resources.device();
        let (_, view, _) = global_sdf.albedo_cache();
        write_combined_sampler(
            raw,
            self.trace_set,
            0,
            view,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            global_sdf.cascade_sampler(),
        );
    }

    /// Binds the live raw-radiance sky SH buffer used for trace misses.
    pub fn bind_sky_sh(&self, sh: &Buffer) {
        let info = [vk::DescriptorBufferInfo::default()
            .buffer(sh.handle())
            .offset(0)
            .range(sh.size())];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(self.trace_set)
            .dst_binding(3)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&info)];
        unsafe { self.resources.device().update_descriptor_sets(&write, &[]) };
    }

    /// Writes the (image never reallocate) static descriptors into the five sets: the
    /// trace/blend/border compute sets + the mesh set 5. The trace set's albedo binding (b0) is
    /// written separately by [`Ddgi::bind_gdf_albedo`] (it lives in the GDF sub-state).
    fn write_static_descriptors(&self) {
        let raw = self.resources.device();
        let general = vk::ImageLayout::GENERAL;
        let ro = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        // trace set 2: albedo cache (b0, bind_gdf_albedo) + prev-irradiance sampler (b1) + ray
        // storage (b2) + live sky SH (b3, bound by `bind_sky_sh`).
        write_combined_sampler(
            raw,
            self.trace_set,
            1,
            self.irradiance.view(),
            ro,
            self.sampler,
        );
        write_storage_image(raw, self.trace_set, 2, self.rays.view(), general);
        // blend irradiance: ray sampler (b0) + irradiance storage (b1).
        write_combined_sampler(
            raw,
            self.blend_irr_set,
            0,
            self.rays.view(),
            ro,
            self.sampler,
        );
        write_storage_image(raw, self.blend_irr_set, 1, self.irradiance.view(), general);
        // blend distance: ray sampler (b0) + distance storage (b1).
        write_combined_sampler(
            raw,
            self.blend_dist_set,
            0,
            self.rays.view(),
            ro,
            self.sampler,
        );
        write_storage_image(raw, self.blend_dist_set, 1, self.distance.view(), general);
        // border: irradiance storage (b0).
        write_storage_image(raw, self.border_set, 0, self.irradiance.view(), general);
        // mesh set 5: irradiance (b0) + distance (b1) samplers.
        write_combined_sampler(
            raw,
            self.mesh_set,
            0,
            self.irradiance.view(),
            ro,
            self.sampler,
        );
        write_combined_sampler(
            raw,
            self.mesh_set,
            1,
            self.distance.view(),
            ro,
            self.sampler,
        );
    }

    /// One-shot init that clears all three images to zero and parks them in their resting layouts,
    /// waited idle. The two atlases land in `SHADER_READ_ONLY_OPTIMAL` (the mesh sample + the
    /// trace's prev-irradiance bind); the ray image lands in `GENERAL` (its storage-write layout)
    /// carrying the cleared zeros — so for the first round-robin cycle, before the budget has traced
    /// every probe, the full-volume blend passes read deterministic zero from untraced probe rows
    /// rather than undefined device memory (which could be NaN and lock the hysteresis lerp).
    fn init_transition_atlases(&mut self, device: &Device) -> Result<()> {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. The pool is freed at the end of this function.
        let pool = checked(
            unsafe { raw.create_command_pool(&pool_info, None) },
            "ddgi init pool",
        )?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = match unsafe { raw.allocate_command_buffers(&alloc) } {
            Ok(cmds) => cmds[0],
            Err(result) => {
                // SAFETY: the ash seam. Free the pool before returning.
                unsafe { raw.destroy_command_pool(pool, None) };
                return Err(crate::Error::Vk {
                    context: "ddgi init cmd",
                    result,
                });
            }
        };
        let fence = match unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) } {
            Ok(fence) => fence,
            Err(result) => {
                // SAFETY: the ash seam. Free the pool before returning.
                unsafe { raw.destroy_command_pool(pool, None) };
                return Err(crate::Error::Vk {
                    context: "ddgi init fence",
                    result,
                });
            }
        };

        let result = (|| -> Result<()> {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            let zero = vk::ClearColorValue { float32: [0.0; 4] };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let images = [
                self.irradiance.handle(),
                self.distance.handle(),
                self.rays.handle(),
            ];
            // SAFETY: the ash seam. The barriers + clears reference images this device created.
            unsafe {
                checked(raw.begin_command_buffer(cmd, &begin), "ddgi init begin")?;
                // UNDEFINED → GENERAL so the clear writes defined contents; the post-clear barrier's
                // GENERAL source then preserves the cleared zeros (an UNDEFINED source would let the
                // driver discard them).
                let pre: Vec<vk::ImageMemoryBarrier2> =
                    images.iter().map(|&i| clear_pre_barrier(i)).collect();
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&pre),
                );
                for &image in &images {
                    raw.cmd_clear_color_image(
                        cmd,
                        image,
                        vk::ImageLayout::GENERAL,
                        &zero,
                        &[range],
                    );
                }
                // Atlases → SHADER_READ_ONLY (the resting mesh-sample state); the ray image stays
                // GENERAL (its storage-write layout) with the cleared zeros intact.
                let sampled = (
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER
                        | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
                let post = [
                    clear_post_barrier(self.irradiance.handle(), sampled.0, sampled.1, sampled.2),
                    clear_post_barrier(self.distance.handle(), sampled.0, sampled.1, sampled.2),
                    clear_post_barrier(
                        self.rays.handle(),
                        vk::ImageLayout::GENERAL,
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_STORAGE_READ
                            | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                    ),
                ];
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&post),
                );
                checked(raw.end_command_buffer(cmd), "ddgi init end")?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The queue is touched single-threaded at init.
            unsafe {
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "ddgi init submit")?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "ddgi init wait",
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
        if result.is_ok() {
            self.irradiance.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            self.distance.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            self.rays.layout = vk::ImageLayout::GENERAL;
        }
        result
    }
}

impl Drop for Ddgi {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The `Arc<DeviceResources>` keeps the device alive for the call; the
        // run loop idled it before teardown (README §4). The sets are pool-owned (freed with the
        // descriptor pool in `Descriptors::drop`), so only the sampler + the four compute layouts
        // are destroyed here, each exactly once. The owned images Drop after this by field order.
        let raw = self.resources.device();
        unsafe {
            raw.destroy_descriptor_set_layout(self.trace_layout, None);
            raw.destroy_descriptor_set_layout(self.blend_irr_layout, None);
            raw.destroy_descriptor_set_layout(self.blend_dist_layout, None);
            raw.destroy_descriptor_set_layout(self.border_layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
    }
}

/// The four DDGI compute set layouts (the mesh set 5 layout is owned by `Descriptors`).
struct DdgiLayouts {
    trace: vk::DescriptorSetLayout,
    blend_irr: vk::DescriptorSetLayout,
    blend_dist: vk::DescriptorSetLayout,
    border: vk::DescriptorSetLayout,
}

impl DdgiLayouts {
    /// Frees every created layout (the partial-failure cleanup path).
    ///
    /// # Safety
    ///
    /// The device must be idle and each layout created exactly once.
    unsafe fn destroy(&self, raw: &ash::Device) {
        // SAFETY: forwarded from the caller's contract — each layout is freed once.
        unsafe {
            raw.destroy_descriptor_set_layout(self.trace, None);
            raw.destroy_descriptor_set_layout(self.blend_irr, None);
            raw.destroy_descriptor_set_layout(self.blend_dist, None);
            raw.destroy_descriptor_set_layout(self.border, None);
        }
    }
}

/// Builds the four compute set layouts, freeing what was created so far on any failure.
fn build_layouts(raw: &ash::Device) -> Result<DdgiLayouts> {
    let si = vk::DescriptorType::STORAGE_IMAGE;
    let cs = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;

    // trace set 2: albedo cache sampler (b0) + prev-irradiance sampler (b1) + ray storage (b2) +
    // live sky SH storage buffer (b3).
    let trace = make_compute_layout(raw, &[cs, cs, si, vk::DescriptorType::STORAGE_BUFFER])?;
    let blend_irr = match make_compute_layout(raw, &[cs, si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layout on this partial-failure path.
            unsafe { raw.destroy_descriptor_set_layout(trace, None) };
            return Err(err);
        }
    };
    let blend_dist = match make_compute_layout(raw, &[cs, si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layouts.
            unsafe {
                raw.destroy_descriptor_set_layout(blend_irr, None);
                raw.destroy_descriptor_set_layout(trace, None);
            }
            return Err(err);
        }
    };
    let border = match make_compute_layout(raw, &[si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layouts.
            unsafe {
                raw.destroy_descriptor_set_layout(blend_dist, None);
                raw.destroy_descriptor_set_layout(blend_irr, None);
                raw.destroy_descriptor_set_layout(trace, None);
            }
            return Err(err);
        }
    };

    Ok(DdgiLayouts {
        trace,
        blend_irr,
        blend_dist,
        border,
    })
}

/// Allocates the five DDGI sets (the four compute sets + the mesh set 5) from the shared descriptor
/// pool. The sets are pool-owned (freed with the pool in teardown).
fn allocate_sets(
    descriptors: &Descriptors,
    layouts: &DdgiLayouts,
) -> Result<(
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
)> {
    let trace = descriptors.allocate_set(layouts.trace)?;
    let blend_irr = descriptors.allocate_set(layouts.blend_irr)?;
    let blend_dist = descriptors.allocate_set(layouts.blend_dist)?;
    let border = descriptors.allocate_set(layouts.border)?;
    let mesh = descriptors.allocate_set(descriptors.ddgi_mesh_set_layout())?;
    Ok((trace, blend_irr, blend_dist, border, mesh))
}

/// A compute-stage set layout with one binding per `types` entry, in order (binding 0, 1, …). The
/// DDGI compute sets are all single-descriptor-per-binding.
fn make_compute_layout(
    raw: &ash::Device,
    types: &[vk::DescriptorType],
) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = types
        .iter()
        .enumerate()
        .map(|(i, &ty)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i as u32)
                .descriptor_type(ty)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop` (or the
    // partial-failure cleanup).
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ddgi compute layout",
    )
}

/// A single-mip, single-layer 2D color image with the given storage/sampled usage — the DDGI
/// atlases + ray image.
fn make_storage_image(
    resources: &Arc<DeviceResources>,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<Image> {
    Image::new(
        resources,
        &ImageDesc::color_2d(vk::Extent2D { width, height }, format, usage),
    )
}

/// The linear, clamp-to-edge sampler the mesh + trace passes read the atlases with.
fn create_linear_clamp_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
    // SAFETY: the ash seam. The sampler is owned and freed in `Drop`.
    checked(unsafe { raw.create_sampler(&info, None) }, "ddgi sampler")
}

/// `UNDEFINED → GENERAL` barrier readying an image (1 mip, 1 layer, color) for the init clear (a
/// transfer write).
fn clear_pre_barrier(image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource())
}

/// `GENERAL → new_layout` barrier parking an image in its resting layout after the init clear,
/// making the cleared zeros visible to `dst_stage`/`dst_access`. The `GENERAL` source preserves the
/// cleared contents (unlike an `UNDEFINED` source, which the driver may discard).
fn clear_post_barrier(
    image: vk::Image,
    new_layout: vk::ImageLayout,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource())
}

/// The full color subresource range (1 mip, 1 layer) shared by the init barriers.
fn color_subresource() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Writes a storage image into `(set, binding)` at `layout` (no sampler).
fn write_storage_image(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    view: vk::ImageView,
    layout: vk::ImageLayout,
) {
    let info = [vk::DescriptorImageInfo::default()
        .image_view(view)
        .image_layout(layout)];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
        .image_info(&info);
    // SAFETY: the ash seam. The set + view outlive the call; the write targets one binding the
    // set's layout declares.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

/// Writes a combined-image-sampler into `(set, binding)` at `layout`.
fn write_combined_sampler(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    view: vk::ImageView,
    layout: vk::ImageLayout,
    sampler: vk::Sampler,
) {
    let info = [vk::DescriptorImageInfo::default()
        .sampler(sampler)
        .image_view(view)
        .image_layout(layout)];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&info);
    // SAFETY: the ash seam. The set + view + sampler outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;

    /// Building the DDGI sub-state (the atlases + ray image + the four layouts/sets + mesh set 5 +
    /// the static descriptor writes + the one-shot atlas init barrier) is validation-clean on a
    /// software device — the GPU-runtime half of this phase the toolbox can actually run (the
    /// four-pass chain is all compute, no ray tracing). The per-frame trace render is exercised by
    /// the engine e2e once DDGI is wired through the control plane; here the resource bring-up + the
    /// init transition are validated. Skips cleanly when no Vulkan device is obtainable.
    #[test]
    fn ddgi_resource_bringup_is_validation_clean() {
        let device = match crate::Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = crate::Descriptors::new(&device, &free_list).expect("Descriptors");
        let before = validation_issue_count();

        let mut ddgi = Ddgi::new(&device, &descriptors).expect("Ddgi::new");
        // Built + ready, ON by default; the two atlases rest in ShaderReadOnly after the init
        // barrier (the mesh-sample resting state).
        assert!(ddgi.ready);
        assert!(ddgi.use_ddgi);
        assert_eq!(
            ddgi.irradiance().2,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        );
        assert_eq!(ddgi.distance().2, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        assert_ne!(ddgi.mesh_set(), vk::DescriptorSet::null());

        // A camera-centered scene set snaps the volume to the probe grid; the probe-grid UBO + the
        // scroll base match the constants.
        assert!(ddgi.history_reset());
        assert!(ddgi.enabled());
        ddgi.set_scene(
            Vec3::new(5.0, 2.0, -3.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::ONE,
            1.0,
        );
        // The volume centres on the camera (extent = count · spacing), snapped to a whole cell.
        let (vmin, vext) = ddgi.volume();
        assert_eq!(vext.x, DDGI_PROBES_X as f32 * DDGI_PROBE_SPACING);
        let center = vmin + vext * 0.5;
        assert!((center.x - 5.0).abs() <= DDGI_PROBE_SPACING);
        assert_eq!(ddgi.probe_count_ubo().x, DDGI_PROBES_X);

        drop(ddgi);
        // SAFETY: the device must idle before its sub-state Drops (here Ddgi already dropped,
        // freeing its sampler + layouts + images).
        device.wait_idle().expect("wait_idle");
        assert_eq!(
            validation_issue_count(),
            before,
            "the DDGI bring-up + init transition raised no validation issues"
        );
    }

    /// The push-constant structs byte-match the `.slang` `Push` layouts the SPIR-V reads — a wrong
    /// offset is a silently corrupted dispatch, so pin each size.
    #[test]
    fn ddgi_push_sizes_match_slang() {
        assert_eq!(size_of::<TracePush>(), 112);
        assert_eq!(size_of::<BlendPush>(), 80);
        assert_eq!(size_of::<BorderPush>(), 32);
    }

    /// The octahedral atlas dimensions follow `tilesPerRow · (interior + 2)`, matching the shaders'
    /// `atlasW`/`atlasH` derivation — a wrong gutter count corrupts the border copy + the bilinear
    /// sample. Pinned at the camera-centered clipmap's 16×8×16 probe grid.
    #[test]
    fn atlas_dimensions_match_octahedral_tiling() {
        assert_eq!(irradiance_atlas_width(), 16 * 8 * (8 + 2));
        assert_eq!(irradiance_atlas_height(), 16 * (8 + 2));
        assert_eq!(distance_atlas_width(), 16 * 8 * (16 + 2));
        assert_eq!(distance_atlas_height(), 16 * (16 + 2));
        assert_eq!(DDGI_PROBE_TOTAL, 16 * 8 * 16);
        assert_eq!(DDGI_PROBE_BUDGET, DDGI_PROBE_TOTAL / 4);
    }

    /// Euclidean modulo wraps a negative cell to a positive physical tile (the toroidal scroll
    /// fold) — an off-by-one here crawls or flashes probes as the camera moves.
    #[test]
    fn wrap_mod_is_euclidean() {
        assert_eq!(wrap_mod(0, 16), 0);
        assert_eq!(wrap_mod(17, 16), 1);
        assert_eq!(wrap_mod(-1, 16), 15);
        assert_eq!(wrap_mod(-16, 16), 0);
        assert_eq!(wrap_mod(-17, 16), 15);
    }

    /// The pure DDGI state machine (enable/scene/advance/history/scroll) without a device, so the
    /// acceptance gate can assert the camera-snap + scroll-delta + history-reset behavior the named
    /// tests require. Mirrors the production fields the methods touch.
    #[derive(Default)]
    struct DdgiState {
        use_ddgi: bool,
        ready: bool,
        history_reset: bool,
        frame_index: u32,
        snap_base: IVec3,
        snap_base_ring: [IVec3; DDGI_PROBE_CYCLE],
    }

    impl DdgiState {
        fn set_enabled(&mut self, enabled: bool) {
            if enabled && !self.use_ddgi {
                self.history_reset = true;
            }
            self.use_ddgi = enabled;
        }

        fn set_scene(&mut self, cam_pos: Vec3) {
            if !self.ready {
                return;
            }
            let count = IVec3::new(
                DDGI_PROBES_X as i32,
                DDGI_PROBES_Y as i32,
                DDGI_PROBES_Z as i32,
            );
            let snapped = (cam_pos / DDGI_PROBE_SPACING).round().as_ivec3();
            self.snap_base = snapped - count / 2;
        }

        fn advance_frame(&mut self) {
            self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE] = self.snap_base;
            self.frame_index = self.frame_index.wrapping_add(1);
            self.history_reset = false;
        }

        /// Scroll accumulated over one round-robin cycle — the reset reference for a probe re-rayed
        /// this frame (it was last traced exactly a cycle ago).
        fn delta(&self) -> IVec3 {
            self.snap_base - self.snap_base_ring[self.frame_index as usize % DDGI_PROBE_CYCLE]
        }

        fn wants_ddgi(&self, pipelines_ready: bool) -> bool {
            self.use_ddgi && self.ready && pipelines_ready
        }
    }

    /// The four DDGI passes run only when DDGI is on AND the resources/PSOs are ready; absent
    /// otherwise (the acceptance gate's first bullet). Default-on, so they run from boot.
    #[test]
    fn wants_ddgi_only_when_on_ready_and_pipelines_present() {
        let mut s = DdgiState {
            use_ddgi: true,
            ready: true,
            ..Default::default()
        };
        // On + ready + PSOs present → the chain runs (default state at boot).
        assert!(s.wants_ddgi(true));
        // On + ready but the PSOs failed to build → no passes.
        assert!(!s.wants_ddgi(false));
        // Off → never, whatever the pipelines.
        s.set_enabled(false);
        assert!(!s.wants_ddgi(true));
        // Not ready → never (e.g. a creation failure left `ready` false).
        s.set_enabled(true);
        s.ready = false;
        assert!(!s.wants_ddgi(true));
    }

    /// `set_scene` snaps the volume's min corner to the probe grid so the cage centres on the
    /// camera; the cycle-accumulated scroll delta (the toroidal relocation reference, measured over
    /// one round-robin cycle) is zero for a sub-cell creep and one cell for a whole-cell move. A
    /// no-op before `ready`.
    #[test]
    fn set_scene_snaps_volume_and_tracks_scroll() {
        let mut s = DdgiState {
            use_ddgi: true,
            ..Default::default()
        };
        // Before ready → no-op (stale zero base).
        s.set_scene(Vec3::new(100.0, 0.0, 0.0));
        assert_eq!(s.snap_base, IVec3::ZERO);

        s.ready = true;
        s.set_scene(Vec3::ZERO);
        // Centred on the origin: min corner is -count/2 cells.
        assert_eq!(s.snap_base.x, -(DDGI_PROBES_X as i32) / 2);
        // Fill the cycle ring with a stable base so the cycle-delta references it.
        for _ in 0..DDGI_PROBE_CYCLE {
            s.set_scene(Vec3::ZERO);
            s.advance_frame();
        }
        // A sub-cell creep that does not cross a probe cell → no scroll over the cycle.
        s.set_scene(Vec3::new(DDGI_PROBE_SPACING * 0.3, 0.0, 0.0));
        assert_eq!(s.delta(), IVec3::ZERO);
        // A whole-cell move scrolls the cage by exactly one cell relative to a cycle ago.
        s.set_scene(Vec3::new(DDGI_PROBE_SPACING, 0.0, 0.0));
        assert_eq!(s.delta(), IVec3::new(1, 0, 0));
    }

    /// A one-cell scroll stays visible in the cycle-accumulated delta for a full round-robin cycle
    /// — so every probe that scrolled in is still flagged newly-exposed on whatever frame the
    /// budget finally re-rays it, with no probe slipping through between slices (defect: a
    /// single-frame delta dropped the flag before the ~3/4 of the slab outside that frame's slice
    /// was retraced, leaving a ghosting trail). After a full cycle at rest the reference catches up,
    /// so a held camera raises no spurious reset.
    #[test]
    fn scroll_exposure_persists_across_a_round_robin_cycle() {
        let mut s = DdgiState {
            use_ddgi: true,
            ready: true,
            ..Default::default()
        };
        // Stabilise the cycle ring at the origin base.
        for _ in 0..DDGI_PROBE_CYCLE {
            s.set_scene(Vec3::ZERO);
            s.advance_frame();
        }
        // One whole-cell scroll, then hold the camera still for a full cycle: the cycle-delta keeps
        // reporting the one-cell scroll until the moved base propagates through the ring.
        let moved = Vec3::new(DDGI_PROBE_SPACING, 0.0, 0.0);
        for _ in 0..DDGI_PROBE_CYCLE {
            s.set_scene(moved);
            assert_eq!(s.delta(), IVec3::new(1, 0, 0));
            s.advance_frame();
        }
        // After a full cycle at the new position the reference catches up → no spurious reset.
        s.set_scene(moved);
        assert_eq!(s.delta(), IVec3::ZERO);
    }

    /// `history_reset` is set on enable and stays set until a frame is recorded, then
    /// `advance_frame` clears it; re-enabling re-arms it (the acceptance gate — set on
    /// enable/resize, cleared on subsequent frames). `advance_frame` also commits the scroll base.
    #[test]
    fn history_reset_arms_on_enable_and_clears_after_a_frame() {
        let mut s = DdgiState {
            ready: true,
            ..Default::default()
        };
        // Enabling from off arms the reset.
        s.set_enabled(true);
        assert!(s.history_reset);
        // The first recorded frame consumes it + records the scroll base into the cycle ring.
        s.set_scene(Vec3::new(20.0, 0.0, 0.0));
        s.advance_frame();
        assert!(!s.history_reset);
        assert_eq!(s.frame_index, 1);
        assert_eq!(s.snap_base_ring[0], s.snap_base);
        // A second frame keeps it cleared and bumps the index.
        s.advance_frame();
        assert!(!s.history_reset);
        assert_eq!(s.frame_index, 2);
        // Re-enabling while already on does NOT re-arm (only an off→on edge does).
        s.set_enabled(true);
        assert!(!s.history_reset);
        // A resize / explicit reset re-arms it.
        s.history_reset = true;
        assert!(s.history_reset);
    }
}
