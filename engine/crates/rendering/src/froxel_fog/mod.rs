//! The froxel-fog grid data model: the frustum-aligned volume's constants, quality tiers, its
//! `FogGridParams` UBO, and the CPU depth mapping that ties a fog froxel to the clustered-light
//! cull's exponential-Z partition.
//!
//! The froxel grid is a strict refinement of the [`crate::lighting`] cull grid: a finer XY tiling
//! ([`FROXEL_GRID_X`]×[`FROXEL_GRID_Y`]) and its own Z count, but the *identical* exponential
//! view-space Z distribution. So a fog froxel lands inside exactly one cull cluster and can read
//! that cluster's already-built light list ([`froxel_to_cluster`]).

mod noise;
mod setup;

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, UVec4, Vec3, Vec4};

use crate::global_sdf::GDF_ALBEDO_FORMAT;
use crate::lighting::{CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z};
use crate::resources::{Buffer, DeviceResources, Image3D};
use crate::{Device, checked};

use noise::*;
pub(crate) use setup::create_compute_layout;
use setup::*;

/// The upper bound on local [`FogVolumeUpload`]s injected per frame — a small global list looped
/// per froxel (the same bounded-global model as the reflection probes), not a per-volume cull. The
/// injection loop's per-volume bounds test is cheaper than a second cull pass at this count.
pub const MAX_FOG_VOLUMES: u32 = 16;

/// The edge of the cubic tiling noise volume baked once at init (`NOISE_DIM³`, `R8_UNORM`). Sampled
/// with a linear-repeat sampler so a volume of any size tiles the erosion pattern seamlessly.
const NOISE_DIM: u32 = 64;

/// The `u32` shape tag packed into [`FogVolumeGpu`] — kept in sync with the shader's `FOG_SHAPE_*`.
pub const FOG_SHAPE_BOX: u32 = 0;

/// The sphere shape tag (see [`FOG_SHAPE_BOX`]).
pub const FOG_SHAPE_SPHERE: u32 = 1;

/// Per-frame snapshot of one `FogVolume`, its transform baked to world space, handed to the froxel
/// injection pass so the renderer need not depend on the scene. `world_from_local` frames both the
/// oriented-box bounds test and the noise sampling; `center` is the world-space origin.
#[derive(Debug, Clone, Copy)]
pub struct FogVolumeUpload {
    /// Local → world (its inverse frames the injection bounds test + noise).
    pub world_from_local: Mat4,
    /// World-space volume origin.
    pub center: Vec3,
    /// `FOG_SHAPE_BOX` / `FOG_SHAPE_SPHERE`.
    pub shape: u32,
    /// Box half-extents (local space).
    pub extents: Vec3,
    /// Sphere radius.
    pub radius: f32,
    /// Soft-edge width (world units).
    pub edge_falloff: f32,
    /// `sigma_t` at the full interior.
    pub density: f32,
    /// Single-scatter albedo.
    pub albedo: Vec3,
    /// Added in-scatter radiance.
    pub emissive: Vec3,
    /// Henyey-Greenstein anisotropy.
    pub phase_g: f32,
    /// Per-volume exponential height slab (0 = uniform).
    pub height_falloff: f32,
    /// World→noise frequency.
    pub noise_scale: f32,
    /// Erosion strength (0 = off).
    pub noise_intensity: f32,
    /// Detail-octave weight.
    pub noise_detail: f32,
    /// Advection direction.
    pub wind: Vec3,
    /// Advection speed.
    pub speed: f32,
}

/// The std430 record the inject shader reads per volume (160 B, every field vec4-aligned). `extents`
/// packs the shape tag in `.w`; the matrix is `local_from_world` so the shader transforms a froxel's
/// world position straight into the volume's local frame.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FogVolumeGpu {
    /// World → local (bounds test + noise frame).
    pub local_from_world: Mat4,
    /// `xyz` box half-extents, `w` shape tag (`FOG_SHAPE_*`, as a float).
    pub extents_shape: Vec4,
    /// `x` radius, `y` edge falloff, `z` density, `w` height falloff.
    pub radius_edge_density_hf: Vec4,
    /// `xyz` single-scatter albedo, `w` HG anisotropy.
    pub albedo_phase: Vec4,
    /// `xyz` emissive radiance, `w` advection speed.
    pub emissive_speed: Vec4,
    /// `x` noise scale, `y` noise intensity, `z` noise detail, `w` pad.
    pub noise: Vec4,
    /// `xyz` wind direction, `w` pad.
    pub wind: Vec4,
}

const _: () = assert!(
    size_of::<FogVolumeGpu>() == 160,
    "FogVolumeGpu must match the std430 shader layout (mat4 + 6 vec4 == 160 bytes)"
);

impl FogVolumeGpu {
    /// Bakes a [`FogVolumeUpload`] into the std430 record: inverts the world transform for the
    /// local-frame bounds test and packs the optical fields into the vec4 slots.
    pub fn from_upload(u: &FogVolumeUpload) -> Self {
        Self {
            local_from_world: u.world_from_local.inverse(),
            extents_shape: Vec4::new(u.extents.x, u.extents.y, u.extents.z, u.shape as f32),
            radius_edge_density_hf: Vec4::new(
                u.radius,
                u.edge_falloff,
                u.density,
                u.height_falloff,
            ),
            albedo_phase: Vec4::new(u.albedo.x, u.albedo.y, u.albedo.z, u.phase_g),
            emissive_speed: Vec4::new(u.emissive.x, u.emissive.y, u.emissive.z, u.speed),
            noise: Vec4::new(u.noise_scale, u.noise_intensity, u.noise_detail, 0.0),
            wind: Vec4::new(u.wind.x, u.wind.y, u.wind.z, 0.0),
        }
    }
}

/// Froxel grid X tiles at the high/medium tiers — finer than the cull's [`CLUSTER_GRID_X`].
pub const FROXEL_GRID_X: u32 = 160;

/// Froxel grid Y tiles at the high/medium tiers — finer than the cull's [`CLUSTER_GRID_Y`].
pub const FROXEL_GRID_Y: u32 = 90;

/// Froxel grid Z slices at the high tier — the full-resolution exponential partition.
pub const FROXEL_GRID_Z: u32 = 128;

/// The froxel volume's format — `rgba16f`, the same convention the Global-SDF albedo cache uses
/// ([`GDF_ALBEDO_FORMAT`]); rgb carries in-scattered radiance, a carries extinction/transmittance.
pub const FROXEL_FORMAT: vk::Format = GDF_ALBEDO_FORMAT;

/// The near bound of the froxel grid's exponential-Z distribution (metres) — the fog near plane the
/// composite's W mapping inverts. Matches [`FogGridParams::default`]'s `z_planes.x`.
pub const FROXEL_NEAR: f32 = 0.1;

/// The far bound of the froxel grid's coverage (metres). Beyond it the analytic height fog carries
/// the far field, so the grid need not extend to the camera far plane. Stored in
/// [`FogGridParams::z_planes`]`.z` — a *coverage clamp*, distinct from the near/far the Z
/// distribution is parameterized by (which must match the cull's for the grid to align).
pub const FROXEL_FAR: f32 = 128.0;

/// The aerial-perspective grid edge — Hillaire-2020's `32³` atmosphere volume. Fixed at the
/// production size (not tied to the fog quality tier): `32×32×32` is not the bottleneck, and the
/// smooth LUT march needs no finer sampling.
pub const AP_GRID: u32 = 32;

/// The aerial-perspective far plane (metres) — the atmosphere horizon (km-scale), reaching far beyond
/// the fog's [`FROXEL_FAR`] so distant geometry picks up planetary Rayleigh/Mie scattering. The near
/// plane reuses [`FROXEL_NEAR`], so the AP volume shares the fog's exponential-Z *mapping function*
/// with a different far bound — one mapping, two instantiations.
pub const AP_FAR_M: f32 = 32_000.0;

/// View-space Z at the near edge of aerial-perspective slice `k` of [`AP_GRID`] — the same exponential
/// curve as the fog grid ([`froxel_slice_view_z`]) with the AP far plane, so there is one mapping code
/// path, not a second hand-written derivation.
pub fn ap_slice_view_z(near: f32, k: u32) -> f32 {
    froxel_slice_view_z(near, AP_FAR_M, k, AP_GRID)
}

/// The render grid-quality tier: how many froxels the volume carries. The exponential Z
/// distribution is identical across tiers — only the resolution changes, so the froxel→cluster
/// mapping is tier-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FroxelQuality {
    /// `128×72×64` — the coarse tier.
    Low,
    /// `160×90×64` — half the Z slices of high (the default, matching `FogSettings`).
    #[default]
    Medium,
    /// `160×90×128` — the full-resolution grid.
    High,
}

impl FroxelQuality {
    /// The `(x, y, z)` froxel counts for this tier. High keeps the full `160×90×128`; medium halves
    /// the Z slices; low additionally drops XY to `128×72`.
    pub fn grid(self) -> (u32, u32, u32) {
        match self {
            FroxelQuality::Low => (128, 72, 64),
            FroxelQuality::Medium => (FROXEL_GRID_X, FROXEL_GRID_Y, 64),
            FroxelQuality::High => (FROXEL_GRID_X, FROXEL_GRID_Y, FROXEL_GRID_Z),
        }
    }
}

/// The froxel-grid params UBO, std140-compatible (three `mat4` + six 16-byte vector blocks).
/// Mirrors [`crate::lighting::ClusterParams`]'s role for the cull: the injection
/// reconstructs each froxel center's world position from `inverse_projection`/`inverse_view`, then
/// reads the containing cull cluster's light list via [`froxel_to_cluster`].
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FogGridParams {
    /// Clip → view (froxel-center reconstruction).
    pub inverse_projection: Mat4,
    /// View → world (froxel-center reconstruction).
    pub inverse_view: Mat4,
    /// Last frame's un-jittered world → clip, reprojecting a froxel-center world position into the
    /// previous-frame froxel `uvw` for the temporal history sample.
    pub prev_view_proj: Mat4,
    /// `xyz` froxel grid dims, `w` punctual light count (reused from the cull).
    pub grid_size: UVec4,
    /// `xy` offscreen pixel dims, `zw` pad.
    pub screen_size: Vec4,
    /// `x` near plane, `y` far plane (both shared with the cull so the Z curves align), `z`
    /// [`FROXEL_FAR`] coverage clamp, `w` pad.
    pub z_planes: Vec4,
    /// Temporal integration controls: `x` = history blend (fresh-sample weight), `y` = history valid
    /// (`0` = take the fresh sample whole: first frame / camera cut / grid resize), `z` = neighbourhood
    /// clamp enable (`0`/`1`), `w` = per-light in-scatter clamp (`0` = off).
    pub temporal: Vec4,
    /// Sub-froxel jitter: `xy` = this frame's NDC jitter (the same offset TAA advances), `z` = the
    /// Halton jitter phase index (drives the per-frame Z-slice supersample), `w` = scene time.
    pub jitter: Vec4,
}

const _: () = assert!(
    size_of::<FogGridParams>() == 272,
    "FogGridParams must match the std140 shader layout (3 mat4 + 6 vectors == 288 bytes)"
);

impl Default for FogGridParams {
    fn default() -> Self {
        Self {
            inverse_projection: Mat4::IDENTITY,
            inverse_view: Mat4::IDENTITY,
            prev_view_proj: Mat4::IDENTITY,
            grid_size: UVec4::new(FROXEL_GRID_X, FROXEL_GRID_Y, FROXEL_GRID_Z, 0),
            screen_size: Vec4::ZERO,
            z_planes: Vec4::new(0.1, 0.0, FROXEL_FAR, 0.0),
            temporal: Vec4::new(0.05, 0.0, 0.0, 0.0),
            jitter: Vec4::ZERO,
        }
    }
}

/// View-space Z at the near edge of froxel slice `k` of `nz` — the same exponential curve the cull
/// uses (`lighting::cluster_aabb`): `-near · (far/near)^(k / nz)`. At `k == 0` this is `-near`; at
/// `k == nz` it is `-far`. The froxel grid uses this at the finer [`FROXEL_GRID_Z`] slice count over
/// the identical `near`/`far`, which is why a froxel maps cleanly into a cull slice.
pub fn froxel_slice_view_z(near: f32, far: f32, k: u32, nz: u32) -> f32 {
    -near * (far / near).powf(k as f32 / nz as f32)
}

/// The cull cluster (`CLUSTER_GRID_X×Y×Z`) containing fog froxel `(fx, fy, fz)`: reconstruct the
/// froxel-center pixel + view-space Z and index the cull grid exactly as `clusterIndexFor`
/// (`lighting.slang`) does, so the injection reads the coarse cluster's light list per froxel.
/// Requires `params.z_planes` near/far to equal the cull's, since they parameterize one shared
/// exponential distribution; both derive from the same camera, so this holds.
pub fn froxel_to_cluster(params: &FogGridParams, fx: u32, fy: u32, fz: u32) -> u32 {
    let near = params.z_planes.x;
    let far = params.z_planes.y;

    // Froxel-center view-space Z: the exponential curve at the slice center (fz + 0.5).
    let frac = (fz as f32 + 0.5) / params.grid_size.z as f32;
    let view_z = -near * (far / near).powf(frac);

    // Froxel-center pixel (linear screen tiling over the froxel grid).
    let px = (fx as f32 + 0.5) * params.screen_size.x / params.grid_size.x as f32;
    let py = (fy as f32 + 0.5) * params.screen_size.y / params.grid_size.y as f32;

    // Index the *cull* grid at that pixel + Z — the CPU mirror of `clusterIndexFor`.
    let depth = (-view_z).max(near);
    let z_slice = ((depth / near).ln() / (far / near).ln() * CLUSTER_GRID_Z as f32) as u32;
    let z_slice = z_slice.min(CLUSTER_GRID_Z - 1);

    let tile_w = params.screen_size.x / CLUSTER_GRID_X as f32;
    let tile_h = params.screen_size.y / CLUSTER_GRID_Y as f32;
    let tile_x = ((px / tile_w) as u32).min(CLUSTER_GRID_X - 1);
    let tile_y = ((py / tile_h) as u32).min(CLUSTER_GRID_Y - 1);

    tile_x + tile_y * CLUSTER_GRID_X + z_slice * CLUSTER_GRID_X * CLUSTER_GRID_Y
}

/// The froxel volumetric-fog resource: the two fixed-size `rgba16f` volumes the inject/integrate
/// passes fill, the host-mapped [`FogGridParams`] UBO, a linear sampler, and the two compute
/// descriptor sets (write into the scatter volume; read scatter + write the integration volume). The
/// grid is a fixed [`FROXEL_GRID_X`]×[`FROXEL_GRID_Y`]×[`FROXEL_GRID_Z`] frustum lattice independent of
/// the viewport, so the volumes never resize and the descriptor sets are written once.
///
/// The scatter volume is written and read in `GENERAL` (the GDF-albedo storage convention); the
/// integration volume is written in `GENERAL` by the integrate pass and sampled in
/// `SHADER_READ_ONLY_OPTIMAL` by the composite (its resting layout, so the composite's static
/// reference is always valid even in analytic mode).
pub struct FroxelFog {
    resources: Arc<DeviceResources>,
    raw: ash::Device,
    /// The linear `(L_scat, sigma_t)` ping-pong pair: index `write` is this frame's inject target,
    /// `write ^ 1` is last frame's history the reprojection reads. Persistent so history survives
    /// the frame boundary (the same cross-frame ownership the Global-SDF cascades use).
    scatter: [Image3D; 2],
    /// This frame's inject-target index into `scatter`; toggles each frame via [`Self::advance_frame`].
    write: usize,
    /// `false` on the first frame after a reset (new resource / quality switch): the history volume
    /// carries no reprojectable content yet, so the inject takes the fresh sample whole.
    history_ready: bool,
    /// The current grid-quality tier (drives the volume dims + dispatch counts).
    quality: FroxelQuality,
    /// The active `(x, y, z)` froxel counts (`quality.grid()`).
    dims: (u32, u32, u32),
    integration: Image3D,
    grid_params: Buffer,
    /// The host-mapped `FogVolumeGpu[MAX_FOG_VOLUMES]` SSBO the inject pass loops (binding 3 of both
    /// inject sets). Written each frame by [`Self::update_fog_volumes`], like the grid UBO.
    fog_volumes: Buffer,
    /// The tiling `R8_UNORM` erosion-noise volume (binding 4), baked once at init.
    noise: Image3D,
    /// The linear-repeat sampler the inject pass reads `noise` through (distinct from the clamp
    /// `sampler` the history/composite use).
    noise_sampler: vk::Sampler,
    sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    inject_volume_layout: vk::DescriptorSetLayout,
    integrate_layout: vk::DescriptorSetLayout,
    /// Two inject sets, one per ping-pong parity: `inject_sets[p]` writes `scatter[p]` (binding 0)
    /// and samples `scatter[p ^ 1]` as history (binding 2). This frame's set is `inject_sets[write]`.
    inject_sets: [vk::DescriptorSet; 2],
    /// Two integrate sets, one per parity: `integrate_sets[p]` reads `scatter[p]` (this frame's fresh
    /// inject output) and writes the integration volume.
    integrate_sets: [vk::DescriptorSet; 2],
}

impl FroxelFog {
    /// Allocates the scatter + integration volumes, the grid UBO, the linear sampler, the two
    /// compute set layouts + sets (written once), and init-transitions the volumes into their
    /// resting layouts.
    pub fn new(device: &Device) -> crate::Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = device.raw().clone();
        let quality = FroxelQuality::default();
        let dims = quality.grid();
        let (mut scatter, mut integration) = alloc_volumes(&resources, dims)?;

        let grid_params = {
            let alloc = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            };
            Buffer::new(
                &resources,
                size_of::<FogGridParams>() as vk::DeviceSize,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &alloc,
            )?
        };
        // Seed the UBO with the default grid so the descriptor is valid before the first frame push.
        {
            let params = FogGridParams::default();
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized for one FogGridParams.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&params).as_ptr(),
                    grid_params.mapped_ptr(),
                    size_of::<FogGridParams>(),
                );
            }
        }

        let sampler = {
            let info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
            // SAFETY: the ash seam. Freed in `Drop`.
            checked(
                unsafe { raw.create_sampler(&info, None) },
                "froxel fog sampler",
            )?
        };

        let noise_sampler = {
            let info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT);
            // SAFETY: the ash seam. Freed in `Drop`.
            checked(
                unsafe { raw.create_sampler(&info, None) },
                "froxel fog noise sampler",
            )?
        };

        // The per-frame local-volume SSBO (host-mapped, written like the grid UBO each frame).
        let fog_volumes = {
            let alloc = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            };
            Buffer::new(
                &resources,
                (size_of::<FogVolumeGpu>() * MAX_FOG_VOLUMES as usize) as vk::DeviceSize,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                &alloc,
            )?
        };

        let noise = bake_noise_volume(device, &resources)?;

        let inject_volume_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::UNIFORM_BUFFER),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::STORAGE_BUFFER),
                (4, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
            ],
        )?;
        let integrate_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::STORAGE_IMAGE),
                (2, vk::DescriptorType::UNIFORM_BUFFER),
            ],
        )?;

        let pool = {
            let sizes = [
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(6),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::UNIFORM_BUFFER)
                    .descriptor_count(4),
                // Two inject sets × (history + noise) = 4 combined-image-samplers.
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(4),
                // The fog-volume SSBO bound into each of the two inject sets.
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(2),
            ];
            let info = vk::DescriptorPoolCreateInfo::default()
                .max_sets(4)
                .pool_sizes(&sizes);
            // SAFETY: the ash seam. Freed in `Drop`.
            checked(
                unsafe { raw.create_descriptor_pool(&info, None) },
                "froxel fog pool",
            )?
        };

        // Four sets: two inject parities, then two integrate parities.
        let layouts = [
            inject_volume_layout,
            inject_volume_layout,
            integrate_layout,
            integrate_layout,
        ];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. Four sets from the pool above.
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&alloc) },
            "froxel fog sets",
        )?;
        let inject_sets = [sets[0], sets[1]];
        let integrate_sets = [sets[2], sets[3]];

        write_fog_sets(
            &raw,
            inject_sets,
            integrate_sets,
            &FogSetResources {
                sampler,
                noise_sampler,
                scatter: &scatter,
                integration: &integration,
                grid_params: &grid_params,
                fog_volumes: &fog_volumes,
                noise: &noise,
            },
        );

        init_transition_volumes(
            device,
            [scatter[0].handle(), scatter[1].handle()],
            integration.handle(),
        )?;
        scatter[0].layout = vk::ImageLayout::GENERAL;
        scatter[1].layout = vk::ImageLayout::GENERAL;
        integration.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        Ok(Self {
            resources,
            raw,
            scatter,
            write: 0,
            history_ready: false,
            quality,
            dims,
            integration,
            grid_params,
            fog_volumes,
            noise,
            noise_sampler,
            sampler,
            pool,
            inject_volume_layout,
            integrate_layout,
            inject_sets,
            integrate_sets,
        })
    }

    /// Switches the grid-quality tier: reallocates the ping-pong scatter pair + the integration
    /// volume to the new dims, rewrites the four descriptor sets, and resets the history (the
    /// reprojection cannot read a differently-shaped previous volume). No-op — returning `false` —
    /// when `quality` is already active. Returns `true` when the volumes were reallocated, so the
    /// caller can rebind the composite's integration sample. Waits the GPU idle before freeing.
    pub fn set_quality(&mut self, device: &Device, quality: FroxelQuality) -> crate::Result<bool> {
        if quality == self.quality {
            return Ok(false);
        }
        device.wait_idle()?;
        let dims = quality.grid();
        let (mut scatter, mut integration) = alloc_volumes(&self.resources, dims)?;
        write_fog_sets(
            &self.raw,
            self.inject_sets,
            self.integrate_sets,
            &FogSetResources {
                sampler: self.sampler,
                noise_sampler: self.noise_sampler,
                scatter: &scatter,
                integration: &integration,
                grid_params: &self.grid_params,
                fog_volumes: &self.fog_volumes,
                noise: &self.noise,
            },
        );
        // The scatter volumes rest in GENERAL (the storage read/write state); the integration volume
        // rests in ShaderReadOnly so the *early* forward mesh pass that samples it (light set binding
        // 11) sees a valid layout before this frame's integrate pass rewrites it.
        init_transition_volumes(
            device,
            [scatter[0].handle(), scatter[1].handle()],
            integration.handle(),
        )?;
        scatter[0].layout = vk::ImageLayout::GENERAL;
        scatter[1].layout = vk::ImageLayout::GENERAL;
        integration.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        self.scatter = scatter;
        self.integration = integration;
        self.quality = quality;
        self.dims = dims;
        self.write = 0;
        self.history_ready = false;
        Ok(true)
    }

    /// Toggles the ping-pong write index for next frame and marks the just-written volume as valid
    /// history. Called once per frame after the inject/integrate passes are recorded.
    pub fn advance_frame(&mut self) {
        self.write ^= 1;
        self.history_ready = true;
    }

    /// Whether the history volume this frame carries reprojectable content (`false` immediately after
    /// a reset). The renderer ANDs this with the camera's `prev_view_proj_valid` for `historyValid`.
    pub fn history_ready(&self) -> bool {
        self.history_ready
    }

    /// The active grid-quality tier.
    pub fn quality(&self) -> FroxelQuality {
        self.quality
    }

    /// Uploads this frame's local fog volumes into the host-mapped SSBO (read by the inject loop),
    /// capped at [`MAX_FOG_VOLUMES`]. Returns the written count for the inject push's `volumeCount`.
    pub fn update_fog_volumes(&self, volumes: &[FogVolumeGpu]) -> u32 {
        let count = volumes.len().min(MAX_FOG_VOLUMES as usize);
        if count > 0 {
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized for MAX_FOG_VOLUMES records; the
            // copy writes `count <= MAX_FOG_VOLUMES` of them from the head.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::cast_slice::<_, u8>(&volumes[..count]).as_ptr(),
                    self.fog_volumes.mapped_ptr(),
                    count * size_of::<FogVolumeGpu>(),
                );
            }
        }
        count as u32
    }

    /// Uploads this frame's [`FogGridParams`] into the host-mapped UBO (read by every fog pass).
    pub fn update_grid(&self, params: &FogGridParams) {
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized for one FogGridParams.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(params).as_ptr(),
                self.grid_params.mapped_ptr(),
                size_of::<FogGridParams>(),
            );
        }
    }

    /// This frame's inject-target scatter volume `(image, view, layout)` (`scatter[write]`) for the
    /// render-graph import — the pass writes it as a storage image.
    pub fn scatter_write_import(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        let s = &self.scatter[self.write];
        (s.handle(), s.view(), s.layout)
    }

    /// Last frame's scatter volume `(image, view, layout)` (`scatter[write ^ 1]`) for the render-graph
    /// import — the inject pass samples it as the temporal history.
    pub fn scatter_history_import(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        let s = &self.scatter[self.write ^ 1];
        (s.handle(), s.view(), s.layout)
    }

    /// The integration volume's `(image, view, layout)` for the render-graph import.
    pub fn integration_import(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (
            self.integration.handle(),
            self.integration.view(),
            self.integration.layout,
        )
    }

    /// Writes back this frame's inject-target scatter volume's resolved layout after the graph
    /// executes (`scatter[write]`, left `GENERAL`).
    pub fn set_scatter_write_layout(&mut self, layout: vk::ImageLayout) {
        self.scatter[self.write].layout = layout;
    }

    /// Writes back the history scatter volume's resolved layout after the graph executes
    /// (`scatter[write ^ 1]`, left `SHADER_READ_ONLY_OPTIMAL` by the sampled read).
    pub fn set_scatter_history_layout(&mut self, layout: vk::ImageLayout) {
        self.scatter[self.write ^ 1].layout = layout;
    }

    /// Writes back the integration volume's resolved layout after the graph executes.
    pub fn set_integration_layout(&mut self, layout: vk::ImageLayout) {
        self.integration.layout = layout;
    }

    /// The integration volume's view + sampler, bound into the per-view fog composite set (binding 4)
    /// with a `SHADER_READ_ONLY_OPTIMAL` layout for the trilinear volumetric sample.
    pub fn integration_view(&self) -> vk::ImageView {
        self.integration.view()
    }

    /// The linear-clamp sampler the composite reads the integration volume through.
    pub fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// This frame's inject fog-volume set (set 1): writes `scatter[write]`, samples `scatter[write ^ 1]`
    /// as history, reads the grid UBO.
    pub fn inject_set(&self) -> vk::DescriptorSet {
        self.inject_sets[self.write]
    }

    /// This frame's integrate set: reads the fresh `scatter[write]`, writes the integration volume.
    pub fn integrate_set(&self) -> vk::DescriptorSet {
        self.integrate_sets[self.write]
    }

    /// The inject pass's fog-volume set layout (set 1 of the inject PSO).
    pub fn inject_volume_layout(&self) -> vk::DescriptorSetLayout {
        self.inject_volume_layout
    }

    /// The integrate pass's set layout.
    pub fn integrate_layout(&self) -> vk::DescriptorSetLayout {
        self.integrate_layout
    }

    /// The active froxel grid dispatch dimensions (the selected quality tier's dims).
    pub fn grid(&self) -> (u32, u32, u32) {
        self.dims
    }
}

impl Drop for FroxelFog {
    fn drop(&mut self) {
        let raw = &self.raw;
        // SAFETY: the ash seam. The renderer waits the GPU idle before dropping the fog resource,
        // so nothing below is in flight. The pool frees its four sets; the volumes/buffer drop after.
        unsafe {
            raw.destroy_descriptor_pool(self.pool, None);
            raw.destroy_descriptor_set_layout(self.inject_volume_layout, None);
            raw.destroy_descriptor_set_layout(self.integrate_layout, None);
            raw.destroy_sampler(self.sampler, None);
            raw.destroy_sampler(self.noise_sampler, None);
        }
        let _ = &self.resources;
    }
}

/// The aerial-perspective fill pass's uniform (std140), matching `aerial_perspective.slang`'s
/// `ApParams`. The froxel-center reconstruction matrices + the baked atmosphere physical params +
/// the AP exponential-Z planes + the intensity multiplier.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AerialParamsUbo {
    /// Clip → view (froxel-center reconstruction).
    pub inverse_projection: Mat4,
    /// View → world (froxel-center reconstruction).
    pub inverse_view: Mat4,
    /// `xyz` direction to the sun, `w` sun intensity.
    pub sun_dir: Vec4,
    /// `xyz` Rayleigh scattering, `w` Rayleigh scale height (km).
    pub rayleigh: Vec4,
    /// `xyz` ozone absorption, `w` Mie scattering.
    pub ozone: Vec4,
    /// `x` planet radius (km), `y` atmosphere height (km), `z` Mie scale height (km), `w` Mie anisotropy.
    pub params0: Vec4,
    /// `x` sun-disk angular radius, `y` sun-disk intensity, `z` camera altitude (km), `w` AP intensity.
    pub params1: Vec4,
    /// `x` AP near (m), `y` AP far (m), `z` grid edge ([`AP_GRID`]), `w` metres→km scale (`1e-3`).
    pub ap_planes: Vec4,
}

const _: () = assert!(
    size_of::<AerialParamsUbo>() == 224,
    "AerialParamsUbo must match the std140 shader layout (2 mat4 + 6 vec4 == 224 bytes)"
);

/// The aerial-perspective resource: the fixed `32³` `rgba16f` volume the atmosphere-LUT march fills,
/// its host-mapped [`AerialParamsUbo`], a linear-clamp sampler, and the fill pass's descriptor set.
///
/// Hillaire-2020's AP volume is filled per frame by one compute dispatch that ray-marches the same
/// `transmittance`/`multiscatter` LUTs the sky-view LUT bakes, bounded at each froxel center's
/// distance. The [`crate::height_fog`] composite samples it as an *independent* multiplied medium on
/// the shared transmittance ledger, so near fog and far planetary scattering compose with no double
/// darkening. The volume is a persistent [`Image3D`] (fixed-size, viewport-independent — the same
/// design choice the fog integration volume makes), so its descriptor set is written once and the
/// LUT bindings persist across bakes (the LUT images are reused).
///
/// The volume rests in `SHADER_READ_ONLY_OPTIMAL` (the composite's sample layout, so binding 5 of the
/// fog set is always valid even when the AP fill does not run); the fill pass transitions it to
/// `GENERAL` for the storage write and a barrier pass rests it back.
pub struct AerialPerspective {
    resources: Arc<DeviceResources>,
    raw: ash::Device,
    volume: Image3D,
    params: Buffer,
    sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
}

impl AerialPerspective {
    /// Allocates the `32³` volume + the params UBO + the linear-clamp sampler + the fill set (the
    /// storage volume + params bound now; the LUTs bound later via [`Self::bind_luts`] once the IBL
    /// exists), and rests the volume in `SHADER_READ_ONLY_OPTIMAL`.
    pub fn new(device: &Device) -> crate::Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = device.raw().clone();

        let extent = vk::Extent3D {
            width: AP_GRID,
            height: AP_GRID,
            depth: AP_GRID,
        };
        let mut volume = Image3D::new(
            &resources,
            extent,
            FROXEL_FORMAT,
            1,
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
        )?;

        let params = {
            let alloc = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            };
            Buffer::new(
                &resources,
                size_of::<AerialParamsUbo>() as vk::DeviceSize,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &alloc,
            )?
        };

        let sampler = {
            let info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
            // SAFETY: the ash seam. Freed in `Drop`.
            checked(
                unsafe { raw.create_sampler(&info, None) },
                "aerial perspective sampler",
            )?
        };

        let layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (2, vk::DescriptorType::STORAGE_IMAGE),
                (3, vk::DescriptorType::UNIFORM_BUFFER),
            ],
        )?;

        let pool = {
            let sizes = [
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(2),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::UNIFORM_BUFFER)
                    .descriptor_count(1),
            ];
            let info = vk::DescriptorPoolCreateInfo::default()
                .max_sets(1)
                .pool_sizes(&sizes);
            // SAFETY: the ash seam. Freed in `Drop`.
            checked(
                unsafe { raw.create_descriptor_pool(&info, None) },
                "aerial perspective pool",
            )?
        };

        let layouts = [layout];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. One set from the pool above.
        let set = checked(
            unsafe { raw.allocate_descriptor_sets(&alloc) },
            "aerial perspective set",
        )?[0];

        // Bind the storage volume (binding 2, GENERAL) + params UBO (binding 3) now; the LUTs (0/1)
        // are bound once the IBL exists via `bind_luts`.
        let vol_info = [vk::DescriptorImageInfo::default()
            .image_view(volume.view())
            .image_layout(vk::ImageLayout::GENERAL)];
        let params_info = [vk::DescriptorBufferInfo::default()
            .buffer(params.handle())
            .range(params.size())];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&vol_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&params_info),
        ];
        // SAFETY: the ash seam. All infos outlive the call.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        init_transition_ap(device, volume.handle())?;
        volume.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        Ok(Self {
            resources,
            raw,
            volume,
            params,
            sampler,
            pool,
            layout,
            set,
        })
    }

    /// Binds the atmosphere transmittance + multiscatter LUT views (with the IBL clamp `sampler`) into
    /// the fill set (bindings 0/1). Called once at renderer init; the LUT images are reused across
    /// bakes, so this persists (the fill dispatch is gated on a live atmosphere, not on a fresh bind).
    pub fn bind_luts(
        &self,
        device: &Device,
        ibl_sampler: vk::Sampler,
        transmittance: vk::ImageView,
        multi_scatter: vk::ImageView,
    ) {
        let t_info = [vk::DescriptorImageInfo::default()
            .sampler(ibl_sampler)
            .image_view(transmittance)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let m_info = [vk::DescriptorImageInfo::default()
            .sampler(ibl_sampler)
            .image_view(multi_scatter)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&t_info),
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&m_info),
        ];
        // SAFETY: the ash seam. The set + views + sampler outlive the call; single-threaded at the
        // (idle) build point.
        unsafe { device.raw().update_descriptor_sets(&writes, &[]) };
    }

    /// Uploads this frame's [`AerialParamsUbo`] into the host-mapped UBO (read by the fill pass).
    pub fn update_params(&self, params: &AerialParamsUbo) {
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized for one AerialParamsUbo.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(params).as_ptr(),
                self.params.mapped_ptr(),
                size_of::<AerialParamsUbo>(),
            );
        }
    }

    /// The AP volume's `(image, view, layout)` for the render-graph import (the fill writes it as a
    /// storage image, GENERAL).
    pub fn volume_import(&self) -> (vk::Image, vk::ImageView, vk::ImageLayout) {
        (self.volume.handle(), self.volume.view(), self.volume.layout)
    }

    /// Writes back the AP volume's resolved exit layout after the graph executes.
    pub fn set_volume_layout(&mut self, layout: vk::ImageLayout) {
        self.volume.layout = layout;
    }

    /// The AP volume's sampling view (composite fog-set binding 5).
    pub fn volume_view(&self) -> vk::ImageView {
        self.volume.view()
    }

    /// The linear-clamp sampler the composite reads the AP volume through.
    pub fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// The fill pass's descriptor set (LUTs + params + storage volume).
    pub fn fill_set(&self) -> vk::DescriptorSet {
        self.set
    }

    /// The fill pass's descriptor-set layout.
    pub fn fill_layout(&self) -> vk::DescriptorSetLayout {
        self.layout
    }
}

impl Drop for AerialPerspective {
    fn drop(&mut self) {
        let raw = &self.raw;
        // SAFETY: the ash seam. The renderer waits the GPU idle before dropping this resource, so
        // nothing below is in flight. The pool frees its set; the volume/buffer drop after.
        unsafe {
            raw.destroy_descriptor_pool(self.pool, None);
            raw.destroy_descriptor_set_layout(self.layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
        let _ = &self.resources;
    }
}

#[cfg(test)]
mod tests;
