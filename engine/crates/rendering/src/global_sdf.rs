//! The Global Distance Field (GDF): a camera-centered cascade clipmap that composites the
//! per-mesh Mesh-Distance-Field (MDF) bricks into a small device-resident distance volume per
//! cascade, so every distance query beyond the near field is **one trilinear tap** independent of
//! how many meshes are in the scene (the opposite of the O(instances) `min()` loop the per-mesh
//! cone trace pays).
//!
//! It owns one [`Image3D`] per cascade (the toroidally-addressed `R16_SNORM` distance volume,
//! persistent across frames so the clipmap updates incrementally), plus a per-frame-in-flight slot
//! carrying the device-local cull-list SSBO the GPU rebuilds each frame, the host-mapped params UBO
//! the lighting consumers read, and the cull + composite descriptor sets. The cull list and params
//! UBO are GPU-cleared-and-rebuilt / CPU-rewritten every frame, so each frame in flight needs its
//! own copy — frame `N+1` must not clobber frame `N`'s still-in-flight reads (the same per-frame
//! slotting [`crate::Lighting`]'s cluster buffer uses). The linear-**repeat** cascade sampler and
//! the two compute set layouts are shared across slots.
//!
//! Built once in [`GlobalSdf::new`], then borrowed `&GlobalSdf` by the frame-graph build (its
//! handles are immutable after init); the per-frame placement + toroidal recenter is written
//! through [`GlobalSdf::set_camera`] / [`GlobalSdf::set_enabled`] / [`GlobalSdf::advance_frame`]
//! (`&mut self.global_sdf`).
//!
//! # The two passes
//!
//! 1. `gdf-cull` — bins each [`crate::SdfInstance`]'s world AABB into the cascade(s) it touches,
//!    atomic-appending its index into a per-cascade compacted list.
//! 2. `gdf-composite` — for each dirty voxel of a cascade, `min()`s the culled bricks' signed
//!    distance and writes the toroidal `R16_SNORM` volume; empty voxels store the conservative
//!    open distance (`+max-encode`, never zero) so the sphere-march leaps across empty space. The
//!    finest cascade also splats each cell's nearest-occluder **base color** into the lite albedo
//!    cache (an `rgba16f` volume the DDGI trace reads as its hit radiance's albedo). The albedo
//!    cache is a **flat per-cell base color** — no normal, no view-dependent shading, no emissive,
//!    no multi-material resolution within a cell — a coarse radiance approximation, explicitly
//!    **not** a Surface Cache; it is the known fidelity cap of the DDGI hit term.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{IVec3, IVec4, UVec3, UVec4, Vec3, Vec4};

use crate::descriptors::Descriptors;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{Buffer, DeviceResources, Image3D};
use crate::{Device, Result, checked};

/// Cascades in the clipmap (finest first).
pub const GDF_CASCADES: u32 = 3;
/// Each cascade volume is `GDF_RES`³ voxels.
pub const GDF_RES: u32 = 128;
/// A full cascade refresh composites in this many per-frame z-slabs, bounding any one
/// frame's composite volume (a whole `GDF_RES`³ recomposite in one command buffer is the
/// kind of multi-second submission a platform GPU watchdog kills).
pub const GDF_FULL_SLABS: u32 = 8;
/// Each cascade covers `GDF_EXPONENT×` the world extent of the previous.
pub const GDF_EXPONENT: f32 = 2.0;
/// The finest cascade's full world extent (metres). Its voxel is `GDF_CASCADE0_EXTENT / GDF_RES`
/// (≈ 0.25 m), resolving the near-field handoff scale; cascade `c` covers
/// `GDF_CASCADE0_EXTENT · GDF_EXPONENT^c`.
pub const GDF_CASCADE0_EXTENT: f32 = 32.0;
/// The cascade volume format — `R16_SNORM`, matching the per-mesh brick atlas encode so the
/// composite `min()` is in the same normalized space.
pub const GDF_FORMAT: vk::Format = vk::Format::R16_SNORM;
/// The porous-occupancy cascade format — `R8_UNORM` density in [0, 1]; `0` = open or solid
/// (solid matter lives in the distance field), fractional = porous aggregate matter the
/// consumers march THROUGH, accumulating Beer–Lambert extinction instead of hitting.
pub const GDF_OCCUPANCY_FORMAT: vk::Format = vk::Format::R8_UNORM;
/// The lite albedo-cache format — `rgba16f` (rgb base color, a = the written flag). Aligned to the
/// finest cascade (`GDF_RES`³, toroidal); read by the DDGI trace at hit points.
pub const GDF_ALBEDO_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
/// The near per-mesh field radius (metres): the cone march taps the full-resolution per-mesh MDF
/// for `t < GDF_NEAR_HANDOFF`, the Global-SDF beyond (the README's leak mitigation).
pub const GDF_NEAR_HANDOFF: f32 = 2.0;
/// The overlap-band fraction of a cascade's half-extent over which it blends into the next coarser
/// cascade (so the cascade boundary shows no shell).
pub const GDF_BAND_FRACTION: f32 = 0.15;
/// Maximum MDF occluders binned per cascade per frame (the per-cascade cull-list segment).
pub const GDF_MAX_CULLED: u32 = 256;
/// Header `u32`s before the per-cascade index segments in the cull list (the per-cascade atomic
/// counters + pad). Matches `gdf_cull.slang` / `gdf_composite.slang`'s `kCullHeader`.
pub const GDF_CULL_HEADER: u32 = 4;

/// The cull push: per-cascade world bounds + the instance count + the per-cascade list capacity.
/// 64 bytes, matching `gdf_cull.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GdfCullPush {
    /// Per-cascade `xyz` = world center, `w` = half-extent (world).
    pub cascade: [Vec4; GDF_CASCADES as usize],
    /// `x` = instance count, `y` = max culled per cascade, `z` = cascade count, `w` reserved.
    pub counts: UVec4,
}

const _: () = assert!(size_of::<GdfCullPush>() == 64);

/// The composite push: one dirty region (global voxel coords) of one cascade + the cascade's
/// voxel/encode scale. 48 bytes, matching `gdf_composite.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GdfCompositePush {
    /// `xyz` = region min in global voxel coords, `w` = cascade index.
    pub global_base: IVec4,
    /// `xyz` = region dims (voxels), `w` = max culled per cascade.
    pub size: UVec4,
    /// `x` = voxel size (world), `y` = max-encode dist (world), `z` = resolution, `w` reserved.
    pub cascade_info: Vec4,
}

const _: () = assert!(size_of::<GdfCompositePush>() == 48);

/// One cascade's world placement + encode scale, folded into the params UBO the consumers read.
/// 32 bytes, matching `sdf.slang`'s `GdfCascade`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GdfCascadeUbo {
    /// `xyz` = snapped world center, `w` = half-extent (world).
    pub center_half: Vec4,
    /// `x` = voxel size, `y` = max-encode dist (world), `z` = world extent (= voxel·res), `w` reserved.
    pub voxel_encode: Vec4,
}

/// The Global-SDF params UBO (light set binding 10). 112 bytes, matching `sdf.slang`'s `GdfParams`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GdfParamsUbo {
    /// Per-cascade placement + encode.
    pub cascades: [GdfCascadeUbo; GDF_CASCADES as usize],
    /// `x` = enabled (0/1), `y` = active cascade count, `z` = near-handoff radius, `w` = overlap-band fraction.
    pub control: Vec4,
}

const _: () = assert!(size_of::<GdfParamsUbo>() == 112);

/// One dirty region to composite: a global-voxel-coordinate min corner + dimensions, for one
/// cascade. Pure data so the toroidal recenter helper is device-free unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GdfRegion {
    /// The region's min corner in global (camera-independent) voxel coordinates.
    pub base: IVec3,
    /// The region's dimensions in voxels.
    pub size: UVec3,
}

/// The finest cascade's full world extent for cascade `c` (`GDF_CASCADE0_EXTENT · GDF_EXPONENT^c`).
pub fn cascade_world_extent(c: u32) -> f32 {
    GDF_CASCADE0_EXTENT * GDF_EXPONENT.powi(c as i32)
}

/// The world size of one voxel in cascade `c`.
pub fn cascade_voxel_size(c: u32) -> f32 {
    cascade_world_extent(c) / GDF_RES as f32
}

/// Half the world extent of cascade `c` (the clipmap window radius).
pub fn cascade_half_extent(c: u32) -> f32 {
    cascade_world_extent(c) * 0.5
}

/// The `R16_SNORM` encode clamp (world units) for cascade `c` — a quarter of the cascade extent,
/// so empty space saturates to a generous positive leap and near-surface gradients keep precision.
pub fn cascade_max_encode(c: u32) -> f32 {
    cascade_world_extent(c) * 0.25
}

/// The camera-snapped center of cascade `c` in global voxel coordinates (snapped to the cascade's
/// own voxel grid so the field does not shimmer sub-voxel as the camera creeps).
pub fn cascade_center_voxel(c: u32, eye: Vec3) -> IVec3 {
    (eye / cascade_voxel_size(c)).round().as_ivec3()
}

/// The dirty regions to recomposite for a cascade this frame, in global voxel coordinates. A full
/// region (the whole `res³` window) when the cascade has no history, scrolled more than a full
/// window, or is this frame's round-robin full refresh; otherwise up to three axis slabs covering
/// exactly the voxels the recenter scrolled in (toroidal incremental update). The composite write
/// coordinates wrap modulo `res`, so the global-coordinate slabs need no wrap handling here.
pub fn cascade_dirty_regions(
    prev: IVec3,
    cur: IVec3,
    has_history: bool,
    round_robin_full: bool,
    res: i32,
) -> Vec<GdfRegion> {
    let half = res / 2;
    let lo = cur - IVec3::splat(half);
    let delta = cur - prev;
    let scrolled_out = delta.abs().max_element() >= res;
    if !has_history || round_robin_full || scrolled_out {
        return vec![GdfRegion {
            base: lo,
            size: UVec3::splat(res as u32),
        }];
    }
    let mut regions = Vec::new();
    for axis in 0..3usize {
        let d = delta[axis];
        if d == 0 {
            continue;
        }
        let mut base = lo;
        let mut size = IVec3::splat(res);
        if d > 0 {
            // Cells entered at the high end: [prev_hi, cur_hi).
            base[axis] = cur[axis] + half - d;
            size[axis] = d;
        } else {
            // Cells entered at the low end: [cur_lo, prev_lo).
            base[axis] = cur[axis] - half;
            size[axis] = -d;
        }
        regions.push(GdfRegion {
            base,
            size: size.as_uvec3(),
        });
    }
    regions
}

/// Deduplicates exactly-equal regions and, past a small cap, collapses the set to a single full
/// window (many tiny composite dispatches cost more than one full one). Overlaps are otherwise left
/// as-is — the composite min-blend is idempotent, so a voxel recomposited twice is harmless.
fn merge_regions(mut regions: Vec<GdfRegion>) -> Vec<GdfRegion> {
    regions.retain(|r| r.size.min_element() > 0);
    regions.dedup();
    if regions.len() > GDF_MAX_DIRTY_INSTANCES {
        // Collapse around the widest region's center is unnecessary: the caller only reaches here
        // with bounded moved-occluder counts, so just union into one covering window.
        let lo = regions
            .iter()
            .map(|r| r.base)
            .reduce(|a, b| a.min(b))
            .unwrap_or(IVec3::ZERO);
        let hi = regions
            .iter()
            .map(|r| r.base + r.size.as_ivec3())
            .reduce(|a, b| a.max(b))
            .unwrap_or(IVec3::ZERO);
        return vec![GdfRegion {
            base: lo,
            size: (hi - lo).as_uvec3(),
        }];
    }
    regions
}

/// One frame-in-flight's GDF buffers + descriptor sets: the GPU-rebuilt cull-list SSBO (cleared +
/// atomic-appended by `gdf-cull`, read by `gdf-composite`), the host-mapped params UBO the lighting
/// consumers read, and the cull + composite compute sets binding them. Slotted per frame so frame
/// `N+1`'s clear/rebuild never races frame `N`'s still-in-flight reads (the cluster-buffer pattern
/// in [`crate::Lighting`]).
struct FrameGdf {
    cull_buffer: Buffer,
    params_ubo: Buffer,
    cull_set: vk::DescriptorSet,
    composite_set: vk::DescriptorSet,
    scatter_set: vk::DescriptorSet,
}

/// The Global-SDF sub-state: the shared cascade volumes + sampler + compute layouts, and a
/// per-frame-in-flight slot ([`FrameGdf`]) carrying the cull-list SSBO, params UBO, and compute sets.
///
/// Owns an [`Arc`]`<`[`DeviceResources`]`>` so its handles free in [`Drop`] without a live
/// `&Device`. The light-set *layout* is owned by [`Descriptors`]; this struct writes the cascade
/// samplers + params UBO into the light set via [`GlobalSdf::write_light_set`].
pub struct GlobalSdf {
    resources: Arc<DeviceResources>,

    /// On by default — it is the distance oracle the DDGI trace sphere-marches (its two compute
    /// passes per frame and the far-field GDF tap are the cost of that indirect path).
    pub use_gdf: bool,
    /// Resources + sets valid. True after [`GlobalSdf::new`].
    pub ready: bool,

    cascades: Vec<Image3D>,
    /// One porous-occupancy volume per cascade (toroidal `R8_UNORM`, the same addressing as
    /// its distance cascade): the aggregate density porous matter splats where solid matter
    /// would have written distance.
    occupancy: Vec<Image3D>,
    /// The lite per-cell albedo cache, aligned to the finest cascade (`GDF_RES`³, toroidal). The
    /// DDGI trace reads it for hit radiance; the composite splats it for cascade 0.
    albedo: Image3D,
    frames: Vec<FrameGdf>,
    sampler: vk::Sampler,

    cull_layout: vk::DescriptorSetLayout,
    composite_layout: vk::DescriptorSetLayout,
    scatter_layout: vk::DescriptorSetLayout,

    /// The camera eye this frame (the cascade placement center before snapping).
    eye: Vec3,
    /// Per-cascade snapped center this frame (global voxel coords), set by [`GlobalSdf::set_camera`].
    cur_center: [IVec3; GDF_CASCADES as usize],
    /// Per-cascade snapped center last composited frame (the toroidal delta base).
    prev_center: [IVec3; GDF_CASCADES as usize],
    /// Per-cascade history flag — false until the cascade has had a full composite (forces a full
    /// refresh on the first frame after enable / bring-up).
    has_history: [bool; GDF_CASCADES as usize],
    /// Per-cascade in-progress full-refresh cursor: the next z-slab (of
    /// [`GDF_FULL_SLABS`]) to composite, or `GDF_FULL_SLABS` when no full refresh is in
    /// flight. Advanced in [`GlobalSdf::advance_frame`].
    full_slab: [u32; GDF_CASCADES as usize],
    /// Round-robins the staggered far-cascade full refresh (one cascade fully refreshed per frame).
    frame: u32,
}

/// Past this many dirty regions in one frame the set collapses to one covering window —
/// the per-region dispatch overhead would exceed the win.
const GDF_MAX_DIRTY_INSTANCES: usize = 24;

/// The world-space bounds an occluder must intersect to affect global illumination, given an
/// eye position: the outermost cascade's window, dilated by one cascade-0 extent.
///
/// An occluder outside the coarsest cascade cannot influence any march, so this is the correct
/// gate for the GI occluder list — **not** the camera frustum, which would drop occluders that
/// shadow visible surfaces from off-screen.
///
/// The dilation covers a frame of camera motion: the window is derived from the eye the gather
/// sees, and the cascades re-centre later in the frame, so a margin keeps the cull conservative
/// rather than racing that ordering.
#[must_use]
pub fn gi_occluder_bounds(eye: Vec3) -> (Vec3, Vec3) {
    let coarsest = GDF_CASCADES - 1;
    let centre = cascade_center_voxel(coarsest, eye).as_vec3() * cascade_voxel_size(coarsest);
    let reach = cascade_half_extent(coarsest) + GDF_CASCADE0_EXTENT;
    (centre - Vec3::splat(reach), centre + Vec3::splat(reach))
}

impl GlobalSdf {
    /// Allocates the cascade volumes + cull SSBO + params UBO, the linear-repeat sampler, the two
    /// compute set layouts + sets, writes the static descriptors, init-transitions the cascade
    /// volumes into `SHADER_READ_ONLY_OPTIMAL` (their resting state for the consumer sample), and
    /// seeds the params UBO. `ready` is set true on success; `use_gdf` defaults ON (the DDGI trace
    /// reads the GDF as its distance oracle).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; already-created handles are freed
    /// before returning on a partial failure.
    pub fn new(device: &Device, descriptors: &Descriptors) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        // One toroidal distance volume per cascade: STORAGE (composite write, GENERAL) + SAMPLED
        // (consumer read, ShaderReadOnly) + TRANSFER_DST (the one-shot open-space clear below,
        // which makes a partially-refreshed cascade read as open rather than garbage).
        let usage = vk::ImageUsageFlags::STORAGE
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_DST;
        let mut cascades = Vec::with_capacity(GDF_CASCADES as usize);
        for _ in 0..GDF_CASCADES {
            cascades.push(Image3D::new(
                &resources,
                vk::Extent3D {
                    width: GDF_RES,
                    height: GDF_RES,
                    depth: GDF_RES,
                },
                GDF_FORMAT,
                1,
                usage,
            )?);
        }

        // One porous-occupancy volume per cascade, addressed exactly like its distance
        // cascade.
        let mut occupancy = Vec::with_capacity(GDF_CASCADES as usize);
        for _ in 0..GDF_CASCADES {
            occupancy.push(Image3D::new(
                &resources,
                vk::Extent3D {
                    width: GDF_RES,
                    height: GDF_RES,
                    depth: GDF_RES,
                },
                GDF_OCCUPANCY_FORMAT,
                1,
                usage,
            )?);
        }

        // The lite albedo cache: one `GDF_RES`³ `rgba16f` volume aligned to the finest cascade,
        // STORAGE (composite write) + SAMPLED (DDGI trace read).
        let albedo = Image3D::new(
            &resources,
            vk::Extent3D {
                width: GDF_RES,
                height: GDF_RES,
                depth: GDF_RES,
            },
            GDF_ALBEDO_FORMAT,
            1,
            usage,
        )?;

        // One-shot: every volume clears to open space (distance +max, occupancy 0, albedo
        // 0) and parks in GENERAL, so a cascade mid-way through its slabbed full refresh
        // samples open air instead of uninitialized memory.
        initialize_volumes(device, &cascades, &occupancy, &albedo)?;
        let mut cascades = cascades;
        let mut occupancy = occupancy;
        let mut albedo = albedo;
        for img in cascades.iter_mut().chain(occupancy.iter_mut()) {
            img.layout = vk::ImageLayout::GENERAL;
        }
        albedo.layout = vk::ImageLayout::GENERAL;

        let sampler = create_linear_repeat_sampler(raw)?;

        let layouts = match build_layouts(raw) {
            Ok(layouts) => layouts,
            Err(err) => {
                // SAFETY: the ash seam. Free the sampler created above before returning.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };

        // One slot per frame in flight: each its own GPU-rebuilt cull list, params UBO, and the
        // two compute sets binding them (sets are pool-owned, so a partial failure frees only the
        // layouts + sampler; the slots' buffers Drop with `frames`).
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            match build_frame(&resources, descriptors, layouts) {
                Ok(frame) => frames.push(frame),
                Err(err) => {
                    // SAFETY: the ash seam. Free the shared layouts + sampler; `frames` drops here,
                    // freeing every slot buffer already built.
                    unsafe {
                        raw.destroy_descriptor_set_layout(layouts.0, None);
                        raw.destroy_descriptor_set_layout(layouts.1, None);
                        raw.destroy_descriptor_set_layout(layouts.2, None);
                        raw.destroy_sampler(sampler, None);
                    }
                    return Err(err);
                }
            }
        }

        let mut gdf = Self {
            resources,
            use_gdf: true,
            ready: false,
            cascades,
            occupancy,
            albedo,
            frames,
            sampler,
            cull_layout: layouts.0,
            composite_layout: layouts.1,
            scatter_layout: layouts.2,
            eye: Vec3::ZERO,
            cur_center: [IVec3::ZERO; GDF_CASCADES as usize],
            prev_center: [IVec3::ZERO; GDF_CASCADES as usize],
            has_history: [false; GDF_CASCADES as usize],
            full_slab: [0; GDF_CASCADES as usize],
            frame: 0,
        };

        gdf.write_static_descriptors();
        // On a failure the `?` early-returns, dropping `gdf` — which frees every owned handle.
        gdf.init_transition_cascades(device)?;
        for frame in 0..gdf.frames.len() {
            gdf.write_params_ubo(frame);
        }
        gdf.ready = true;
        Ok(gdf)
    }

    /// Toggles the GDF; turning it on re-fills every cascade from scratch (arms a full refresh).
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.use_gdf {
            self.has_history = [false; GDF_CASCADES as usize];
        }
        self.use_gdf = enabled;
    }

    /// Whether the GDF is on and its resources are built (the consumer-enable gate).
    pub fn enabled(&self) -> bool {
        self.use_gdf && self.ready
    }

    /// Whether the cull + composite passes run this frame: on + ready + the two PSOs are present.
    /// Pure logic, so the acceptance test can assert it without a device.
    pub fn wants_gdf(&self, pipelines_ready: bool) -> bool {
        self.use_gdf && self.ready && pipelines_ready
    }

    /// Recenters every cascade on the camera eye this frame (snapped to its own voxel grid) and
    /// rewrites the current frame slot's params UBO the consumers read. A no-op when not ready.
    /// Called once per frame from the renderer (with the recording frame's slot index) alongside
    /// `set_ssao_camera` / `set_cluster_camera`.
    pub fn set_camera(&mut self, eye: Vec3, frame: usize) {
        if !self.ready {
            return;
        }
        self.eye = eye;
        for c in 0..GDF_CASCADES {
            self.cur_center[c as usize] = cascade_center_voxel(c, eye);
        }
        self.write_params_ubo(frame);
    }

    /// The cull push for this frame (per-cascade world bounds + the instance count). `count` is the
    /// renderer's active SDF-instance count.
    pub fn cull_push(&self, count: u32) -> GdfCullPush {
        let mut cascade = [Vec4::ZERO; GDF_CASCADES as usize];
        for (c, slot) in cascade.iter_mut().enumerate() {
            let center = self.cur_center[c].as_vec3() * cascade_voxel_size(c as u32);
            *slot = center.extend(cascade_half_extent(c as u32));
        }
        GdfCullPush {
            cascade,
            counts: UVec4::new(count, GDF_MAX_CULLED, GDF_CASCADES, 0),
        }
    }

    /// The dirty regions to composite for cascade `c` this frame. Empty when the cascade needs no
    /// update this frame.
    ///
    /// Between full-refresh cycles a cascade composites only the toroidal scroll slabs
    /// the camera exposed; a static scene with a still camera composites nothing. Every
    /// cascade fully refreshes on a staggered round-robin (one per frame, slab-amortized),
    /// which is also what reconverges the field on occluder motion — the occluder set is
    /// GPU-produced, so no CPU-side AABB diff can dirty the near field ahead of it.
    pub fn dirty_regions(&self, c: u32) -> Vec<GdfRegion> {
        // An in-flight full refresh emits one z-slab per frame — never the whole
        // `GDF_RES`³ volume in one command buffer (a submission that large is the kind
        // the platform GPU watchdog kills). `prepare_frame_regions` arms the cursor;
        // `advance_frame` moves it.
        if self.full_slab[c as usize] < GDF_FULL_SLABS {
            return vec![self.full_slab_region(c)];
        }
        if c > 0 {
            // Far cascades between full-refresh cycles composite only their scroll
            // slabs.
            return cascade_dirty_regions(
                self.prev_center[c as usize],
                self.cur_center[c as usize],
                true,
                false,
                GDF_RES as i32,
            );
        }

        // Near cascade between full-refresh cycles: the toroidal scroll slabs the
        // camera exposed. Occluder motion reconverges through the round-robin full
        // refresh below — the occluder set is GPU-produced, so the CPU has no
        // per-occluder AABBs to diff.
        merge_regions(cascade_dirty_regions(
            self.prev_center[0],
            self.cur_center[0],
            true,
            false,
            GDF_RES as i32,
        ))
    }

    /// Arms each cascade's full-refresh slab cursor for this frame: first fill, a
    /// scroll past a whole window, or the round-robin that keeps every cascade
    /// reconverging on occluder motion. Every cascade joins the round-robin because
    /// the occluder set is GPU-produced — the CPU has no per-occluder AABBs to dirty
    /// the near field with, so the near field reconverges on the same staggered
    /// cadence the far cascades always used. Called once per frame after the centers
    /// update, before the graph builds.
    pub fn prepare_frame_regions(&mut self) {
        for c in 0..GDF_CASCADES as usize {
            if self.full_slab[c] < GDF_FULL_SLABS {
                continue; // a refresh is already in flight
            }
            let scrolled_out = (self.cur_center[c] - self.prev_center[c])
                .abs()
                .max_element()
                >= GDF_RES as i32;
            let round_robin = c as u32 == self.frame % GDF_CASCADES;
            let arm = !self.has_history[c] || scrolled_out || round_robin;
            if arm {
                self.full_slab[c] = 0;
            }
        }
    }

    /// The z-slab of cascade `c`'s window the in-flight full refresh composites this
    /// frame.
    fn full_slab_region(&self, c: u32) -> GdfRegion {
        let half = GDF_RES as i32 / 2;
        let slab = self.full_slab[c as usize].min(GDF_FULL_SLABS - 1);
        let depth = GDF_RES / GDF_FULL_SLABS;
        let mut base = self.cur_center[c as usize] - IVec3::splat(half);
        base.z += (slab * depth) as i32;
        GdfRegion {
            base,
            size: UVec3::new(GDF_RES, GDF_RES, depth),
        }
    }

    /// The composite push for one dirty `region` of cascade `c`.
    pub fn composite_push(&self, c: u32, region: GdfRegion) -> GdfCompositePush {
        GdfCompositePush {
            global_base: region.base.extend(c as i32),
            size: region.size.extend(GDF_MAX_CULLED),
            cascade_info: Vec4::new(
                cascade_voxel_size(c),
                cascade_max_encode(c),
                GDF_RES as f32,
                0.0,
            ),
        }
    }

    /// One cascade volume's handle + view + tracked layout, for the graph import.
    pub fn cascade(&self, c: u32) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        let img = &self.cascades[c as usize];
        (img.handle(), img.view(), img.layout)
    }

    /// Writes back cascade `c`'s resolved layout after the graph executes.
    pub fn set_cascade_layout(&mut self, c: u32, layout: vk::ImageLayout) {
        self.cascades[c as usize].layout = layout;
    }

    /// Cascade `c`'s porous-occupancy volume handle + view + tracked layout, for the graph
    /// import.
    pub fn occupancy_cascade(&self, c: u32) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        let img = &self.occupancy[c as usize];
        (img.handle(), img.view(), img.layout)
    }

    /// Writes back cascade `c`'s occupancy-volume layout after the graph executes.
    pub fn set_occupancy_layout(&mut self, c: u32, layout: vk::ImageLayout) {
        self.occupancy[c as usize].layout = layout;
    }

    /// The lite albedo cache's handle + view + tracked layout, for the graph import + the DDGI
    /// trace's set-2 bind.
    pub fn albedo_cache(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (self.albedo.handle(), self.albedo.view(), self.albedo.layout)
    }

    /// Writes back the albedo cache's resolved layout after the graph executes.
    pub fn set_albedo_layout(&mut self, layout: vk::ImageLayout) {
        self.albedo.layout = layout;
    }

    /// The linear-**repeat** cascade sampler — the DDGI trace reads the albedo cache with it so the
    /// toroidal `wp / worldExtent` UVW wraps across the storage seam.
    pub fn cascade_sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// Frame slot `frame`'s cull-list SSBO handle (for the graph import + barrier tracking).
    pub fn cull_buffer(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].cull_buffer.handle()
    }

    /// The number of `u32` header words to clear at the start of the cull pass (the per-cascade
    /// atomic counters).
    pub fn cull_counter_bytes(&self) -> vk::DeviceSize {
        u64::from(GDF_CULL_HEADER) * size_of::<u32>() as u64
    }

    /// Frame slot `frame`'s cull pass descriptor set (set 1: instances + cull list).
    pub fn cull_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].cull_set
    }

    /// Frame slot `frame`'s composite pass descriptor set (set 1: instances + cull list + cascade
    /// storage images).
    pub fn composite_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].composite_set
    }

    /// The cull compute set layout.
    pub fn cull_layout(&self) -> vk::DescriptorSetLayout {
        self.cull_layout
    }

    /// The composite compute set layout.
    pub fn composite_layout(&self) -> vk::DescriptorSetLayout {
        self.composite_layout
    }

    /// Advances the temporal state after a frame's passes are recorded: commits each cascade's
    /// snapped center as the next frame's toroidal base, marks every cascade as having history, and
    /// bumps the round-robin frame index. Only called when the chain ran this frame.
    pub fn advance_frame(&mut self) {
        self.prev_center = self.cur_center;
        for c in 0..GDF_CASCADES as usize {
            if self.full_slab[c] < GDF_FULL_SLABS {
                self.full_slab[c] += 1;
                if self.full_slab[c] == GDF_FULL_SLABS {
                    self.has_history[c] = true;
                }
            }
        }
        self.frame = self.frame.wrapping_add(1);
    }

    /// Binds the renderer-owned SDF-instance SSBO into every frame slot's compute sets (the cull +
    /// composite read the same per-mesh instance list the cone trace does). One-time wire-up at
    /// construction.
    pub fn bind_scene(
        &self,
        buffer: vk::Buffer,
        slot_bytes: vk::DeviceSize,
        meta: vk::Buffer,
        meta_slot_bytes: vk::DeviceSize,
    ) {
        let raw = self.resources.device();
        for (slot, frame) in self.frames.iter().enumerate() {
            let offset = slot as vk::DeviceSize * slot_bytes;
            let meta_offset = slot as vk::DeviceSize * meta_slot_bytes;
            write_storage_buffer(raw, frame.cull_set, 0, buffer, offset, slot_bytes);
            write_storage_buffer(raw, frame.composite_set, 0, buffer, offset, slot_bytes);
            write_storage_buffer(raw, frame.cull_set, 2, meta, meta_offset, meta_slot_bytes);
            write_storage_buffer(raw, frame.scatter_set, 2, buffer, offset, slot_bytes);
            write_storage_buffer(
                raw,
                frame.scatter_set,
                3,
                meta,
                meta_offset,
                meta_slot_bytes,
            );
        }
    }

    /// Writes frame slot `frame`'s scatter inputs: the reach view's counters (b0) and
    /// visible list (b1), plus the frame's scene address-block slice (b4). Rewritten
    /// whenever the reach view rebuilds, which reallocates both lists.
    pub fn write_scatter_inputs(
        &self,
        frame: usize,
        counters: (vk::Buffer, vk::DeviceSize),
        visible: (vk::Buffer, vk::DeviceSize),
        addresses: (vk::Buffer, u64, u64),
    ) {
        let raw = self.resources.device();
        let set = self.frames[frame].scatter_set;
        write_storage_buffer(raw, set, 0, counters.0, 0, counters.1);
        write_storage_buffer(raw, set, 1, visible.0, 0, visible.1);
        let info = [vk::DescriptorBufferInfo {
            buffer: addresses.0,
            offset: addresses.1,
            range: addresses.2,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(4)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&info);
        // SAFETY: the ash seam. The set + buffer outlive the call.
        unsafe { raw.update_descriptor_sets(&[write], &[]) };
    }

    /// Frame slot `frame`'s scatter set.
    #[must_use]
    pub fn scatter_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].scatter_set
    }

    /// The scatter set layout, for pipeline creation.
    #[must_use]
    pub fn scatter_layout(&self) -> vk::DescriptorSetLayout {
        self.scatter_layout
    }

    /// Writes the cascade samplers (binding 9, an array) + frame slot `frame`'s params UBO
    /// (binding 10) into that frame's light set. The cascade volumes rest in
    /// `SHADER_READ_ONLY_OPTIMAL`, so the descriptor's declared layout matches even when the GDF
    /// never runs (the consumers gate the sample on the `enabled` control flag).
    pub fn write_light_set(&self, light_set: vk::DescriptorSet, frame: usize) {
        let raw = self.resources.device();
        let ro = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        let infos: Vec<vk::DescriptorImageInfo> = self
            .cascades
            .iter()
            .map(|img| {
                vk::DescriptorImageInfo::default()
                    .sampler(self.sampler)
                    .image_view(img.view())
                    .image_layout(ro)
            })
            .collect();
        let write = vk::WriteDescriptorSet::default()
            .dst_set(light_set)
            .dst_binding(9)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&infos);
        // SAFETY: the ash seam. The set + views + sampler outlive the call; the array write fills
        // binding 9's `GDF_CASCADES` elements the light layout declares.
        unsafe { raw.update_descriptor_sets(&[write], &[]) };
        let occupancy_infos: Vec<vk::DescriptorImageInfo> = self
            .occupancy
            .iter()
            .map(|img| {
                vk::DescriptorImageInfo::default()
                    .sampler(self.sampler)
                    .image_view(img.view())
                    .image_layout(ro)
            })
            .collect();
        let occupancy_write = vk::WriteDescriptorSet::default()
            .dst_set(light_set)
            .dst_binding(14)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&occupancy_infos);
        // SAFETY: the ash seam. The set + views + sampler outlive the call.
        unsafe { raw.update_descriptor_sets(&[occupancy_write], &[]) };
        let params_ubo = &self.frames[frame].params_ubo;
        write_uniform_buffer(raw, light_set, 10, params_ubo.handle(), params_ubo.size());
    }

    /// Writes the static (never-reallocated) compute descriptors for every frame slot: that slot's
    /// cull list into both its sets, and the shared cascade storage images into its composite set.
    fn write_static_descriptors(&self) {
        let raw = self.resources.device();
        let views: Vec<vk::DescriptorImageInfo> = self
            .cascades
            .iter()
            .map(|img| {
                vk::DescriptorImageInfo::default()
                    .image_view(img.view())
                    .image_layout(vk::ImageLayout::GENERAL)
            })
            .collect();
        for frame in &self.frames {
            write_storage_buffer(
                raw,
                frame.cull_set,
                1,
                frame.cull_buffer.handle(),
                0,
                frame.cull_buffer.size(),
            );
            write_storage_buffer(
                raw,
                frame.composite_set,
                1,
                frame.cull_buffer.handle(),
                0,
                frame.cull_buffer.size(),
            );
            let write = vk::WriteDescriptorSet::default()
                .dst_set(frame.composite_set)
                .dst_binding(2)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&views);
            // SAFETY: the ash seam. The set + views outlive the call; the array write fills binding
            // 2's `GDF_CASCADES` storage-image elements.
            unsafe { raw.update_descriptor_sets(&[write], &[]) };
            // The lite albedo cache (binding 3, single storage image).
            let albedo_info = [vk::DescriptorImageInfo::default()
                .image_view(self.albedo.view())
                .image_layout(vk::ImageLayout::GENERAL)];
            let albedo_write = vk::WriteDescriptorSet::default()
                .dst_set(frame.composite_set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&albedo_info);
            // SAFETY: the ash seam. The set + view outlive the call.
            unsafe { raw.update_descriptor_sets(&[albedo_write], &[]) };
            // The porous-occupancy volumes (binding 4, storage-image array).
            let occupancy_infos: Vec<vk::DescriptorImageInfo> = self
                .occupancy
                .iter()
                .map(|img| {
                    vk::DescriptorImageInfo::default()
                        .image_view(img.view())
                        .image_layout(vk::ImageLayout::GENERAL)
                })
                .collect();
            let occupancy_write = vk::WriteDescriptorSet::default()
                .dst_set(frame.composite_set)
                .dst_binding(4)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&occupancy_infos);
            // SAFETY: the ash seam. The set + views outlive the call.
            unsafe { raw.update_descriptor_sets(&[occupancy_write], &[]) };
        }
    }

    /// Rewrites frame slot `frame`'s params UBO (per-cascade placement + encode + the control flags)
    /// the consumers read. Called from [`GlobalSdf::new`] (disabled placement, every slot) +
    /// [`GlobalSdf::set_camera`] (the recording frame's slot).
    fn write_params_ubo(&mut self, frame: usize) {
        let mut params = GdfParamsUbo {
            cascades: [GdfCascadeUbo::default(); GDF_CASCADES as usize],
            control: Vec4::new(
                if self.use_gdf { 1.0 } else { 0.0 },
                GDF_CASCADES as f32,
                GDF_NEAR_HANDOFF,
                GDF_BAND_FRACTION,
            ),
        };
        for (c, cascade) in params.cascades.iter_mut().enumerate() {
            let center = self.cur_center[c].as_vec3() * cascade_voxel_size(c as u32);
            *cascade = GdfCascadeUbo {
                center_half: center.extend(cascade_half_extent(c as u32)),
                voxel_encode: Vec4::new(
                    cascade_voxel_size(c as u32),
                    cascade_max_encode(c as u32),
                    cascade_world_extent(c as u32),
                    0.0,
                ),
            };
        }
        if let Some(mapped) = self.frames[frame].params_ubo.mapped_bytes() {
            mapped[..size_of::<GdfParamsUbo>()].copy_from_slice(bytemuck::bytes_of(&params));
        }
    }

    /// One-shot init barrier transitioning every cascade volume from `UNDEFINED` into
    /// `SHADER_READ_ONLY_OPTIMAL` (their resting state for the consumer sample), waited idle.
    fn init_transition_cascades(&mut self, device: &Device) -> Result<()> {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. The pool is freed at the end of this function.
        let pool = checked(
            unsafe { raw.create_command_pool(&pool_info, None) },
            "gdf init pool",
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
                    context: "gdf init cmd",
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
                    context: "gdf init fence",
                    result,
                });
            }
        };

        let result = (|| -> Result<()> {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            let barriers: Vec<vk::ImageMemoryBarrier2> = self
                .cascades
                .iter()
                .map(|img| cascade_init_barrier(img.handle()))
                .chain(std::iter::once(cascade_init_barrier(self.albedo.handle())))
                .collect();
            // SAFETY: the ash seam. The barriers reference images this device created.
            unsafe {
                checked(raw.begin_command_buffer(cmd, &begin), "gdf init begin")?;
                let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
                raw.cmd_pipeline_barrier2(cmd, &dep);
                checked(raw.end_command_buffer(cmd), "gdf init end")?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The queue is touched single-threaded at init.
            unsafe {
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "gdf init submit")?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "gdf init wait",
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
            for img in &mut self.cascades {
                img.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            }
            self.albedo.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }
        result
    }
}

impl Drop for GlobalSdf {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The `Arc<DeviceResources>` keeps the device alive for the call; the
        // run loop idled it before teardown. The sets are pool-owned (freed with the descriptor
        // pool), so only the sampler + the two compute layouts are destroyed here, each exactly
        // once. The cascade images + every frame slot's buffers Drop after this by field order.
        let raw = self.resources.device();
        unsafe {
            raw.destroy_descriptor_set_layout(self.cull_layout, None);
            raw.destroy_descriptor_set_layout(self.composite_layout, None);
            raw.destroy_descriptor_set_layout(self.scatter_layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
    }
}

/// Builds one frame-in-flight slot: its GPU-rebuilt cull-list SSBO, its host-mapped params UBO, and
/// the cull + composite compute sets (pool-owned). The sets reference the slot's own cull list, so a
/// frame in flight never shares a GPU-written buffer with another.
fn build_frame(
    resources: &Arc<DeviceResources>,
    descriptors: &Descriptors,
    layouts: (
        vk::DescriptorSetLayout,
        vk::DescriptorSetLayout,
        vk::DescriptorSetLayout,
    ),
) -> Result<FrameGdf> {
    // The cull list: header (per-cascade counters + pad) + per-cascade index segments. Device-local
    // — GPU-built (atomic append) + GPU-read (composite); the counters are cleared each frame by a
    // `cmd_fill_buffer` in the cull pass body.
    let cull_bytes =
        u64::from(GDF_CULL_HEADER + GDF_CASCADES * GDF_MAX_CULLED) * size_of::<u32>() as u64;
    let cull_buffer = make_device_storage_buffer(resources, cull_bytes)?;
    let params_ubo = make_mapped_uniform_buffer(resources, size_of::<GdfParamsUbo>() as u64)?;
    let cull_set = descriptors.allocate_set(layouts.0)?;
    let composite_set = descriptors.allocate_set(layouts.1)?;
    let scatter_set = descriptors.allocate_set(layouts.2)?;
    Ok(FrameGdf {
        cull_buffer,
        params_ubo,
        cull_set,
        composite_set,
        scatter_set,
    })
}

/// Builds the cull + composite compute set layouts, freeing the cull layout on a composite
/// failure. Returns `(cull, composite)`.
/// Clears every cascade volume to the open-space value (`1.0` = +max-encode distance),
/// the occupancy volumes to `0`, and the albedo cache to `0`, leaving all of them in
/// `GENERAL` for the composite. One submission at construction, fence-waited.
fn initialize_volumes(
    device: &Device,
    cascades: &[Image3D],
    occupancy: &[Image3D],
    albedo: &Image3D,
) -> Result<()> {
    let raw = device.resources().device();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "gdf init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = match checked(
        unsafe { raw.allocate_command_buffers(&alloc) },
        "gdf init cmd",
    ) {
        Ok(buffers) => buffers[0],
        Err(err) => {
            // SAFETY: the ash seam. Free the pool on this failure path.
            unsafe { raw.destroy_command_pool(pool, None) };
            return Err(err);
        }
    };
    let fence = match checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "gdf init fence",
    ) {
        Ok(fence) => fence,
        Err(err) => {
            // SAFETY: the ash seam.
            unsafe { raw.destroy_command_pool(pool, None) };
            return Err(err);
        }
    };
    let result = (|| -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let volumes: Vec<(vk::Image, vk::ClearColorValue)> = cascades
            .iter()
            .map(|img| {
                (
                    img.handle(),
                    vk::ClearColorValue {
                        float32: [1.0, 0.0, 0.0, 0.0],
                    },
                )
            })
            .chain(
                occupancy
                    .iter()
                    .chain(std::iter::once(albedo))
                    .map(|img| (img.handle(), vk::ClearColorValue { float32: [0.0; 4] })),
            )
            .collect();
        // SAFETY: the ash seam. The barriers/clears reference this device's images.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "gdf init begin")?;
            let to_transfer: Vec<vk::ImageMemoryBarrier2> = volumes
                .iter()
                .map(|(image, _)| {
                    vk::ImageMemoryBarrier2::default()
                        .image(*image)
                        .subresource_range(range)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_transfer),
            );
            for (image, clear) in &volumes {
                raw.cmd_clear_color_image(
                    cmd,
                    *image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    clear,
                    &[range],
                );
            }
            let to_general: Vec<vk::ImageMemoryBarrier2> = volumes
                .iter()
                .map(|(image, _)| {
                    vk::ImageMemoryBarrier2::default()
                        .image(*image)
                        .subresource_range(range)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .dst_access_mask(
                            vk::AccessFlags2::SHADER_STORAGE_READ
                                | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                        )
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_general),
            );
            checked(raw.end_command_buffer(cmd), "gdf init end")?;
            let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&buffers)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "gdf init submit")?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "gdf init wait",
            )?;
        }
        Ok(())
    })();
    // SAFETY: the ash seam. The fence was waited (or the submit never happened).
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

fn build_layouts(
    raw: &ash::Device,
) -> Result<(
    vk::DescriptorSetLayout,
    vk::DescriptorSetLayout,
    vk::DescriptorSetLayout,
)> {
    let sb = vk::DescriptorType::STORAGE_BUFFER;
    let si = vk::DescriptorType::STORAGE_IMAGE;
    let ub = vk::DescriptorType::UNIFORM_BUFFER;
    // Cull set: instances (b0, read) + cull list (b1, rw) + the scatter meta words (b2, read).
    let cull = make_compute_layout(raw, &[(sb, 1), (sb, 1), (sb, 1)])?;
    // Composite set: instances (b0) + cull list (b1) + cascade volumes (b2, array of GDF_CASCADES)
    // + the lite albedo cache (b3, single storage image, written for the finest cascade) + the
    // porous-occupancy volumes (b4, array of GDF_CASCADES).
    let composite = match make_compute_layout(
        raw,
        &[
            (sb, 1),
            (sb, 1),
            (si, GDF_CASCADES),
            (si, 1),
            (si, GDF_CASCADES),
        ],
    ) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the cull layout on this partial-failure path.
            unsafe { raw.destroy_descriptor_set_layout(cull, None) };
            return Err(err);
        }
    };
    // Scatter set: the reach view's counters (b0) + visible list (b1) + the occluder
    // output region (b2, rw) + the meta words (b3, rw) + the scene address block (b4).
    let scatter = match make_compute_layout(raw, &[(sb, 1), (sb, 1), (sb, 1), (sb, 1), (ub, 1)]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free both earlier layouts on this partial-failure path.
            unsafe {
                raw.destroy_descriptor_set_layout(cull, None);
                raw.destroy_descriptor_set_layout(composite, None);
            }
            return Err(err);
        }
    };
    Ok((cull, composite, scatter))
}

/// A compute-stage set layout with one binding per `(type, count)` entry, in order.
fn make_compute_layout(
    raw: &ash::Device,
    bindings: &[(vk::DescriptorType, u32)],
) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .enumerate()
        .map(|(i, &(ty, count))| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i as u32)
                .descriptor_type(ty)
                .descriptor_count(count)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop` (or the
    // partial-failure cleanup).
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "gdf compute layout",
    )
}

/// A device-local storage buffer of `size` bytes (the GPU-built cull list).
fn make_device_storage_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
        &alloc_info,
    )
}

/// A host-mapped, persistently-mapped uniform buffer of `size` bytes (the params UBO).
fn make_mapped_uniform_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::Auto,
        flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
            | vk_mem::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    Buffer::new(
        resources,
        size,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        &alloc_info,
    )
}

/// The linear, **repeat** cascade sampler — repeat addressing realizes the toroidal wrap (a
/// trilinear tap across the storage wrap reads spatially-adjacent texels).
fn create_linear_repeat_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::REPEAT)
        .address_mode_v(vk::SamplerAddressMode::REPEAT)
        .address_mode_w(vk::SamplerAddressMode::REPEAT);
    // SAFETY: the ash seam. The sampler is owned and freed in `Drop`.
    checked(unsafe { raw.create_sampler(&info, None) }, "gdf sampler")
}

/// `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` init barrier for a cascade volume (1 mip, 1 layer,
/// color), made sampler-readable by both the fragment + compute consumers.
fn cascade_init_barrier(image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(
            vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COMPUTE_SHADER,
        )
        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
}

/// Writes a storage buffer into `(set, binding)`.
fn write_storage_buffer(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
) {
    let info = [vk::DescriptorBufferInfo {
        buffer,
        offset,
        range: size,
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set + buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

/// Writes a uniform buffer into `(set, binding)`.
fn write_uniform_buffer(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
) {
    let info = [vk::DescriptorBufferInfo {
        buffer,
        offset: 0,
        range: size,
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set + buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

#[cfg(test)]
mod tests {
    /// The GI gate is a reach test, not a visibility test.
    ///
    /// This is the distinction the whole cut turns on: an occluder behind the camera still
    /// shadows what the camera sees, so it must survive, while one beyond the coarsest cascade
    /// cannot influence any march and must not. A frustum test would invert exactly this.
    #[test]
    fn gi_bounds_keep_occluders_behind_the_eye_and_drop_unreachable_ones() {
        let eye = Vec3::new(10.0, 2.0, -30.0);
        let (min, max) = super::gi_occluder_bounds(eye);

        // The eye is inside its own window, and the window reaches at least the coarsest
        // cascade's half extent in every direction.
        assert!(min.cmple(eye).all() && max.cmpge(eye).all());
        let reach = super::cascade_half_extent(super::GDF_CASCADES - 1);
        assert!(
            max.x - eye.x >= reach,
            "the window must reach the coarsest cascade"
        );

        let inside = |p: Vec3| min.cmple(p).all() && max.cmpge(p).all();
        // Directly behind the eye, well within reach: a frustum cull would drop this, and it
        // is precisely the occluder that darkens what is in front.
        assert!(inside(eye + Vec3::new(0.0, 0.0, -8.0)));
        assert!(inside(eye + Vec3::new(0.0, 0.0, 8.0)));
        // Far beyond any cascade: unreachable, so excluding it changes nothing.
        assert!(!inside(eye + Vec3::splat(4000.0)));
    }

    /// The window travels with the eye rather than being anchored at the origin — otherwise a
    /// scene far from the origin would cull everything.
    #[test]
    fn gi_bounds_follow_the_eye() {
        let (near_min, _) = super::gi_occluder_bounds(Vec3::ZERO);
        let (far_min, _) = super::gi_occluder_bounds(Vec3::new(5000.0, 0.0, 0.0));
        assert!(far_min.x > near_min.x + 4000.0);
    }

    use std::sync::Mutex;

    use super::*;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;

    /// The push + UBO structs byte-match the `.slang` layouts the SPIR-V reads — a wrong offset is
    /// a silently corrupted dispatch, so pin each size.
    #[test]
    fn gdf_struct_sizes_match_slang() {
        assert_eq!(size_of::<GdfCullPush>(), 64);
        assert_eq!(size_of::<GdfCompositePush>(), 48);
        assert_eq!(size_of::<GdfCascadeUbo>(), 32);
        assert_eq!(size_of::<GdfParamsUbo>(), 112);
    }

    /// The cascade geometry doubles per cascade (the clipmap exponent): finest extent 32 m, voxel
    /// 0.25 m; each coarser cascade covers twice the world at twice the voxel size.
    #[test]
    fn cascade_geometry_follows_the_clipmap_exponent() {
        assert_eq!(cascade_world_extent(0), 32.0);
        assert_eq!(cascade_world_extent(1), 64.0);
        assert_eq!(cascade_world_extent(2), 128.0);
        assert!((cascade_voxel_size(0) - 32.0 / 128.0).abs() < 1e-6);
        assert!((cascade_voxel_size(2) - 1.0).abs() < 1e-6);
        assert_eq!(cascade_half_extent(0), 16.0);
        assert_eq!(cascade_max_encode(0), 8.0);
    }

    /// `wants_gdf` runs only when on AND ready AND the PSOs are present (the acceptance gate).
    #[test]
    fn wants_gdf_gates_on_on_ready_and_pipelines() {
        // A device-free shadow of the gate (the real `GlobalSdf::new` needs a Vulkan device).
        struct Gate {
            use_gdf: bool,
            ready: bool,
        }
        impl Gate {
            fn wants_gdf(&self, pipelines: bool) -> bool {
                self.use_gdf && self.ready && pipelines
            }
        }
        let mut g = Gate {
            use_gdf: false,
            ready: true,
        };
        assert!(!g.wants_gdf(true));
        g.use_gdf = true;
        assert!(!g.wants_gdf(false));
        assert!(g.wants_gdf(true));
        g.ready = false;
        assert!(!g.wants_gdf(true));
    }

    /// First-fill / scroll-out / round-robin all force a single full-window region; a small camera
    /// delta yields only the scrolled-in axis slabs (the toroidal incremental update) — an
    /// off-by-one here smears the field as the camera moves.
    #[test]
    fn dirty_regions_full_on_first_fill_else_axis_slabs() {
        let res = 128;
        let c = IVec3::new(10, 0, -5);

        // No history -> one full window region covering [c - res/2, c + res/2).
        let full = cascade_dirty_regions(IVec3::ZERO, c, false, false, res);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].size, UVec3::splat(res as u32));
        assert_eq!(full[0].base, c - IVec3::splat(res / 2));

        // Round-robin full refresh -> full window even with history + no motion.
        let rr = cascade_dirty_regions(c, c, true, true, res);
        assert_eq!(rr.len(), 1);
        assert_eq!(rr[0].size, UVec3::splat(res as u32));

        // No motion, has history, not round-robin -> nothing to update.
        assert!(cascade_dirty_regions(c, c, true, false, res).is_empty());

        // +3 along x -> one slab, 3 voxels thick, at the high-x leading edge, full on y/z.
        let prev = c;
        let cur = c + IVec3::new(3, 0, 0);
        let slabs = cascade_dirty_regions(prev, cur, true, false, res);
        assert_eq!(slabs.len(), 1);
        let s = slabs[0];
        assert_eq!(s.size, UVec3::new(3, res as u32, res as u32));
        // The slab's leading edge sits at cur.x + res/2 - 3, the newly-entered high-x cells.
        assert_eq!(s.base.x, cur.x + res / 2 - 3);
        assert_eq!(s.base.y, cur.y - res / 2);
        assert_eq!(s.base.z, cur.z - res / 2);

        // -2 along z -> one slab at the low-z edge.
        let slabs_z = cascade_dirty_regions(c, c + IVec3::new(0, 0, -2), true, false, res);
        assert_eq!(slabs_z.len(), 1);
        assert_eq!(slabs_z[0].size, UVec3::new(res as u32, res as u32, 2));
        assert_eq!(slabs_z[0].base.z, (c.z - 2) - res / 2);

        // Diagonal motion -> one slab per moving axis (their union covers all new cells).
        let diag = cascade_dirty_regions(c, c + IVec3::new(1, 0, 1), true, false, res);
        assert_eq!(diag.len(), 2);

        // A scroll past a full window -> a single full refresh, never a giant slab.
        let jump = cascade_dirty_regions(c, c + IVec3::new(res, 0, 0), true, false, res);
        assert_eq!(jump.len(), 1);
        assert_eq!(jump[0].size, UVec3::splat(res as u32));
    }

    /// Region merge drops empties, and collapses an over-cap set into one covering window.
    #[test]
    fn merge_regions_dedups_and_caps() {
        let unit = GdfRegion {
            base: IVec3::ZERO,
            size: UVec3::splat(4),
        };
        // Under the cap: kept (dedup only removes exact consecutive repeats).
        let kept = merge_regions(vec![unit, unit]);
        assert!(kept.len() <= 2 && !kept.is_empty());
        // Over the cap: collapsed to a single covering region.
        let many: Vec<_> = (0..GDF_MAX_DIRTY_INSTANCES as i32 + 2)
            .map(|i| GdfRegion {
                base: IVec3::new(i * 8, 0, 0),
                size: UVec3::splat(4),
            })
            .collect();
        let merged = merge_regions(many);
        assert_eq!(merged.len(), 1);
    }

    /// The composite's empty-voxel + encode contract (mirrored from `gdf_composite.slang`): an
    /// empty voxel (no brick covers it) stores `+1` normalized (= +max-encode world), never zero,
    /// so the sphere-march never stalls; an occupied voxel stores the clamped signed ratio.
    #[test]
    fn composite_encode_empty_voxel_is_positive_not_zero() {
        let encode = |best_world: f32, max_encode: f32| -> f32 {
            if best_world >= 1e29 {
                1.0
            } else {
                (best_world / max_encode).clamp(-1.0, 1.0)
            }
        };
        let max_encode = cascade_max_encode(0);
        // Empty voxel -> +1 (full open distance), strictly positive.
        assert_eq!(encode(1e30, max_encode), 1.0);
        // A surface 2 m away in an 8 m encode -> 0.25.
        assert!((encode(2.0, max_encode) - 0.25).abs() < 1e-6);
        // Inside a surface (negative) -> negative, clamped at -1.
        assert_eq!(encode(-100.0, max_encode), -1.0);
    }

    /// Building the GDF sub-state (cascade volumes, cull SSBO, params UBO, the two layouts/sets, the
    /// static descriptor writes, and the one-shot cascade init barrier) is validation-clean on a
    /// software device — the resource bring-up half of this phase the toolbox can run. Skips cleanly
    /// when no Vulkan device is obtainable.
    #[test]
    fn gdf_resource_bringup_is_validation_clean() {
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

        let mut gdf = GlobalSdf::new(&device, &descriptors).expect("GlobalSdf::new");
        // Built + ready, on by default; every cascade rests ShaderReadOnly after the init barrier.
        assert!(gdf.ready);
        assert!(gdf.use_gdf);
        for c in 0..GDF_CASCADES {
            assert_eq!(gdf.cascade(c).2, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        }
        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            assert_ne!(gdf.cull_set(frame), vk::DescriptorSet::null());
            assert_ne!(gdf.composite_set(frame), vk::DescriptorSet::null());
        }

        // On by default with no history yet → the first frame is a full refresh; a camera recenter
        // snaps the cascade centers + rewrites the current frame slot's params UBO; the cull push
        // carries the per-cascade bounds.
        assert!(gdf.enabled());
        gdf.set_camera(Vec3::new(5.0, 2.0, -3.0), 0);
        let push = gdf.cull_push(7);
        assert_eq!(push.counts.x, 7);
        assert_eq!(push.counts.y, GDF_MAX_CULLED);
        // The finest cascade's first frame (no history) -> one full region.
        let regions = gdf.dirty_regions(0);
        assert!(!regions.is_empty());

        drop(gdf);
        // SAFETY: the device must idle before its sub-state Drops.
        device.wait_idle().expect("wait_idle");
        assert_eq!(
            validation_issue_count(),
            before,
            "the GDF bring-up + init transition raised no validation issues"
        );
    }
}
