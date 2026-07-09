//! Image-based lighting, the visible sky, and reflection probes.
//!
//! The IBL bake produces an environment cube → a diffuse irradiance cube + a
//! roughness-mipped prefiltered specular cube + a split-sum BRDF LUT, sampled as the
//! mesh ambient (set 3). Three environment sources sit behind [`EnvSource`]: a
//! procedural sky (`ibl_skygen`), a user equirect panorama (`ibl_equirect`), and the
//! Hillaire-2020 atmosphere LUT chain (`atmos_*` → `atmos_skygen`). The visible sky is a
//! fullscreen pass before the scene; reflection probes capture + prefilter local
//! environments into the same IBL set (bindings 3-5).
//!
//! # The re-bake is editor-time, not per-frame
//!
//! [`Ibl::rebake_pending`] is set by [`Ibl::request_env_bake`] when the sky inputs change
//! (the sun moves, the panorama swaps, the atmosphere params differ — gated by a `!=` on
//! the POD params, which derive [`PartialEq`]). The renderer fires the bake at the next
//! GPU-idle point (`begin_frame_graph` start) so the visible sky + IBL relight together.
//! The bake itself waits idle — one of the few legitimate mid-session `wait_idle` sites,
//! isolated to the bake method.

use std::sync::Arc;

use ash::vk;
use bytemuck::Zeroable;
use saffron_geometry::glam::{UVec4, Vec3, Vec4};
use vk_mem::Alloc;

use crate::descriptors::{Descriptors, MAX_REFLECTION_PROBES};
use crate::resources::{Buffer, DeviceResources, GpuTexture};
use crate::{Device, Error, Result, checked};

/// The IBL cube / LUT format — `R16G16B16A16_SFLOAT`, sampled and storage-written by the
/// convolution compute passes.
pub const IBL_COLOR_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
/// Source environment cube resolution per face. (Mirrored as `EnvSize` in `ibl_prefilter.slang` —
/// update both together.)
pub const IBL_ENV_SIZE: u32 = 256;
/// Diffuse irradiance cube resolution per face.
pub const IBL_IRRADIANCE_SIZE: u32 = 32;
/// Prefiltered specular cube base resolution per face: 256² mip-0 gives near-mirror metals 4× the
/// texels of a 128² base.
pub const IBL_PREFILTER_SIZE: u32 = 256;
/// Prefiltered specular mip count — `mesh.slang`'s `IblPrefilterMaxMip` must be this − 1.
pub const IBL_PREFILTER_MIPS: u32 = 5;
/// Split-sum BRDF LUT resolution.
pub const IBL_LUT_SIZE: u32 = 256;

/// Atmosphere transmittance LUT width (view-zenith).
pub const ATMOS_TRANSMITTANCE_W: u32 = 256;
/// Atmosphere transmittance LUT height (altitude).
pub const ATMOS_TRANSMITTANCE_H: u32 = 64;
/// Atmosphere isotropic multiple-scattering LUT size.
pub const ATMOS_MULTI_SCATTER_SIZE: u32 = 32;
/// Atmosphere sky-view LUT width (azimuth).
pub const ATMOS_SKY_VIEW_W: u32 = 192;
/// Atmosphere sky-view LUT height (elevation).
pub const ATMOS_SKY_VIEW_H: u32 = 108;

/// Which shader fills the IBL environment cube before the convolution chain.
///
/// The bake dispatches on it; a missing [`EnvSource::Equirect`] panorama degrades to
/// [`EnvSource::Procedural`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvSource {
    /// `ibl_skygen.slang` from [`SkygenParams`] (the default).
    #[default]
    Procedural,
    /// `ibl_equirect.slang` projecting a user panorama.
    Equirect,
    /// The `atmos_*` LUT chain into `atmos_skygen` (Hillaire 2020).
    Atmosphere,
}

/// Renderer-side mirror of the scene's atmosphere settings (the renderer does not import
/// the scene). A plain aggregate compared memberwise (`!=`) to gate the re-bake.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtmosphereParams {
    /// Gates the atmosphere LUT chain (false = the source falls back to procedural).
    pub enabled: bool,
    /// Planet radius (km).
    pub planet_radius: f32,
    /// Atmosphere thickness above the surface (km).
    pub atmosphere_height: f32,
    /// Rayleigh scattering coefficients (per channel).
    pub rayleigh_scattering: Vec3,
    /// Rayleigh density scale height (km).
    pub rayleigh_scale_height: f32,
    /// Mie scattering coefficient.
    pub mie_scattering: f32,
    /// Mie density scale height (km).
    pub mie_scale_height: f32,
    /// Mie phase anisotropy `g` (forward-scattering bias).
    pub mie_anisotropy: f32,
    /// Ozone absorption coefficients (per channel).
    pub ozone_absorption: Vec3,
    /// Sun disk angular radius (radians).
    pub sun_disk_angular_radius: f32,
    /// Sun disk intensity multiplier.
    pub sun_disk_intensity: f32,
}

impl Default for AtmosphereParams {
    fn default() -> Self {
        Self {
            enabled: false,
            planet_radius: 6360.0,
            atmosphere_height: 100.0,
            rayleigh_scattering: Vec3::new(5.802, 13.558, 33.1),
            rayleigh_scale_height: 8.0,
            mie_scattering: 3.996,
            mie_scale_height: 1.2,
            mie_anisotropy: 0.8,
            ozone_absorption: Vec3::new(0.650, 1.881, 0.085),
            sun_disk_angular_radius: 0.004_65,
            sun_disk_intensity: 20.0,
        }
    }
}

/// Inputs that drive the procedural-sky bake (`ibl_skygen`). The sun follows the scene's
/// directional light, so a re-bake re-tints the visible sky AND the IBL together. Derives
/// [`PartialEq`] so the "did the inputs change" check is a `!=`, not a hand-written
/// memberwise compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkygenParams {
    /// Direction TO the sun (= −lightDir); the shader normalizes.
    pub sun_dir: Vec3,
    /// Sun intensity multiplier.
    pub sun_intensity: f32,
    /// Sun color (RGB).
    pub sun_color: Vec3,
    /// Physically based source params; [`AtmosphereParams::enabled`] gates the LUT chain.
    pub atmosphere: AtmosphereParams,
}

impl Default for SkygenParams {
    fn default() -> Self {
        Self {
            sun_dir: Vec3::new(0.5, 1.0, 0.3),
            sun_intensity: 1.0,
            sun_color: Vec3::ONE,
            atmosphere: AtmosphereParams::default(),
        }
    }
}

/// The visible-sky settings the host pushes each frame.
/// Carried as POD so the renderer never imports the scene; `mode` matches `SkyMode`'s
/// values (0 = Color, 1 = Texture, 2 = Procedural).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyRenderSettings {
    /// 0 = Color (flat fill), 1 = Texture (bindless panorama), 2 = Procedural (env cube),
    /// 3 = the fixed studio gradient (see [`SKY_MODE_THUMBNAIL_GRADIENT`]).
    pub mode: u32,
    /// Color-mode flat fill (also the sky pass's clear color).
    pub clear_color: Vec3,
    /// Overall sky intensity (applied by the visible-sky pass, not baked).
    pub intensity: f32,
    /// Yaw rotation of the lookup around world up (radians).
    pub rotation: f32,
    /// Whether the visible-sky pass runs at all.
    pub visible: bool,
    /// Bindless panorama slot (Texture mode).
    pub texture_index: u32,
}

/// The visible-sky mode that draws the fixed neutral studio gradient (`sky.slang` mode 3): a
/// backdrop independent of the environment, forced on the offscreen thumbnail view so every subject
/// — including a chrome ball reflecting a dark HDRI — reads as a clean silhouette. Never a scene
/// `sky_mode`; the renderer overrides the submitted mode for the thumbnail draw only.
pub const SKY_MODE_THUMBNAIL_GRADIENT: u32 = 3;

impl Default for SkyRenderSettings {
    fn default() -> Self {
        Self {
            mode: 2,
            clear_color: Vec3::new(0.05, 0.06, 0.08),
            intensity: 1.0,
            rotation: 0.0,
            visible: true,
            texture_index: 0,
        }
    }
}

/// A per-frame snapshot of one reflection-probe component, passed from the host without
/// the renderer depending on the scene. `dirty` arms a (re)capture.
#[derive(Debug, Clone, Copy)]
pub struct ReflectionProbeUpload {
    /// Owning entity id (the capture re-uses the slot when re-armed).
    pub entity: u64,
    /// World-space origin (the entity's translation).
    pub origin: Vec3,
    /// Influence radius (world units).
    pub influence_radius: f32,
    /// Specular intensity multiplier.
    pub intensity: f32,
    /// Whether to box-project the cube (parallax-corrected reflections).
    pub box_projection: bool,
    /// Box half-extents (box-projection mode).
    pub box_extent: Vec3,
    /// Explicitly arms a (re)capture this frame.
    pub dirty: bool,
}

impl Default for ReflectionProbeUpload {
    fn default() -> Self {
        Self {
            entity: 0,
            origin: Vec3::ZERO,
            influence_radius: 10.0,
            intensity: 1.0,
            box_projection: false,
            box_extent: Vec3::splat(10.0),
            dirty: false,
        }
    }
}

/// The per-probe metadata record the mesh fragment reads (IBL set binding 5 SSBO). std430:
/// three 16-byte vec4/uvec4 blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ProbeMetaGpu {
    /// `xyz` world origin, `w` influence radius.
    pub origin_radius: Vec4,
    /// `xyz` box half-extents, `w` intensity.
    pub extent_intensity: Vec4,
    /// `x` valid (1/0), `y` box-projection (1/0), `zw` reserved.
    pub flags: UVec4,
}

const _: () = assert!(size_of::<ProbeMetaGpu>() == 48);

/// The push the procedural-skygen compute reads (`ibl_skygen.slang`): two vec4s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkygenPush {
    /// `xyz` direction to the sun, `w` sun intensity.
    sun_dir: Vec4,
    /// `rgb` sun color, `a` unused.
    sun_color: Vec4,
}

/// The push the equirect-projection compute reads (`ibl_equirect.slang`): one vec4
/// (`x` rotation, `y` intensity). The IBL bakes the raw panorama; the visible-sky pass
/// applies the scene's rotation/intensity itself.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EquirectPush {
    params: Vec4,
}

/// The push the atmosphere LUT + skygen passes share (`atmos_*.slang`): five vec4s packing
/// [`AtmosphereParams`] + the sun.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AtmosPush {
    /// `xyz` dir to sun, `w` sun intensity.
    sun_dir: Vec4,
    /// `xyz` rayleigh scattering, `w` rayleigh scale height.
    rayleigh: Vec4,
    /// `xyz` ozone absorption, `w` mie scattering.
    ozone: Vec4,
    /// `x` planet radius, `y` atmosphere height, `z` mie scale height, `w` mie anisotropy.
    params0: Vec4,
    /// `x` sun-disk angular radius, `y` sun-disk intensity, `zw` reserved (camera altitude).
    params1: Vec4,
}

impl AtmosPush {
    fn new(sky: &SkygenParams) -> Self {
        let a = &sky.atmosphere;
        Self {
            sun_dir: sky.sun_dir.normalize_or_zero().extend(sky.sun_intensity),
            rayleigh: a.rayleigh_scattering.extend(a.rayleigh_scale_height),
            ozone: a.ozone_absorption.extend(a.mie_scattering),
            params0: Vec4::new(
                a.planet_radius,
                a.atmosphere_height,
                a.mie_scale_height,
                a.mie_anisotropy,
            ),
            params1: Vec4::new(a.sun_disk_angular_radius, a.sun_disk_intensity, 0.0, 0.0),
        }
    }
}

/// The push the visible-sky fragment reads (`sky.slang`'s `SkyPush`): the inverse
/// view-projection + params + clear color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyPush {
    inv_view_proj: saffron_geometry::glam::Mat4,
    /// `x` intensity, `y` rotation, `z` mode, `w` texture index.
    params: Vec4,
    /// `rgb` Color-mode fill.
    clear_color: Vec4,
}

const _: () = assert!(size_of::<SkyPush>() == 64 + 16 + 16);

/// A `CUBE_COMPATIBLE` 6-layer color cube (sampled + storage) owning its handle, a
/// `CUBE` sampling view, and the VMA allocation. The convolution passes write it through
/// transient per-mip `TYPE_2D_ARRAY` storage views the bake creates and frees itself.
///
/// A move-only Drop type — its view goes through the device, its image through the
/// allocator (the `Image::reset()` order). `layout` tracks the cross-bake layout (the
/// bake's `UNDEFINED → GENERAL → SHADER_READ_ONLY` cycle).
struct IblCube {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
}

// SAFETY: as `Image` — no thread-affine state; vk-mem `Allocation` is Send.
unsafe impl Send for IblCube {}

impl IblCube {
    /// Creates a `size`²×6 cube of `mip_levels` mips, `IBL_COLOR_FORMAT`, sampled +
    /// storage, dedicated-allocated, with a `CUBE` sampling view spanning all mips/layers.
    fn new(resources: &Arc<DeviceResources>, size: u32, mip_levels: u32) -> Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::CUBE_COMPATIBLE)
            .image_type(vk::ImageType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .extent(vk::Extent3D {
                width: size,
                height: size,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(6)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            // TRANSFER_SRC|DST so the env cube can blit-generate its mip chain (filtered
            // importance sampling reads coarser source mips); harmless on the other cubes.
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image + allocation are
        // owned and freed in `Drop` (or below on a view-creation failure).
        let (image, allocation) = checked(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage (ibl cube)",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::CUBE)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 6,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe {
                    resources.allocator().destroy_image(image, &mut allocation);
                }
                return Err(Error::Vk {
                    context: "create_image_view (ibl cube)",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
        })
    }

    /// A transient `TYPE_2D_ARRAY` storage view over one mip (all 6 layers) — the
    /// convolution passes bind these as the storage-image output; the caller frees them.
    fn storage_view(&self, mip: u32) -> Result<vk::ImageView> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(self.image)
            .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: mip,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 6,
            });
        // SAFETY: the ash seam. The transient view is freed by the bake before returning.
        checked(
            unsafe { self.resources.device().create_image_view(&view_info, None) },
            "create_image_view (ibl storage)",
        )
    }
}

impl Drop for IblCube {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; the view
        // is destroyed through the device, then the image through the allocator. Each
        // handle is freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// A 2D color image (sampled + storage), the IBL BRDF LUT and the three atmosphere LUTs.
/// A move-only Drop type.
struct IblImage {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
}

// SAFETY: as `Image` — no thread-affine state; vk-mem `Allocation` is Send.
unsafe impl Send for IblImage {}

impl IblImage {
    /// Creates a `width`×`height` `IBL_COLOR_FORMAT` image, sampled + storage, with a
    /// `TYPE_2D` view that doubles as both the sampled and storage view.
    fn new(resources: &Arc<DeviceResources>, width: u32, height: u32) -> Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            ..Default::default()
        };
        // SAFETY: the VMA seam. Owned + freed in `Drop` (or below on a view failure).
        let (image, allocation) = checked(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage (ibl lut)",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe {
                    resources.allocator().destroy_image(image, &mut allocation);
                }
                return Err(Error::Vk {
                    context: "create_image_view (ibl lut)",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
        })
    }
}

impl Drop for IblImage {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. View through the device, then image through the
        // allocator. Each handle freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// Image-based lighting: the source environment cube convolved into a diffuse irradiance
/// cube + a roughness-mipped prefiltered specular cube + a split-sum BRDF LUT, plus the
/// atmosphere LUT chain. Sampled as the mesh ambient (set 3, bindings 0-2). Baked at
/// startup, re-baked on demand when the sky inputs change.
///
/// Owns its Vulkan handles + an
/// [`Arc`]`<`[`DeviceResources`]`>` so the images (Drop types) free without a live
/// `&Device`; the sampler/set-layout free in [`Drop`], the descriptor set with the pool.
pub struct Ibl {
    resources: Arc<DeviceResources>,
    env_cube: IblCube,
    irradiance_cube: IblCube,
    prefiltered_cube: IblCube,
    brdf_lut: IblImage,
    transmittance_lut: IblImage,
    multi_scatter_lut: IblImage,
    sky_view_lut: IblImage,
    prefilter_mips: u32,

    sampler: vk::Sampler,
    /// The descriptors' linear repeat sampler (the equirect panorama wraps in longitude;
    /// the clamp IBL sampler would seam the meridian). Borrowed, not owned.
    equirect_sampler: vk::Sampler,
    set: vk::DescriptorSet,
    /// Whether the bake has run and set 3 is written.
    pub ready: bool,
    /// Master IBL ambient toggle; false = the flat scalar ambient fallback.
    pub use_ibl: bool,

    baked_params: SkygenParams,
    pending_params: SkygenParams,
    /// `pending_params` differ from `baked_params` → re-bake armed at the next idle point.
    pub rebake_pending: bool,
    source: EnvSource,
    baked_source: EnvSource,
    /// The equirect source panorama, held alive across the bake.
    env_panorama: Option<Arc<GpuTexture>>,
}

impl Ibl {
    /// Allocates the IBL images, the linear/clamp/mipped sampler, and the persistent set 3
    /// from the shared pool, then runs the first bake (procedural sky) so set 3 is valid
    /// before the first frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing image/sampler/set/pipeline/bake step.
    pub fn new(device: &Device, descriptors: &Descriptors) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let mips = IBL_PREFILTER_MIPS;
        // The env cube carries a full mip chain so the prefilter's filtered importance sampling can
        // read coarser (pre-averaged) source mips — the firefly/aliasing fix.
        let env_mips = IBL_ENV_SIZE.ilog2() + 1;
        let env_cube = IblCube::new(&resources, IBL_ENV_SIZE, env_mips)?;
        let irradiance_cube = IblCube::new(&resources, IBL_IRRADIANCE_SIZE, 1)?;
        let prefiltered_cube = IblCube::new(&resources, IBL_PREFILTER_SIZE, mips)?;
        let brdf_lut = IblImage::new(&resources, IBL_LUT_SIZE, IBL_LUT_SIZE)?;
        let transmittance_lut =
            IblImage::new(&resources, ATMOS_TRANSMITTANCE_W, ATMOS_TRANSMITTANCE_H)?;
        let multi_scatter_lut = IblImage::new(
            &resources,
            ATMOS_MULTI_SCATTER_SIZE,
            ATMOS_MULTI_SCATTER_SIZE,
        )?;
        let sky_view_lut = IblImage::new(&resources, ATMOS_SKY_VIEW_W, ATMOS_SKY_VIEW_H)?;

        let sampler = create_ibl_sampler(resources.device())?;
        let set = match descriptors.allocate_set(descriptors.ibl_set_layout()) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the ash seam. The sampler was created just above; free it on
                // the early return (the images free via their Drop).
                unsafe { resources.device().destroy_sampler(sampler, None) };
                return Err(err);
            }
        };

        Ok(Self {
            resources,
            env_cube,
            irradiance_cube,
            prefiltered_cube,
            brdf_lut,
            transmittance_lut,
            multi_scatter_lut,
            sky_view_lut,
            prefilter_mips: mips,
            sampler,
            equirect_sampler: descriptors.linear_sampler(),
            set,
            ready: false,
            use_ibl: true,
            baked_params: SkygenParams::default(),
            pending_params: SkygenParams::default(),
            rebake_pending: false,
            source: EnvSource::Procedural,
            baked_source: EnvSource::Procedural,
            env_panorama: None,
        })
    }

    /// The IBL set (set 3 in the mesh pipeline; also the reflection-probe set).
    pub fn set(&self) -> vk::DescriptorSet {
        self.set
    }

    /// The IBL linear/clamp/mipped sampler (shared by the sky + probe sets).
    pub fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// The source environment cube's sampling view (the visible-sky procedural mode).
    pub fn env_cube_view(&self) -> vk::ImageView {
        self.env_cube.view
    }

    /// The global diffuse irradiance cube's sampling view (the probe-slot fallback).
    pub fn irradiance_cube_view(&self) -> vk::ImageView {
        self.irradiance_cube.view
    }

    /// The global prefiltered specular cube's sampling view (the probe-slot fallback).
    pub fn prefiltered_cube_view(&self) -> vk::ImageView {
        self.prefiltered_cube.view
    }

    /// Re-arms the environment bake when the source / panorama / params change. An exact
    /// `!=` over the POD params flags only real user changes —
    /// no per-frame float drift, no churn. The bake fires at the next idle point.
    pub fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<GpuTexture>>,
        params: SkygenParams,
    ) {
        let pano_changed = source == EnvSource::Equirect
            && match (&self.env_panorama, &panorama) {
                (Some(old), Some(new)) => old.bindless_index() != new.bindless_index(),
                _ => true,
            };
        if should_rebake(
            source,
            self.baked_source,
            &params,
            &self.baked_params,
            pano_changed,
        ) {
            self.rebake_pending = true;
        }
        self.source = source;
        self.env_panorama = panorama;
        self.pending_params = params;
    }

    /// Fires the armed re-bake: bakes the pending params, then commits them as the baked
    /// set on success. Clears `rebake_pending`
    /// regardless (a failed bake is logged, not retried in a tight loop).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the bake fails (the caller logs it).
    pub fn fire_rebake(&mut self, device: &Device) -> Result<()> {
        self.rebake_pending = false;
        self.bake(device, false)?;
        self.baked_params = self.pending_params;
        self.baked_source = self.source;
        Ok(())
    }

    /// Runs the IBL bake: fill the environment cube (procedural / equirect / atmosphere),
    /// convolve it into the diffuse-irradiance + roughness-mipped prefiltered cubes,
    /// integrate the split-sum BRDF LUT, and (on the first bake) write the persistent set
    /// 3. Synchronous one-shot work on its own command pool + `wait_idle` — an editor-time
    /// event, not per-frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing pool / pipeline / submit step.
    pub fn bake(&mut self, device: &Device, first_bake: bool) -> Result<()> {
        let sky = self.pending_params;
        let mips = self.prefilter_mips;
        let raw = device.raw().clone();
        let use_atmosphere = self.source == EnvSource::Atmosphere && sky.atmosphere.enabled;
        let use_equirect = self.source == EnvSource::Equirect && self.env_panorama.is_some();
        if self.source == EnvSource::Equirect && self.env_panorama.is_none() {
            tracing::warn!("ibl bake: Equirect source has no panorama; falling back to procedural");
        }

        // A re-bake overwrites the existing images in place (the UNDEFINED→GENERAL barriers
        // below discard prior contents); drain any in-flight frame still sampling them.
        if !first_bake {
            device.wait_idle()?;
        }

        // The transient bake state (pool + compute pipelines + descriptor pool/layouts +
        // storage views + fence + command pool). Built fresh per bake, freed at the end via
        // `BakeScratch`'s Drop. The probe convolve shares the same pattern.
        let mut scratch = BakeScratch::new(&raw, device, use_atmosphere)?;

        let env_store = self.env_cube.storage_view(0)?;
        scratch.transient_views.push(env_store);
        let irr_store = self.irradiance_cube.storage_view(0)?;
        scratch.transient_views.push(irr_store);
        let mut pre_store = Vec::with_capacity(mips as usize);
        for m in 0..mips {
            let view = self.prefiltered_cube.storage_view(m)?;
            scratch.transient_views.push(view);
            pre_store.push(view);
        }

        // Allocate + write the per-set descriptors against the transient pool.
        let views = BakeStorageViews {
            env: env_store,
            irradiance: irr_store,
            prefilter: &pre_store,
        };
        scratch.write_sets(&raw, self, &views, use_atmosphere, use_equirect)?;

        let atmos = AtmosPush::new(&sky);
        // SAFETY: the ash seam. The command buffer is freshly allocated; the pipelines /
        // sets / images all live for the duration of the recording, submitted + waited
        // below. Every layout transition is an explicit sync2 barrier.
        unsafe {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            checked(raw.begin_command_buffer(scratch.cmd, &begin), "ibl begin")?;

            if use_atmosphere {
                self.record_atmosphere(&raw, &scratch, &atmos);
            }

            // Environment cube → general, fill it, → shader-read for the convolutions.
            cube_barrier(
                &raw,
                scratch.cmd,
                self.env_cube.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                0,
                1,
            );
            if use_atmosphere {
                bind_dispatch_push(
                    &raw,
                    scratch.cmd,
                    &scratch.atmos_skygen,
                    scratch.atmos_skygen_set,
                    bytemuck::bytes_of(&atmos),
                    group(IBL_ENV_SIZE),
                    group(IBL_ENV_SIZE),
                    6,
                );
            } else if use_equirect {
                let push = EquirectPush {
                    params: Vec4::new(0.0, 1.0, 0.0, 0.0),
                };
                bind_dispatch_push(
                    &raw,
                    scratch.cmd,
                    &scratch.equirect,
                    scratch.equirect_set,
                    bytemuck::bytes_of(&push),
                    group(IBL_ENV_SIZE),
                    group(IBL_ENV_SIZE),
                    6,
                );
            } else {
                let push = SkygenPush {
                    sun_dir: sky.sun_dir.extend(sky.sun_intensity),
                    sun_color: sky.sun_color.extend(1.0),
                };
                bind_dispatch_push(
                    &raw,
                    scratch.cmd,
                    &scratch.skygen,
                    scratch.skygen_set,
                    bytemuck::bytes_of(&push),
                    group(IBL_ENV_SIZE),
                    group(IBL_ENV_SIZE),
                    6,
                );
            }
            // Generate the env cube's mip chain (mip 0 was just written, in GENERAL) and leave
            // every mip in SHADER_READ — the prefilter's filtered importance sampling reads the
            // coarser mips to kill fireflies/aliasing.
            let env_mips = IBL_ENV_SIZE.ilog2() + 1;
            generate_cube_mips(
                &raw,
                scratch.cmd,
                self.env_cube.image,
                IBL_ENV_SIZE,
                env_mips,
            );

            // Diffuse irradiance.
            cube_barrier(
                &raw,
                scratch.cmd,
                self.irradiance_cube.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                0,
                1,
            );
            bind_dispatch(
                &raw,
                scratch.cmd,
                &scratch.irradiance,
                scratch.irradiance_set,
                group(IBL_IRRADIANCE_SIZE),
                group(IBL_IRRADIANCE_SIZE),
                6,
            );
            cube_barrier(
                &raw,
                scratch.cmd,
                self.irradiance_cube.image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
                0,
                1,
            );

            // Prefiltered specular: one dispatch per mip (roughness = mip / (mips-1)).
            cube_barrier(
                &raw,
                scratch.cmd,
                self.prefiltered_cube.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                0,
                mips,
            );
            raw.cmd_bind_pipeline(
                scratch.cmd,
                vk::PipelineBindPoint::COMPUTE,
                scratch.prefilter.handle,
            );
            for m in 0..mips {
                let mip_size = (IBL_PREFILTER_SIZE >> m).max(1);
                let roughness = if mips > 1 {
                    m as f32 / (mips - 1) as f32
                } else {
                    0.0
                };
                raw.cmd_push_constants(
                    scratch.cmd,
                    scratch.prefilter.layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&roughness),
                );
                raw.cmd_bind_descriptor_sets(
                    scratch.cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    scratch.prefilter.layout,
                    0,
                    &[scratch.prefilter_sets[m as usize]],
                    &[],
                );
                raw.cmd_dispatch(scratch.cmd, group(mip_size), group(mip_size), 6);
            }
            cube_barrier(
                &raw,
                scratch.cmd,
                self.prefiltered_cube.image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
                0,
                mips,
            );

            // Split-sum BRDF LUT (2D, single layer).
            cube_barrier(
                &raw,
                scratch.cmd,
                self.brdf_lut.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                0,
                1,
            );
            bind_dispatch(
                &raw,
                scratch.cmd,
                &scratch.brdf,
                scratch.brdf_set,
                group(IBL_LUT_SIZE),
                group(IBL_LUT_SIZE),
                1,
            );
            cube_barrier(
                &raw,
                scratch.cmd,
                self.brdf_lut.image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
                0,
                1,
            );

            checked(raw.end_command_buffer(scratch.cmd), "ibl end")?;
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(scratch.cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            checked(
                raw.queue_submit2(device.graphics_queue, &submit, scratch.fence),
                "ibl submit",
            )?;
            checked(
                raw.wait_for_fences(&[scratch.fence], true, u64::MAX),
                "ibl wait",
            )?;
        }

        // The bake leaves every image in SHADER_READ_ONLY_OPTIMAL (the final per-stage
        // barrier above), so the mesh / sky directly sample them — these are never imported
        // into the per-frame render graph, so no cross-frame layout slot is tracked.

        // First bake writes the persistent set 3 the mesh fragment samples (bindings 0-2);
        // a re-bake reuses the same images/views, so the set stays valid.
        if first_bake {
            self.write_mesh_set(&raw);
            self.ready = true;
        }
        tracing::debug!(
            "ibl baked — env {IBL_ENV_SIZE}^2, irradiance {IBL_IRRADIANCE_SIZE}^2, prefiltered \
             {IBL_PREFILTER_SIZE}^2 x{mips} mips, lut {IBL_LUT_SIZE}^2{}",
            if use_atmosphere { " (atmosphere)" } else { "" }
        );
        Ok(())
    }

    /// Records the atmosphere LUT chain (transmittance → multiscatter → skyview), each
    /// `UNDEFINED → GENERAL`, dispatch, `GENERAL → SHADER_READ` so the next stage samples
    /// it.
    ///
    /// The caller holds an open command buffer; the pipelines/sets/images live for the
    /// recording. Every transition is an explicit sync2 barrier (the barrier/dispatch
    /// helpers wrap the ash seam internally, so this is a safe fn).
    fn record_atmosphere(&self, raw: &ash::Device, scratch: &BakeScratch, atmos: &AtmosPush) {
        let push = bytemuck::bytes_of(atmos);
        let chain = [
            (
                self.transmittance_lut.image,
                &scratch.atmos_transmittance,
                scratch.atmos_transmittance_set,
                ATMOS_TRANSMITTANCE_W,
                ATMOS_TRANSMITTANCE_H,
            ),
            (
                self.multi_scatter_lut.image,
                &scratch.atmos_multiscatter,
                scratch.atmos_multiscatter_set,
                ATMOS_MULTI_SCATTER_SIZE,
                ATMOS_MULTI_SCATTER_SIZE,
            ),
            (
                self.sky_view_lut.image,
                &scratch.atmos_skyview,
                scratch.atmos_skyview_set,
                ATMOS_SKY_VIEW_W,
                ATMOS_SKY_VIEW_H,
            ),
        ];
        for (image, pipeline, set, w, h) in chain {
            cube_barrier(
                raw,
                scratch.cmd,
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                0,
                1,
            );
            bind_dispatch_push(raw, scratch.cmd, pipeline, set, push, group(w), group(h), 1);
            cube_barrier(
                raw,
                scratch.cmd,
                image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
                0,
                1,
            );
        }
    }

    /// Writes the persistent set 3 (bindings 0-2: irradiance / prefiltered / BRDF LUT) the
    /// mesh fragment samples.
    fn write_mesh_set(&self, raw: &ash::Device) {
        let infos = [
            self.irradiance_cube.view,
            self.prefiltered_cube.view,
            self.brdf_lut.view,
        ];
        let image_infos: Vec<vk::DescriptorImageInfo> = infos
            .iter()
            .map(|&view| {
                vk::DescriptorImageInfo::default()
                    .sampler(self.sampler)
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            })
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = (0..3)
            .map(|b| {
                vk::WriteDescriptorSet::default()
                    .dst_set(self.set)
                    .dst_binding(b)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&image_infos[b as usize]))
            })
            .collect();
        // SAFETY: the ash seam. The set/views/sampler outlive the renderer; host access to
        // the set is single-threaded at the (idle) bake point.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }
}

impl Drop for Ibl {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The device idled before teardown (the renderer's Drop); the
        // sampler is freed exactly once. The images free via their own Drop; the set frees
        // with the shared descriptor pool.
        unsafe {
            self.resources.device().destroy_sampler(self.sampler, None);
        }
    }
}

/// `(n + 7) / 8` — the 8×8 compute group count covering `n`.
fn group(n: u32) -> u32 {
    n.div_ceil(8)
}

/// The re-bake decision, pure so it is unit-testable
/// without a device. A re-bake is armed on a source change, a panorama change (Equirect),
/// a sun change (Procedural/Atmosphere), or an atmosphere-param change (Atmosphere only).
/// Identical inputs → no re-bake (the exact `!=` over the POD params is the whole point).
fn should_rebake(
    source: EnvSource,
    baked_source: EnvSource,
    params: &SkygenParams,
    baked: &SkygenParams,
    pano_changed: bool,
) -> bool {
    let source_changed = source != baked_source;
    let sky_changed = params.sun_dir != baked.sun_dir
        || params.sun_color != baked.sun_color
        || params.sun_intensity != baked.sun_intensity;
    let atmos_changed = params.atmosphere != baked.atmosphere;
    source_changed
        || pano_changed
        || (source == EnvSource::Procedural && sky_changed)
        || (source == EnvSource::Atmosphere && (sky_changed || atmos_changed))
}

/// One transient compute pipeline + its layout (freed by [`BakeScratch::drop`]).
struct ComputePso {
    handle: vk::Pipeline,
    layout: vk::PipelineLayout,
}

/// The transient `TYPE_2D_ARRAY` storage views the bake writes into: the env cube mip 0,
/// the irradiance cube mip 0, and one per prefiltered mip. A borrowing param bundle so
/// [`BakeScratch::write_sets`] reads as named fields.
struct BakeStorageViews<'a> {
    env: vk::ImageView,
    irradiance: vk::ImageView,
    prefilter: &'a [vk::ImageView],
}

/// The transient GPU state one bake (or one probe convolve) creates and frees: the command
/// pool + buffer + fence, the descriptor pool + the three set layouts, the compute
/// pipelines, the transient image views, and the allocated sets. A move-only Drop type so
/// every handle is released on the function's exit path — success or error.
struct BakeScratch {
    resources: Arc<DeviceResources>,
    command_pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    descriptor_pool: vk::DescriptorPool,
    layout_a: vk::DescriptorSetLayout,
    layout_b: vk::DescriptorSetLayout,
    layout_c: Option<vk::DescriptorSetLayout>,
    transient_views: Vec<vk::ImageView>,

    skygen: ComputePso,
    equirect: ComputePso,
    irradiance: ComputePso,
    prefilter: ComputePso,
    brdf: ComputePso,
    atmos_transmittance: ComputePso,
    atmos_multiscatter: ComputePso,
    atmos_skyview: ComputePso,
    atmos_skygen: ComputePso,
    pipelines: Vec<ComputePso>,

    skygen_set: vk::DescriptorSet,
    equirect_set: vk::DescriptorSet,
    brdf_set: vk::DescriptorSet,
    irradiance_set: vk::DescriptorSet,
    prefilter_sets: Vec<vk::DescriptorSet>,
    atmos_transmittance_set: vk::DescriptorSet,
    atmos_multiscatter_set: vk::DescriptorSet,
    atmos_skyview_set: vk::DescriptorSet,
    atmos_skygen_set: vk::DescriptorSet,
}

impl BakeScratch {
    /// Allocates the bake's command pool/buffer/fence, the transient descriptor pool + the
    /// three set layouts (A = 1 storage, B = sampler + storage, C = 2 samplers + storage),
    /// and every compute pipeline (the atmosphere chain only when `atmosphere`).
    fn new(raw: &ash::Device, device: &Device, atmosphere: bool) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed in `Drop`.
        let command_pool = checked(
            unsafe { raw.create_command_pool(&pool_info, None) },
            "ibl cmd pool",
        )?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. Freed with the pool.
        let cmd = match unsafe { raw.allocate_command_buffers(&alloc) } {
            Ok(cmds) => cmds[0],
            Err(result) => {
                // SAFETY: the ash seam. Free the pool before the early return.
                unsafe { raw.destroy_command_pool(command_pool, None) };
                return Err(Error::Vk {
                    context: "ibl alloc cmd",
                    result,
                });
            }
        };
        // SAFETY: the ash seam. Freed in `Drop`.
        let fence = match unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) } {
            Ok(fence) => fence,
            Err(result) => {
                // SAFETY: the ash seam. Free the pool before the early return.
                unsafe { raw.destroy_command_pool(command_pool, None) };
                return Err(Error::Vk {
                    context: "ibl fence",
                    result,
                });
            }
        };

        // The transient descriptor pool + the three set layouts. Built before the pipelines
        // so a failure frees the pool/fence/layouts via the partial struct's Drop.
        let mut scratch = Self {
            resources,
            command_pool,
            cmd,
            fence,
            descriptor_pool: vk::DescriptorPool::null(),
            layout_a: vk::DescriptorSetLayout::null(),
            layout_b: vk::DescriptorSetLayout::null(),
            layout_c: None,
            transient_views: Vec::new(),
            skygen: ComputePso::null(),
            equirect: ComputePso::null(),
            irradiance: ComputePso::null(),
            prefilter: ComputePso::null(),
            brdf: ComputePso::null(),
            atmos_transmittance: ComputePso::null(),
            atmos_multiscatter: ComputePso::null(),
            atmos_skyview: ComputePso::null(),
            atmos_skygen: ComputePso::null(),
            pipelines: Vec::new(),
            skygen_set: vk::DescriptorSet::null(),
            equirect_set: vk::DescriptorSet::null(),
            brdf_set: vk::DescriptorSet::null(),
            irradiance_set: vk::DescriptorSet::null(),
            prefilter_sets: Vec::new(),
            atmos_transmittance_set: vk::DescriptorSet::null(),
            atmos_multiscatter_set: vk::DescriptorSet::null(),
            atmos_skyview_set: vk::DescriptorSet::null(),
            atmos_skygen_set: vk::DescriptorSet::null(),
        };

        scratch.descriptor_pool = create_bake_pool(raw)?;
        scratch.layout_a = create_layout_a(raw)?;
        scratch.layout_b = create_layout_b(raw)?;
        if atmosphere {
            scratch.layout_c = Some(create_layout_c(raw)?);
        }

        // The compute pipelines. `pipelines` owns each one's Drop; the named fields are
        // copies of the (handle, layout) pair for the dispatch sites (no double-free —
        // only `pipelines` runs the ComputePso Drop, the named copies are `null` Drops).
        let dir = crate::pipelines::resolve_shader_dir();
        scratch.skygen = scratch.add_pipeline(raw, &dir, "ibl_skygen.spv", scratch.layout_a, 32)?;
        scratch.equirect =
            scratch.add_pipeline(raw, &dir, "ibl_equirect.spv", scratch.layout_b, 16)?;
        scratch.irradiance =
            scratch.add_pipeline(raw, &dir, "ibl_irradiance.spv", scratch.layout_b, 0)?;
        scratch.prefilter = scratch.add_pipeline(
            raw,
            &dir,
            "ibl_prefilter.spv",
            scratch.layout_b,
            size_of::<f32>() as u32,
        )?;
        scratch.brdf = scratch.add_pipeline(raw, &dir, "ibl_brdf.spv", scratch.layout_a, 0)?;
        if atmosphere {
            let layout_c = scratch.layout_c.expect("layout_c built for atmosphere");
            let push = size_of::<AtmosPush>() as u32;
            scratch.atmos_transmittance = scratch.add_pipeline(
                raw,
                &dir,
                "atmos_transmittance.spv",
                scratch.layout_a,
                push,
            )?;
            scratch.atmos_multiscatter =
                scratch.add_pipeline(raw, &dir, "atmos_multiscatter.spv", layout_c, push)?;
            scratch.atmos_skyview =
                scratch.add_pipeline(raw, &dir, "atmos_skyview.spv", layout_c, push)?;
            scratch.atmos_skygen =
                scratch.add_pipeline(raw, &dir, "atmos_skygen.spv", scratch.layout_b, push)?;
        }
        Ok(scratch)
    }

    /// Builds one compute pipeline, pushes the owning `ComputePso` onto `pipelines`, and
    /// returns a (handle, layout) copy for the dispatch site.
    fn add_pipeline(
        &mut self,
        raw: &ash::Device,
        dir: &std::path::Path,
        shader: &str,
        set_layout: vk::DescriptorSetLayout,
        push_size: u32,
    ) -> Result<ComputePso> {
        let pso = build_compute_pipeline(raw, dir, shader, set_layout, push_size)?;
        let copy = ComputePso {
            handle: pso.handle,
            layout: pso.layout,
        };
        self.pipelines.push(pso);
        Ok(copy)
    }

    /// Allocates + writes every descriptor set the bake binds.
    fn write_sets(
        &mut self,
        raw: &ash::Device,
        ibl: &Ibl,
        views: &BakeStorageViews<'_>,
        atmosphere: bool,
        equirect: bool,
    ) -> Result<()> {
        let env_store = views.env;
        let irr_store = views.irradiance;
        let pre_store = views.prefilter;
        self.skygen_set = self.alloc_set(raw, self.layout_a)?;
        self.equirect_set = self.alloc_set(raw, self.layout_b)?;
        self.brdf_set = self.alloc_set(raw, self.layout_a)?;
        self.irradiance_set = self.alloc_set(raw, self.layout_b)?;
        for _ in 0..pre_store.len() {
            let set = self.alloc_set(raw, self.layout_b)?;
            self.prefilter_sets.push(set);
        }

        write_storage(raw, self.skygen_set, 0, env_store);
        write_storage(raw, self.brdf_set, 0, ibl.brdf_lut.view);
        write_sampler(raw, self.irradiance_set, 0, ibl.sampler, ibl.env_cube.view);
        write_storage(raw, self.irradiance_set, 1, irr_store);
        for (m, &store) in pre_store.iter().enumerate() {
            write_sampler(
                raw,
                self.prefilter_sets[m],
                0,
                ibl.sampler,
                ibl.env_cube.view,
            );
            write_storage(raw, self.prefilter_sets[m], 1, store);
        }

        if equirect && !atmosphere {
            let panorama = ibl
                .env_panorama
                .as_ref()
                .expect("equirect panorama present");
            // The panorama wraps in longitude, so it reads through the eRepeat linear
            // sampler (the IBL sampler is clamp and would seam the meridian).
            write_sampler(
                raw,
                self.equirect_set,
                0,
                ibl.equirect_sampler,
                panorama.view(),
            );
            write_storage(raw, self.equirect_set, 1, env_store);
        }

        if atmosphere {
            let layout_c = self.layout_c.expect("layout_c built for atmosphere");
            self.atmos_transmittance_set = self.alloc_set(raw, self.layout_a)?;
            self.atmos_multiscatter_set = self.alloc_set(raw, layout_c)?;
            self.atmos_skyview_set = self.alloc_set(raw, layout_c)?;
            self.atmos_skygen_set = self.alloc_set(raw, self.layout_b)?;

            write_storage(
                raw,
                self.atmos_transmittance_set,
                0,
                ibl.transmittance_lut.view,
            );
            write_sampler(
                raw,
                self.atmos_multiscatter_set,
                0,
                ibl.sampler,
                ibl.transmittance_lut.view,
            );
            write_storage(
                raw,
                self.atmos_multiscatter_set,
                2,
                ibl.multi_scatter_lut.view,
            );
            write_sampler(
                raw,
                self.atmos_skyview_set,
                0,
                ibl.sampler,
                ibl.transmittance_lut.view,
            );
            write_sampler(
                raw,
                self.atmos_skyview_set,
                1,
                ibl.sampler,
                ibl.multi_scatter_lut.view,
            );
            write_storage(raw, self.atmos_skyview_set, 2, ibl.sky_view_lut.view);
            write_sampler(
                raw,
                self.atmos_skygen_set,
                0,
                ibl.sampler,
                ibl.sky_view_lut.view,
            );
            write_storage(raw, self.atmos_skygen_set, 1, env_store);
        }
        Ok(())
    }

    /// Allocates one descriptor set of `layout` from the transient bake pool.
    fn alloc_set(
        &self,
        raw: &ash::Device,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet> {
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. The set frees with the pool in `Drop`.
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&info) },
            "ibl alloc set",
        )?;
        Ok(sets[0])
    }
}

impl Drop for BakeScratch {
    fn drop(&mut self) {
        let raw = self.resources.device().clone();
        // SAFETY: the ash seam. The fence was waited (or the submit never happened) by the
        // bake before this Drop runs, so every handle is idle. Each is freed exactly once;
        // the named-field `ComputePso` copies are `null` so only `pipelines` frees them.
        unsafe {
            for view in self.transient_views.drain(..) {
                raw.destroy_image_view(view, None);
            }
            for pso in self.pipelines.drain(..) {
                raw.destroy_pipeline(pso.handle, None);
                raw.destroy_pipeline_layout(pso.layout, None);
            }
            if let Some(layout_c) = self.layout_c.take() {
                raw.destroy_descriptor_set_layout(layout_c, None);
            }
            raw.destroy_descriptor_set_layout(self.layout_b, None);
            raw.destroy_descriptor_set_layout(self.layout_a, None);
            raw.destroy_descriptor_pool(self.descriptor_pool, None);
            raw.destroy_fence(self.fence, None);
            raw.destroy_command_pool(self.command_pool, None);
        }
    }
}

impl ComputePso {
    /// A null placeholder — the named dispatch-site fields hold (handle, layout) copies of
    /// pipelines owned by `BakeScratch::pipelines`; a `null` here is never freed twice.
    fn null() -> Self {
        Self {
            handle: vk::Pipeline::null(),
            layout: vk::PipelineLayout::null(),
        }
    }
}

/// The transient descriptor pool the bake's sets allocate against (16 storage images +
/// 16 samplers, 32 sets).
fn create_bake_pool(raw: &ash::Device) -> Result<vk::DescriptorPool> {
    let pool_sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(16),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(16),
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(32)
        .pool_sizes(&pool_sizes);
    // SAFETY: the ash seam. Freed in `BakeScratch::drop`.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "ibl bake pool",
    )
}

/// Layout A — one compute-stage storage image (binding 0): the skygen / BRDF / transmittance
/// outputs.
fn create_layout_a(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::COMPUTE)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. Freed in `BakeScratch::drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl layoutA",
    )
}

/// Layout B — a sampler (binding 0) + a storage image (binding 1): the equirect / irradiance /
/// prefilter / atmos-skygen passes (sample one cube/LUT, write the other).
fn create_layout_b(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. Freed in `BakeScratch::drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl layoutB",
    )
}

/// Layout C — two samplers (bindings 0-1) + a storage image (binding 2): the multiscatter /
/// skyview atmosphere passes (read two prior LUTs, write one out).
fn create_layout_c(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. Freed in `BakeScratch::drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl layoutC",
    )
}

/// Builds a transient compute pipeline from `dir/shader` over `set_layout` with an optional
/// compute-stage push of `push_size` bytes (0 = none). Entry point `computeMain`.
fn build_compute_pipeline(
    raw: &ash::Device,
    dir: &std::path::Path,
    shader: &str,
    set_layout: vk::DescriptorSetLayout,
    push_size: u32,
) -> Result<ComputePso> {
    let path = dir.join(shader);
    let bytes = std::fs::read(&path)
        .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}' ({} bytes)",
            path.display(),
            bytes.len()
        )));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: the ash seam. The module is freed after pipeline creation below.
    let module = checked(
        unsafe { raw.create_shader_module(&module_info, None) },
        "ibl shader module",
    )?;

    let set_layouts = [set_layout];
    let push_constant = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
        .offset(0)
        .size(push_size)];
    let mut layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
    if push_size > 0 {
        layout_info = layout_info.push_constant_ranges(&push_constant);
    }
    // SAFETY: the ash seam. The layout is owned by the returned `ComputePso`.
    let layout = match checked(
        unsafe { raw.create_pipeline_layout(&layout_info, None) },
        "ibl pipeline layout",
    ) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. The module was created above; freed once here.
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
    // SAFETY: the ash seam. On failure both the layout and module are freed.
    let created =
        unsafe { raw.create_compute_pipelines(vk::PipelineCache::null(), &pipeline_info, None) };
    // SAFETY: the ash seam. The module is consumed by creation; free it now.
    unsafe { raw.destroy_shader_module(module, None) };
    let handle = match created {
        Ok(pipelines) => pipelines[0],
        Err((_, result)) => {
            // SAFETY: the ash seam. The layout was created above; freed once here.
            unsafe { raw.destroy_pipeline_layout(layout, None) };
            return Err(Error::Vk {
                context: "ibl create_compute_pipelines",
                result,
            });
        }
    };
    Ok(ComputePso { handle, layout })
}

/// A sync2 image-layout transition over `[base_layer..base_layer+layer_count]` mips
/// `[0..mip_count]`, all 6 cube layers — the bake's per-stage barrier.
#[allow(clippy::too_many_arguments)]
fn cube_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
    base_mip: u32,
    mip_count: u32,
) {
    let barrier = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: base_mip,
            level_count: mip_count,
            base_array_layer: 0,
            layer_count: vk::REMAINING_ARRAY_LAYERS,
        })];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barrier);
    // SAFETY: the ash seam. The barrier references an image the bake created/owns.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// Generates a cube image's mip chain by successive linear blits, then leaves every mip in
/// `SHADER_READ_ONLY` for the convolution passes. Mip 0 must already be filled and in `GENERAL`
/// (the env bake just wrote it). The filtered-importance prefilter reads these coarser, pre-averaged
/// mips, which is what suppresses fireflies/aliasing with a low GGX sample count.
///
/// # Safety
///
/// The ash blit/barrier seam: `image` must be a 6-layer cube with `mip_levels` mips and
/// `TRANSFER_SRC|DST` usage, mip 0 in `GENERAL`; `cmd` is recording.
unsafe fn generate_cube_mips(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    base_size: u32,
    mip_levels: u32,
) {
    // Mip 0: GENERAL (just written by the env dispatch) → TRANSFER_SRC.
    cube_barrier(
        raw,
        cmd,
        image,
        vk::ImageLayout::GENERAL,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE,
        vk::PipelineStageFlags2::ALL_TRANSFER,
        vk::AccessFlags2::TRANSFER_READ,
        0,
        1,
    );
    for m in 1..mip_levels {
        let src = (base_size >> (m - 1)).max(1) as i32;
        let dst = (base_size >> m).max(1) as i32;
        // Dest mip: UNDEFINED → TRANSFER_DST.
        cube_barrier(
            raw,
            cmd,
            image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::TOP_OF_PIPE,
            vk::AccessFlags2::empty(),
            vk::PipelineStageFlags2::ALL_TRANSFER,
            vk::AccessFlags2::TRANSFER_WRITE,
            m,
            1,
        );
        let region = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: m - 1,
                base_array_layer: 0,
                layer_count: 6,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: src,
                    y: src,
                    z: 1,
                },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: m,
                base_array_layer: 0,
                layer_count: 6,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: dst,
                    y: dst,
                    z: 1,
                },
            ]);
        // SAFETY: the ash seam. Both subresources are valid mips of `image`, in the layouts the
        // barriers above just set; the regions are within the per-mip extents.
        unsafe {
            raw.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
                vk::Filter::LINEAR,
            );
        }
        // This mip becomes the source for the next blit: TRANSFER_DST → TRANSFER_SRC.
        cube_barrier(
            raw,
            cmd,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::ALL_TRANSFER,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::PipelineStageFlags2::ALL_TRANSFER,
            vk::AccessFlags2::TRANSFER_READ,
            m,
            1,
        );
    }
    // Every mip is now TRANSFER_SRC → move them all to SHADER_READ for the convolution passes.
    cube_barrier(
        raw,
        cmd,
        image,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::PipelineStageFlags2::ALL_TRANSFER,
        vk::AccessFlags2::TRANSFER_READ,
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
        0,
        mip_levels,
    );
}

/// Binds `pso` + `set` and dispatches `(x, y, z)` groups (no push).
fn bind_dispatch(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pso: &ComputePso,
    set: vk::DescriptorSet,
    x: u32,
    y: u32,
    z: u32,
) {
    // SAFETY: the ash seam. The PSO/set are valid for the open command buffer.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pso.handle);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pso.layout,
            0,
            &[set],
            &[],
        );
        raw.cmd_dispatch(cmd, x, y, z);
    }
}

/// Binds `pso` + `set`, pushes `push`, and dispatches `(x, y, z)` groups.
#[allow(clippy::too_many_arguments)]
fn bind_dispatch_push(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pso: &ComputePso,
    set: vk::DescriptorSet,
    push: &[u8],
    x: u32,
    y: u32,
    z: u32,
) {
    // SAFETY: the ash seam. The PSO/set/push are valid for the open command buffer; the
    // push spans the declared compute range.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pso.handle);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pso.layout,
            0,
            &[set],
            &[],
        );
        raw.cmd_push_constants(cmd, pso.layout, vk::ShaderStageFlags::COMPUTE, 0, push);
        raw.cmd_dispatch(cmd, x, y, z);
    }
}

/// Writes a `GENERAL`-layout storage image into `(set, binding)`.
fn write_storage(raw: &ash::Device, set: vk::DescriptorSet, binding: u32, view: vk::ImageView) {
    let info = [vk::DescriptorImageInfo::default()
        .image_view(view)
        .image_layout(vk::ImageLayout::GENERAL)];
    let write = [vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
        .image_info(&info)];
    // SAFETY: the ash seam. Host access at the (idle) bake point is single-threaded.
    unsafe { raw.update_descriptor_sets(&write, &[]) };
}

/// Writes a `SHADER_READ_ONLY`-layout combined image sampler into `(set, binding)`.
fn write_sampler(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    sampler: vk::Sampler,
    view: vk::ImageView,
) {
    let info = [vk::DescriptorImageInfo::default()
        .sampler(sampler)
        .image_view(view)
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
    let write = [vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&info)];
    // SAFETY: the ash seam. Host access at the (idle) bake point is single-threaded.
    unsafe { raw.update_descriptor_sets(&write, &[]) };
}

/// The visible-sky pass: a fullscreen graphics pass before the scene that fills the scene
/// color target. Procedural mode samples the IBL env cube (set 1), Texture mode a bindless
/// panorama (set 0), Color mode a flat fill.
///
/// Owns its set layout + descriptor set (allocated from the shared pool) + the fullscreen
/// PSO. The PSO bakes the sample count, so it is rebuilt on an AA change.
pub struct Sky {
    resources: Arc<DeviceResources>,
    /// 0 = Color, 1 = Texture, 2 = Procedural (matches `SkyMode`).
    pub mode: u32,
    /// Color-mode flat fill (also the sky-pass clear color).
    pub clear_color: Vec3,
    /// Overall sky intensity.
    pub intensity: f32,
    /// Yaw rotation (radians).
    pub rotation: f32,
    /// Whether the visible-sky pass runs.
    pub visible: bool,
    /// Bindless panorama slot (Texture mode).
    pub texture_index: u32,
    set_layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    /// Whether the set is written + the env cube baked.
    pub ready: bool,
}

impl Sky {
    /// Creates the sky set layout (set 1: the env cube), allocates the set from the shared
    /// pool, and builds the fullscreen PSO over the bindless set + the sky set. The bake
    /// writes the env-cube descriptor + marks [`Sky::ready`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing layout / set / pipeline step.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        sample_count: vk::SampleCountFlags,
    ) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the ash seam. Freed in `Drop`.
        let set_layout = checked(
            unsafe { raw.create_descriptor_set_layout(&layout_info, None) },
            "skySetLayout",
        )?;

        let set = match descriptors.allocate_set(set_layout) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the ash seam. Free the layout before the early return.
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };

        let (pipeline, pipeline_layout) = match build_sky_pipeline(
            device,
            descriptors.bindless_set_layout(),
            set_layout,
            sample_count,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // SAFETY: the ash seam. Free the layout (the set frees with the pool).
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };

        Ok(Self {
            resources,
            mode: 2,
            clear_color: Vec3::new(0.05, 0.06, 0.08),
            intensity: 1.0,
            rotation: 0.0,
            visible: true,
            texture_index: 0,
            set_layout,
            set,
            pipeline,
            pipeline_layout,
            ready: false,
        })
    }

    /// Folds the host-supplied [`SkyRenderSettings`] in.
    pub fn submit(&mut self, settings: &SkyRenderSettings) {
        self.mode = settings.mode;
        self.clear_color = settings.clear_color;
        self.intensity = settings.intensity;
        self.rotation = settings.rotation;
        self.visible = settings.visible;
        self.texture_index = settings.texture_index;
    }

    /// Writes the env-cube descriptor (set 1, binding 0) so the procedural-sky pass samples
    /// the same cube the IBL bake produced, then marks the sky ready. Called once after the
    /// first IBL bake.
    pub fn bind_env_cube(&mut self, ibl: &Ibl) {
        let info = [vk::DescriptorImageInfo::default()
            .sampler(ibl.sampler)
            .image_view(ibl.env_cube.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info)];
        // SAFETY: the ash seam. Host access at the (idle) bake point is single-threaded.
        unsafe { self.resources.device().update_descriptor_sets(&write, &[]) };
        self.ready = true;
    }

    /// Rebuilds the fullscreen sky PSO for a new MSAA sample count, replacing the prior one.
    /// The PSO bakes `rasterizationSamples`, so an AA change must rebuild it or the sky pass
    /// draws into the MSAA scene color with a mismatched 1× pipeline
    /// (`VUID-vkCmdDraw-multisampledRenderToSingleSampled-07285`). The caller idles the
    /// device first (the live PSO may be in flight), so the old handle is free to destroy
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the new pipeline cannot be built; the old PSO is kept on failure.
    pub fn set_sample_count(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        sample_count: vk::SampleCountFlags,
    ) -> Result<()> {
        let (pipeline, pipeline_layout) = build_sky_pipeline(
            device,
            descriptors.bindless_set_layout(),
            self.set_layout,
            sample_count,
        )?;
        let raw = self.resources.device();
        // SAFETY: the ash seam. The caller idled the device, so the old PSO + layout are no
        // longer referenced by any in-flight command buffer; destroyed exactly once here.
        unsafe {
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
        }
        self.pipeline = pipeline;
        self.pipeline_layout = pipeline_layout;
        Ok(())
    }

    /// Whether the visible-sky pass should run this frame (visible + ready).
    pub fn should_draw(&self) -> bool {
        self.visible && self.ready
    }

    /// Resolves this frame's sky draw into `Copy` handles + push data a render-graph pass
    /// body captures (never `&self`). The render-graph closure must not borrow the renderer
    /// aggregate (README §2), so the sky pass captures a [`SkyDraw`] instead.
    ///
    /// `mode_override` forces the visible-sky mode for this draw (the offscreen thumbnail view
    /// passes the studio-gradient mode so the backdrop is independent of the IBL); `None` uses the
    /// submitted scene mode.
    pub fn draw_data(
        &self,
        view_proj: saffron_geometry::glam::Mat4,
        mode_override: Option<u32>,
    ) -> SkyDraw {
        SkyDraw {
            pipeline: self.pipeline,
            layout: self.pipeline_layout,
            set: self.set,
            push: SkyPush {
                inv_view_proj: view_proj.inverse(),
                params: Vec4::new(
                    self.intensity,
                    self.rotation,
                    mode_override.unwrap_or(self.mode) as f32,
                    self.texture_index as f32,
                ),
                clear_color: self.clear_color.extend(1.0),
            },
        }
    }
}

/// The resolved fullscreen-sky draw a render-graph pass body captures: the PSO, its layout,
/// the env-cube set, and the per-frame push. All `Copy`, so the `'static` closure holds no
/// borrow of the renderer (README §2).
#[derive(Clone, Copy)]
pub struct SkyDraw {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set: vk::DescriptorSet,
    push: SkyPush,
}

/// Records the fullscreen sky into `cmd`: bind the bindless array (set 0) + the env-cube set
/// (set 1), push the inverse view-projection + sky params, draw one fullscreen triangle. The
/// graph sets the dynamic viewport/scissor.
pub fn record_sky(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    bindless_set: vk::DescriptorSet,
    draw: &SkyDraw,
) {
    // SAFETY: the ash seam. The PSO/sets are valid for the open pass; the push spans the
    // declared fragment range; the draw is a single vertexless fullscreen triangle.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, draw.pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            draw.layout,
            0,
            &[bindless_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            draw.layout,
            1,
            &[draw.set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            draw.layout,
            vk::ShaderStageFlags::FRAGMENT,
            0,
            bytemuck::bytes_of(&draw.push),
        );
        raw.cmd_draw(cmd, 3, 1, 0, 0);
    }
}

impl Drop for Sky {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The device idled before teardown; the pipeline + its layout
        // + the set layout are freed exactly once. The set frees with the shared pool.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// One captured + prefiltered local reflection probe. Mirrors the [`Ibl`] cube layout but
/// per-probe: a captured env cube + 6 face render views + a depth scratch, convolved into a
/// per-probe irradiance + prefiltered cube. Sampled via the IBL set (bindings 3-4) at
/// this slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflectionProbe {
    /// World-space origin (the entity translation).
    pub origin: Vec3,
    /// Influence radius (world units).
    pub influence_radius: f32,
    /// Specular intensity multiplier.
    pub intensity: f32,
    /// Box-projection (parallax-corrected) reflections.
    pub box_projection: bool,
    /// Box half-extents.
    pub box_extent: Vec3,
    /// Owning entity id (the capture re-uses the slot when re-armed).
    pub entity: u64,
    /// Cubes created (the lazy per-slot allocation).
    pub allocated: bool,
    /// Captured + written into the IBL set at least once (else the slot resolves to the
    /// global IBL cubes).
    pub valid: bool,
    /// (Re)capture pending this frame.
    pub dirty: bool,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            influence_radius: 10.0,
            intensity: 1.0,
            box_projection: false,
            box_extent: Vec3::splat(10.0),
            entity: 0,
            allocated: false,
            valid: false,
            dirty: false,
        }
    }
}

/// The reflection-probe array + the metadata SSBO (IBL set bindings 3-5) + the per-frame
/// capture state. Every array slot is seeded with the global IBL cubes so the bind is
/// always valid; real probes overwrite
/// their slot on capture.
pub struct ReflectionProbes {
    resources: Arc<DeviceResources>,
    probes: [ReflectionProbe; MAX_REFLECTION_PROBES as usize],
    count: u32,
    /// The IBL set (set 3; probes live at bindings 3-5). Shared with [`Ibl::set`].
    mesh_set: vk::DescriptorSet,
    sampler: vk::Sampler,
    meta_buffer: Buffer,
    /// Master probe toggle.
    pub use_probes: bool,
    /// Any probe dirty this frame → capture at the next idle point.
    pub capture_pending: bool,
    warned_overflow: bool,
    frame_probe_count: u32,
}

impl ReflectionProbes {
    /// Allocates the per-probe metadata SSBO, the probe sampler, and seeds the meta buffer
    /// to zero. The `mesh_set` is the shared [`Ibl::set`]; seeding the array slots happens
    /// after the first IBL bake via [`ReflectionProbes::seed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing buffer/sampler step.
    pub fn new(device: &Device, mesh_set: vk::DescriptorSet) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        let sampler = create_ibl_sampler(raw)?;
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let size = (size_of::<ProbeMetaGpu>() * MAX_REFLECTION_PROBES as usize) as vk::DeviceSize;
        let mut meta_buffer = match Buffer::new(
            &resources,
            size,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &alloc_info,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                // SAFETY: the ash seam. Free the sampler on the early return.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };
        if let Some(dst) = meta_buffer.mapped_bytes() {
            dst.fill(0);
        }

        Ok(Self {
            resources,
            probes: std::array::from_fn(|_| ReflectionProbe::default()),
            count: 0,
            mesh_set,
            sampler,
            meta_buffer,
            use_probes: true,
            capture_pending: false,
            warned_overflow: false,
            frame_probe_count: 0,
        })
    }

    /// Seeds every probe array slot (IBL set bindings 3/4) with the global IBL cubes and
    /// writes the metadata-SSBO binding (5), so the mesh bind is valid before any capture.
    /// Called once after the first IBL bake.
    pub fn seed(&self, ibl: &Ibl) {
        self.seed_set(self.mesh_set, ibl);
    }

    /// Seeds an arbitrary IBL set's probe bindings — the shared [`Ibl::set`] for the project IBL
    /// (via [`ReflectionProbes::seed`]), or a secondary set (the offscreen thumbnail preview IBL)
    /// whose bindings 3/4/5 must be valid before it is bound even though probes are never captured
    /// into it. The fallback cubes come from `ibl`; binding 5 shares the one metadata SSBO (a
    /// secondary set carries no probes, so it is read for `probe_count == 0`, i.e. never).
    pub fn seed_set(&self, dst_set: vk::DescriptorSet, ibl: &Ibl) {
        let raw = self.resources.device();
        for slot in 0..MAX_REFLECTION_PROBES {
            self.write_slot(raw, dst_set, ibl, slot as usize);
        }
        let buffer_info = [vk::DescriptorBufferInfo::default()
            .buffer(self.meta_buffer.handle())
            .offset(0)
            .range(self.meta_buffer.size())];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(dst_set)
            .dst_binding(5)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffer_info)];
        // SAFETY: the ash seam. Host access at the (idle) post-bake point is single-threaded.
        unsafe { raw.update_descriptor_sets(&write, &[]) };
    }

    /// Writes one probe slot's prefiltered (binding 3) + irradiance (binding 4) cube into
    /// the IBL set. A slot with no captured probe falls back to the global IBL cubes, so
    /// every array element is always valid.
    fn write_slot(&self, raw: &ash::Device, dst_set: vk::DescriptorSet, ibl: &Ibl, slot: usize) {
        // There is no per-slot cube storage — every slot resolves to the global IBL cubes
        // (a real capture overwrites the slot via `write_captured`).
        let pre = ibl.prefiltered_cube.view;
        let irr = ibl.irradiance_cube.view;
        let infos = [
            vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(pre)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(irr)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(3)
                .dst_array_element(slot as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&infos[0])),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(4)
                .dst_array_element(slot as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&infos[1])),
        ];
        // SAFETY: the ash seam. Host access at the (idle) post-bake point is single-threaded.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// Folds the host's per-frame probe uploads in:
    /// re-arms a slot on a real change (new/moved/resized probe, or an explicit dirty flag),
    /// drops removed slots, and re-uploads the metadata SSBO. Overflow past
    /// `MAX_REFLECTION_PROBES` is logged once.
    pub fn submit(&mut self, uploads: &[ReflectionProbeUpload]) {
        let cap = MAX_REFLECTION_PROBES as usize;
        let mut count = uploads.len();
        if count > cap {
            if !self.warned_overflow {
                tracing::warn!("more than {cap} reflection probes — excess ignored");
                self.warned_overflow = true;
            }
            count = cap;
        }

        let mut any_dirty = false;
        for (probe, up) in self.probes.iter_mut().zip(&uploads[..count]) {
            let slot_changed = probe.entity != up.entity
                || probe.origin != up.origin
                || probe.influence_radius != up.influence_radius;
            if up.dirty || slot_changed || !probe.valid {
                probe.dirty = true;
                any_dirty = true;
            }
            probe.entity = up.entity;
            probe.origin = up.origin;
            probe.influence_radius = up.influence_radius;
            probe.intensity = up.intensity;
            probe.box_projection = up.box_projection;
            probe.box_extent = up.box_extent;
        }
        for probe in &mut self.probes[count..] {
            probe.entity = 0;
            probe.valid = false;
            probe.dirty = false;
        }
        self.count = count as u32;
        if any_dirty {
            self.capture_pending = true;
        }
        self.upload_meta();
    }

    /// Re-uploads the metadata SSBO (cheap; the shader reads only `count` records). The
    /// sampled count is 0 when probes are disabled, so the mesh fragment ignores them.
    fn upload_meta(&mut self) {
        let sample_count = if self.use_probes { self.count } else { 0 };
        let mut meta = [ProbeMetaGpu::zeroed(); MAX_REFLECTION_PROBES as usize];
        for (slot, probe) in meta
            .iter_mut()
            .zip(&self.probes)
            .take(sample_count as usize)
        {
            *slot = ProbeMetaGpu {
                origin_radius: probe.origin.extend(probe.influence_radius),
                extent_intensity: probe.box_extent.extend(probe.intensity),
                flags: UVec4::new(
                    u32::from(probe.valid),
                    u32::from(probe.box_projection),
                    0,
                    0,
                ),
            };
        }
        if let Some(dst) = self.meta_buffer.mapped_bytes() {
            let bytes = bytemuck::bytes_of(&meta);
            dst[..bytes.len()].copy_from_slice(bytes);
        }
        self.frame_probe_count = sample_count;
    }

    /// The reflection-probe count the mesh fragment iterates this frame (0 when probes are
    /// disabled). The renderer folds this into the light UBO's `ambientColor.w`.
    pub fn frame_probe_count(&self) -> u32 {
        self.frame_probe_count
    }

    /// The active probe-slot count (≤ `MAX_REFLECTION_PROBES`).
    pub fn count(&self) -> u32 {
        self.count
    }

    /// The captured reflection probes (origin / radius / intensity / validity), in slot
    /// order — the `list-probes` control command's source.
    pub fn probes(&self) -> &[ReflectionProbe] {
        &self.probes[..self.count as usize]
    }
}

impl Drop for ReflectionProbes {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The device idled before teardown; the sampler is freed once.
        // The meta buffer frees via its Drop; the set frees with the shared pool.
        unsafe {
            self.resources.device().destroy_sampler(self.sampler, None);
        }
    }
}

/// Builds the fullscreen sky PSO from `sky.slang`: no vertex input, a triangle-list
/// fullscreen triangle, no depth test/write, the scene color format, sets 0 (bindless) + 1
/// (env cube), the `SkyPush` in the fragment stage, the scene's sample count baked in.
/// Returns `(pipeline, layout)`.
fn build_sky_pipeline(
    device: &Device,
    bindless_layout: vk::DescriptorSetLayout,
    sky_layout: vk::DescriptorSetLayout,
    sample_count: vk::SampleCountFlags,
) -> Result<(vk::Pipeline, vk::PipelineLayout)> {
    let raw = device.raw();
    let dir = crate::pipelines::resolve_shader_dir();
    let path = dir.join("sky.spv");
    let bytes = std::fs::read(&path)
        .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}' ({} bytes)",
            path.display(),
            bytes.len()
        )));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: the ash seam. The module is freed after pipeline creation.
    let module = checked(
        unsafe { raw.create_shader_module(&module_info, None) },
        "sky shader module",
    )?;

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
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(sample_count);
    let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(false)
        .depth_write_enable(false);
    let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(false)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let color_blend =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_formats = [crate::pipelines::OFFSCREEN_COLOR_FORMAT];
    let mut rendering_info =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&color_formats);

    let push_constant = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(size_of::<SkyPush>() as u32)];
    let set_layouts = [bindless_layout, sky_layout];
    let layout_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant);
    // SAFETY: the ash seam. The set layouts outlive the call; the layout is returned.
    let layout = match checked(
        unsafe { raw.create_pipeline_layout(&layout_info, None) },
        "createPipelineLayout (sky)",
    ) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the module on the early return.
            unsafe { raw.destroy_shader_module(module, None) };
            return Err(err);
        }
    };

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
    // SAFETY: the ash seam. On failure the layout + module are freed.
    let created =
        unsafe { raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None) };
    // SAFETY: the ash seam. The module is consumed by creation; free it now.
    unsafe { raw.destroy_shader_module(module, None) };
    let pipeline = match created {
        Ok(pipelines) => pipelines[0],
        Err((_, result)) => {
            // SAFETY: the ash seam. The layout was created above; freed once here.
            unsafe { raw.destroy_pipeline_layout(layout, None) };
            return Err(Error::Vk {
                context: "create_graphics_pipelines (sky)",
                result,
            });
        }
    };
    Ok((pipeline, layout))
}

/// The IBL linear/clamp/mipped sampler — all three cubes + the LUT sample through it.
/// `eClampToEdge` so cube faces do not seam.
fn create_ibl_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .max_lod(vk::LOD_CLAMP_NONE);
    // SAFETY: the ash seam. The sampler is owned by `Ibl` and freed in its Drop.
    checked(
        unsafe { raw.create_sampler(&info, None) },
        "createSampler (ibl)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::Descriptors;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use std::sync::Mutex;

    /// Builds a headless device + descriptors or skips (no Vulkan ICD in this toolbox).
    fn device_or_skip() -> Option<(Device, Descriptors)> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = match Descriptors::new(&device, &free_list) {
            Ok(descriptors) => descriptors,
            Err(err) => {
                eprintln!("skipping: descriptors unbuildable ({err})");
                return None;
            }
        };
        Some((device, descriptors))
    }

    /// `SkygenParams` equality gates the re-bake: identical params (and the same source)
    /// arm nothing; a changed sun direction arms a re-bake. The pure gate, no device.
    #[test]
    fn skygen_params_equality_gates_rebake() {
        let baked = SkygenParams::default();

        // Identical params + same source → no re-bake.
        assert!(
            !should_rebake(
                EnvSource::Procedural,
                EnvSource::Procedural,
                &baked,
                &baked,
                false,
            ),
            "identical inputs must not arm a re-bake"
        );

        // A changed sun direction → re-bake armed (Procedural reads the sun).
        let mut moved = baked;
        moved.sun_dir = Vec3::new(-0.2, 0.8, 0.5);
        assert!(
            should_rebake(
                EnvSource::Procedural,
                EnvSource::Procedural,
                &moved,
                &baked,
                false,
            ),
            "a changed sun direction must arm a re-bake"
        );

        // A changed sun intensity / color → re-bake armed.
        let mut brighter = baked;
        brighter.sun_intensity = 2.0;
        assert!(should_rebake(
            EnvSource::Procedural,
            EnvSource::Procedural,
            &brighter,
            &baked,
            false,
        ));

        // A source switch alone → re-bake armed even with identical sky params.
        assert!(should_rebake(
            EnvSource::Atmosphere,
            EnvSource::Procedural,
            &baked,
            &baked,
            false,
        ));

        // An atmosphere-param change matters only for the Atmosphere source.
        let mut atmos = baked;
        atmos.atmosphere.mie_anisotropy = 0.5;
        assert!(
            should_rebake(
                EnvSource::Atmosphere,
                EnvSource::Atmosphere,
                &atmos,
                &baked,
                false,
            ),
            "an atmosphere-param change must arm a re-bake under the Atmosphere source"
        );
        assert!(
            !should_rebake(
                EnvSource::Procedural,
                EnvSource::Procedural,
                &atmos,
                &baked,
                false,
            ),
            "an atmosphere-param change must NOT arm a re-bake under the Procedural source"
        );
    }

    /// `request_env_bake` arms `rebake_pending` only on a real change, matching the gate.
    /// Device-backed (the full `Ibl` owns GPU handles); skipped without an ICD.
    #[test]
    fn request_env_bake_arms_only_on_change() {
        let Some((device, descriptors)) = device_or_skip() else {
            return;
        };
        let mut ibl = Ibl::new(&device, &descriptors).expect("ibl init");
        // The renderer's construction runs the first (procedural) bake right after `new`,
        // committing the default params as `baked`.
        ibl.bake(&device, true).expect("first bake");
        assert!(ibl.ready, "first bake must mark IBL ready");

        // Re-requesting the identical params arms nothing.
        ibl.request_env_bake(EnvSource::Procedural, None, SkygenParams::default());
        assert!(
            !ibl.rebake_pending,
            "identical request must not arm a re-bake"
        );

        // A changed sun arms the re-bake.
        let moved = SkygenParams {
            sun_dir: Vec3::new(0.1, 0.9, -0.4),
            ..SkygenParams::default()
        };
        ibl.request_env_bake(EnvSource::Procedural, None, moved);
        assert!(ibl.rebake_pending, "a changed sun must arm the re-bake");
    }

    /// The probe array seeds all 8 slots with the global IBL cubes at init: a validation-
    /// clean `seed` over the full `MAX_REFLECTION_PROBES` array proves every slot binds a
    /// valid cube before any capture. Device-backed; skipped without an ICD.
    #[test]
    fn probe_array_seeds_all_slots_validation_clean() {
        let Some((device, descriptors)) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let ibl = Ibl::new(&device, &descriptors).expect("ibl init");
        let reflection = ReflectionProbes::new(&device, ibl.set()).expect("probes init");
        // Seeds bindings 3 (prefiltered ×8) + 4 (irradiance ×8) + 5 (meta SSBO); a bad
        // slot or array element would trip a validation error.
        reflection.seed(&ibl);
        device.wait_idle().expect("idle");
        let after = validation_issue_count();
        assert_eq!(
            before, after,
            "seeding all {MAX_REFLECTION_PROBES} probe slots must be validation-clean"
        );
        assert_eq!(reflection.count(), 0, "no probes submitted yet");
    }

    /// `EnvSource` round-trips through the bake dispatch for all three variants: each baked
    /// source completes a validation-clean convolution chain (Procedural skygen, Equirect
    /// fallback-to-procedural with no panorama, Atmosphere LUT chain). Device-backed;
    /// skipped without an ICD.
    #[test]
    fn env_source_round_trips_through_bake() {
        let Some((device, descriptors)) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let mut ibl = Ibl::new(&device, &descriptors).expect("ibl init (procedural)");

        // Procedural: the first bake already ran in `new`; a re-bake exercises the
        // overwrite-in-place path (UNDEFINED→GENERAL discards, then convolve).
        ibl.request_env_bake(
            EnvSource::Procedural,
            None,
            SkygenParams {
                sun_dir: Vec3::new(0.3, 0.7, 0.6),
                ..SkygenParams::default()
            },
        );
        assert!(ibl.rebake_pending);
        ibl.fire_rebake(&device).expect("procedural re-bake");

        // Equirect with no panorama degrades to procedural — the bake must still succeed.
        ibl.request_env_bake(
            EnvSource::Equirect,
            None,
            SkygenParams {
                sun_dir: Vec3::new(-0.3, 0.7, 0.6),
                ..SkygenParams::default()
            },
        );
        ibl.fire_rebake(&device)
            .expect("equirect (fallback) re-bake");

        // Atmosphere: the Hillaire LUT chain (transmittance → multiscatter → skyview →
        // skygen) feeding the env cube.
        let mut atmos = SkygenParams {
            sun_dir: Vec3::new(0.0, 0.2, 1.0),
            ..SkygenParams::default()
        };
        atmos.atmosphere.enabled = true;
        ibl.request_env_bake(EnvSource::Atmosphere, None, atmos);
        ibl.fire_rebake(&device).expect("atmosphere re-bake");

        device.wait_idle().expect("idle");
        let after = validation_issue_count();
        assert_eq!(
            before, after,
            "all three EnvSource bakes must be validation-clean"
        );
    }

    /// `ProbeMetaGpu` matches the std430 layout the mesh fragment reads (48 bytes, three
    /// 16-byte blocks). A wrong offset corrupts the probe sampling, not a compile error.
    #[test]
    fn probe_meta_std430_layout() {
        assert_eq!(size_of::<ProbeMetaGpu>(), 48);
        assert_eq!(std::mem::offset_of!(ProbeMetaGpu, origin_radius), 0);
        assert_eq!(std::mem::offset_of!(ProbeMetaGpu, extent_intensity), 16);
        assert_eq!(std::mem::offset_of!(ProbeMetaGpu, flags), 32);
    }

    /// `submit` arms a capture on a dirty/new probe, drops removed slots, and uploads the
    /// metadata SSBO with the correct sampled count. Device-backed (the meta buffer is GPU
    /// memory); skipped without an ICD.
    #[test]
    fn submit_reflection_probes_tracks_dirty_and_count() {
        let Some((device, descriptors)) = device_or_skip() else {
            return;
        };
        let ibl = Ibl::new(&device, &descriptors).expect("ibl init");
        let mut reflection = ReflectionProbes::new(&device, ibl.set()).expect("probes init");

        let probe = ReflectionProbeUpload {
            entity: 7,
            origin: Vec3::new(1.0, 2.0, 3.0),
            ..ReflectionProbeUpload::default()
        };
        reflection.submit(&[probe]);
        assert_eq!(reflection.count(), 1);
        assert!(reflection.capture_pending, "a new probe must arm a capture");
        assert_eq!(reflection.frame_probe_count(), 1);

        // Disabling probes zeroes the sampled count even with an active slot.
        reflection.use_probes = false;
        reflection.submit(&[probe]);
        assert_eq!(
            reflection.frame_probe_count(),
            0,
            "disabled probes contribute zero samples"
        );

        // Overflow past the cap is clamped (logged once).
        reflection.use_probes = true;
        let many = vec![ReflectionProbeUpload::default(); (MAX_REFLECTION_PROBES + 4) as usize];
        reflection.submit(&many);
        assert_eq!(reflection.count(), MAX_REFLECTION_PROBES);
    }
}
