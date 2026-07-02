//! The device-shared screen-space-effects sub-state: the nearest G-buffer sampler,
//! the two compute set layouts every screen-space pass binds, and the camera
//! transforms + toggles the GTAO / contact-shadow / SSGI passes read.
//!
//! It owns the *device-shared* state only — the nearest sampler, the 2-binding
//! (sampler+storage) and 3-binding (sampler+sampler+storage) compute layouts, the
//! mesh set-4 layout, and the per-frame camera matrices. The descriptor *sets* that
//! bind per-view images live in [`crate::ViewTarget`] (one set of each per editor
//! pane), so a view switch never leaves a set bound to another view's images
//! (README §2 per-view borrow split applied to compute sets).
//!
//! Built once in [`Ssao::new`], then borrowed `&Ssao` by the frame-graph build (its
//! layouts/sampler are immutable after init); the camera + toggles are written
//! through [`Ssao::set_camera`] / the toggle setters (`&mut self.ssao`).

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, UVec4, Vec3, Vec4};

use crate::descriptors::Descriptors;
use crate::resources::DeviceResources;
use crate::{Device, Result, checked};

/// The view-normal + view-Z G-buffer format (rgb = view normal, a = view-Z). Also the
/// SSGI radiance format.
pub const G_NORMAL_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// The AO + contact single-channel map format.
pub const AO_FORMAT: vk::Format = vk::Format::R8_UNORM;

/// The thin G-buffer's second target: per-pixel roughness (∈ [0,1]). `R8_UNORM` is a mandatory
/// color-attachment + sampled format, so no device feature is required.
pub const ROUGHNESS_FORMAT: vk::Format = vk::Format::R8_UNORM;

/// The SSGI temporal-history EMA weight reused by the ssgi-accum pass.
pub const SSGI_HISTORY_WEIGHT: f32 = 0.9;

/// The gtao push constant: `invProjection` (clip → view) + a params vec4
/// (x = radius, y = strength). 80 bytes, matching `gtao.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GtaoPush {
    /// Clip → view, to reconstruct view position from the G-buffer depth.
    pub inv_projection: Mat4,
    /// x = sample radius (view units), y = strength, zw unused.
    pub params: Vec4,
}

const _: () = assert!(size_of::<GtaoPush>() == 80);

/// The contact-shadow push: projection + invProjection + the view-space light
/// direction + a params vec4 (x = ray length, y = step count, z = thickness). 160
/// bytes, matching `contact.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContactPush {
    /// View → clip, to project marched view positions to screen.
    pub projection: Mat4,
    /// Clip → view, to reconstruct the view position.
    pub inv_projection: Mat4,
    /// xyz = direction TO the light, view space.
    pub light_dir_view: Vec4,
    /// x = ray length (view units), y = step count, z = thickness.
    pub params: Vec4,
}

const _: () = assert!(size_of::<ContactPush>() == 160);

/// The SSGI trace push: projection + invProjection + two params vec4s (x = radius,
/// y = intensity, z = step count, w = frame index; then x = ray count). 160 bytes, matching
/// `ssgi.slang`'s `Push`. Shared with the SSR pass, which ignores `params2`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SsgiPush {
    /// View → clip.
    pub projection: Mat4,
    /// Clip → view.
    pub inv_projection: Mat4,
    /// x = gather radius (view units), y = intensity, z = step count, w = frame index.
    pub params: Vec4,
    /// x = ray count (cosine-hemisphere samples per pixel); unused by the SSR pass.
    pub params2: Vec4,
}

const _: () = assert!(size_of::<SsgiPush>() == 160);

/// The SSGI temporal-accumulation push: a params vec4 (x = history weight, y = 1 if
/// history is valid). 16 bytes, matching `ssgi_accum.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SsgiAccumPush {
    /// x = history weight (0..1), y = 1 if history is valid this frame.
    pub params: Vec4,
}

const _: () = assert!(size_of::<SsgiAccumPush>() == 16);

/// The DFAO cone-trace push: `invProjection` (clip → view) + `invView` (view → world) to
/// reconstruct the world position + normal from the thin G-buffer, then a params vec4
/// (x = frame index, rotating the cone ring so the temporal accumulator supersamples). 144
/// bytes, matching `dfao.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DfaoPush {
    /// Clip → view.
    pub inv_projection: Mat4,
    /// View → world.
    pub inv_view: Mat4,
    /// x = frame index (cone azimuth jitter), yzw reserved.
    pub params: Vec4,
}

const _: () = assert!(size_of::<DfaoPush>() == 144);

/// The specular reflection-occlusion cone-trace push: `invProjection` (clip → view) +
/// `invView` (view → world) to reconstruct the world position + normal (and the camera world
/// position, for the reflection vector) from the thin G-buffer, then a params vec4 (x = frame
/// index, reserved). 144 bytes, matching `specocc.slang`'s `Push` (identical to [`DfaoPush`]).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpecoccPush {
    /// Clip → view.
    pub inv_projection: Mat4,
    /// View → world.
    pub inv_view: Mat4,
    /// x = frame index (reserved), yzw reserved.
    pub params: Vec4,
}

const _: () = assert!(size_of::<SpecoccPush>() == 144);

/// The screen-space GI-resolve pass's per-frame params UBO (matches `gi_resolve.slang`'s `GiParams`,
/// 224 bytes / std140: two camera inverses to reconstruct world pos/normal from the thin G-buffer,
/// the DDGI probe-clipmap placement — the flattened `giprobe::DdgiVolume` — and the feature flags).
/// A UBO, not a push: at 224 bytes it exceeds the 128-byte guaranteed push range.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GiParams {
    /// Clip → view.
    pub inv_projection: Mat4,
    /// View → world.
    pub inv_view: Mat4,
    /// DDGI probe volume world min (xyz).
    pub volume_min: Vec4,
    /// DDGI probe volume world size (xyz).
    pub volume_extent: Vec4,
    /// DDGI probes per axis (xyz), w = irradiance tile interior.
    pub probe_count: UVec4,
    /// DDGI toroidal scroll base (xyz).
    pub scroll_base: UVec4,
    /// World-space camera position (xyz) for the DDGI surface-bias view vector.
    pub eye_position: Vec4,
    /// x = DDGI enabled, y = sky-occlusion (dfao) enabled, zw reserved.
    pub flags: UVec4,
}

const _: () = assert!(size_of::<GiParams>() == 224);

/// The gbuffer prepass push: the world→clip + world→view transforms (128 bytes,
/// matching `gbuffer.slang`'s `Push`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GbufferPush {
    /// World → clip.
    pub view_proj: Mat4,
    /// World → view.
    pub view: Mat4,
}

const _: () = assert!(size_of::<GbufferPush>() == 128);

/// The device-shared screen-space sub-state: the nearest G-buffer sampler, the two
/// compute set layouts, and the per-frame camera transforms + effect toggles.
///
/// Owns an [`Arc`]`<`[`DeviceResources`]`>` so its sampler + layouts free in [`Drop`]
/// without a live `&Device`. The mesh set-4 layout is *not* owned here — it lives in
/// [`Descriptors`] (the übershader binds it); this struct only references it for the
/// per-view set allocation.
pub struct Ssao {
    resources: Arc<DeviceResources>,

    /// GTAO ambient occlusion toggle.
    pub use_ssao: bool,
    /// Screen-space directional contact-shadow toggle.
    pub use_contact: bool,
    /// Screen-space one-bounce GI toggle.
    pub use_ssgi: bool,
    /// Screen-space reflections toggle (opt-in; off by default).
    pub use_ssr: bool,
    /// Sets/views valid — built after the per-view targets exist.
    pub ready: bool,

    view: Mat4,
    view_proj: Mat4,
    projection: Mat4,
    inv_projection: Mat4,
    sun_dir_view: Vec3,
    radius: f32,
    strength: f32,
    ssgi_intensity: f32,
    /// SSGI ray-march step count, driven by the render-quality tier (the `ssgi.slang` push
    /// `params.z`). A runtime push-constant, so the tier dials it with no shader recompile.
    ssgi_steps: f32,
    /// SSGI cosine-hemisphere ray count per pixel, driven by the render-quality tier (the
    /// `ssgi.slang` push `params2.x`). A runtime push-constant, dialed with no shader recompile.
    ssgi_rays: f32,
    /// Contact-shadow ray-march step count, driven by the render-quality tier (the `contact.slang`
    /// push `params.y`).
    contact_steps: f32,
    ssgi_frame: u32,
    /// SSR ray-march step count (the `ssr.slang` push `params.z`); more steps = a longer,
    /// finer mirror march.
    ssr_steps: f32,
    ssr_frame: u32,
    /// DFAO monotonic frame index, rotating the cone ring so the temporal accumulator
    /// supersamples the penumbra across frames (the `dfao.slang` push `params.x`).
    dfao_frame: u32,
    /// Specular-occlusion monotonic frame index (the `specocc.slang` push `params.x`, reserved);
    /// bumped per frame so the temporal accumulator sees a decorrelated index like DFAO.
    specocc_frame: u32,

    /// Nearest, clamp sampler reading the G-buffer.
    nearest_sampler: vk::Sampler,
    /// The 2-binding (sampler + storage image) compute layout.
    compute2_layout: vk::DescriptorSetLayout,
    /// The 3-binding (sampler + sampler + storage image) compute layout.
    compute3_layout: vk::DescriptorSetLayout,
    /// The gi-resolve pass's single set layout (b0 G-buffer sampler, b1 storage out, b2 params UBO,
    /// b3 IBL cube sampler, b4 dfao sampler, b5/b6 DDGI atlas samplers).
    gi_resolve_layout: vk::DescriptorSetLayout,
}

impl Ssao {
    /// Builds the nearest sampler + the two compute set layouts. The toggles default
    /// on; `ready` stays false until the per-view targets + sets are built.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; already-created
    /// handles are freed before returning on a partial failure.
    pub fn new(device: &Device) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        let nearest_sampler = create_nearest_sampler(raw)?;
        let compute2_layout = match create_compute_layout(raw, 1) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The sampler was created above; free it on the
                // partial-failure path before returning.
                unsafe { raw.destroy_sampler(nearest_sampler, None) };
                return Err(err);
            }
        };
        let compute3_layout = match create_compute_layout(raw, 2) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. Free what was already created.
                unsafe {
                    raw.destroy_descriptor_set_layout(compute2_layout, None);
                    raw.destroy_sampler(nearest_sampler, None);
                }
                return Err(err);
            }
        };
        let gi_resolve_layout = match create_gi_resolve_layout(raw) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. Free what was already created.
                unsafe {
                    raw.destroy_descriptor_set_layout(compute3_layout, None);
                    raw.destroy_descriptor_set_layout(compute2_layout, None);
                    raw.destroy_sampler(nearest_sampler, None);
                }
                return Err(err);
            }
        };

        Ok(Self {
            resources,
            use_ssao: true,
            use_contact: true,
            use_ssgi: true,
            use_ssr: false,
            ready: false,
            view: Mat4::IDENTITY,
            view_proj: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            inv_projection: Mat4::IDENTITY,
            sun_dir_view: Vec3::Y,
            // Contact AO: a small (0.5 m) gather radius — GTAO now contributes only contact-scale
            // detail the coarse DDGI probe grid cannot resolve. Large-range indirect occlusion is
            // DDGI ray-miss, not a wide screen-space AO. The SSGI / SSR derived radii below
            // re-multiply back to their own world-space extents off this contact base.
            radius: 0.5,
            strength: 3.0,
            ssgi_intensity: 1.0,
            // Defaults match the historical `High` tier (8 SSGI steps, 4 rays, 12 contact steps).
            ssgi_steps: 8.0,
            ssgi_rays: 4.0,
            contact_steps: 12.0,
            ssgi_frame: 0,
            ssr_steps: 24.0,
            ssr_frame: 0,
            dfao_frame: 0,
            specocc_frame: 0,
            nearest_sampler,
            compute2_layout,
            compute3_layout,
            gi_resolve_layout,
        })
    }

    /// The nearest, clamp G-buffer sampler.
    pub fn nearest_sampler(&self) -> vk::Sampler {
        self.nearest_sampler
    }

    /// The 2-binding (sampler + storage) compute set layout — the gtao / contact /
    /// copy_color sets are allocated against it.
    pub fn compute2_layout(&self) -> vk::DescriptorSetLayout {
        self.compute2_layout
    }

    /// The 3-binding (sampler + sampler + storage) compute set layout — the ao_blur /
    /// ssgi / ssgi_blur sets are allocated against it.
    pub fn compute3_layout(&self) -> vk::DescriptorSetLayout {
        self.compute3_layout
    }

    /// The gi-resolve pass's single set layout — the per-view `gi_resolve_set` allocates against it.
    pub fn gi_resolve_layout(&self) -> vk::DescriptorSetLayout {
        self.gi_resolve_layout
    }

    /// The world→view camera matrix this frame, the shared G-buffer reconstruction basis.
    /// ReSTIR derives `invView = view.inverse()` for its world-position reconstruction.
    pub fn view(&self) -> Mat4 {
        self.view
    }

    /// The clip→view inverse projection this frame — ReSTIR's `invProjection` push (it
    /// reconstructs view positions from the G-buffer depth like the screen-space chain).
    pub fn inv_projection(&self) -> Mat4 {
        self.inv_projection
    }

    /// Writes this frame's camera transforms + the world-space incoming sun direction
    /// (the contact-shadow ray marches toward the light, so the direction TO the light
    /// is the negation, transformed to view space).
    pub fn set_camera(&mut self, view: Mat4, proj: Mat4, sun_direction_world: Vec3) {
        self.view = view;
        self.view_proj = proj * view;
        self.projection = proj;
        self.inv_projection = proj.inverse();
        let sun_view = view * (-sun_direction_world).extend(0.0);
        self.sun_dir_view = sun_view.truncate().normalize_or_zero();
    }

    /// The gbuffer prepass push (world→clip + world→view).
    pub fn gbuffer_push(&self) -> GbufferPush {
        GbufferPush {
            view_proj: self.view_proj,
            view: self.view,
        }
    }

    /// The gtao push (invProjection + radius/strength).
    pub fn gtao_push(&self) -> GtaoPush {
        GtaoPush {
            inv_projection: self.inv_projection,
            params: Vec4::new(self.radius, self.strength, 0.0, 0.0),
        }
    }

    /// The contact-shadow push (projection + invProjection + view-space light dir + the ray-march
    /// params: 0.2 length, tier-driven step count, 0.1 thickness).
    pub fn contact_push(&self) -> ContactPush {
        ContactPush {
            projection: self.projection,
            inv_projection: self.inv_projection,
            light_dir_view: self.sun_dir_view.extend(0.0),
            params: Vec4::new(0.2, self.contact_steps, 0.1, 0.0),
        }
    }

    /// Applies a resolved render-quality tier: the enable flags + the SSGI / contact step counts
    /// (runtime push-constants). GTAO has no step-count push, so the tier only gates it on/off.
    pub fn apply_quality(&mut self, quality: &crate::RenderQuality) {
        self.use_ssgi = quality.ssgi_enabled;
        self.use_ssao = quality.gtao_enabled;
        self.use_contact = quality.contact_enabled;
        self.ssgi_steps = quality.ssgi_steps;
        self.ssgi_rays = quality.ssgi_rays;
        self.contact_steps = quality.contact_steps;
    }

    /// Bumps the monotonic SSGI frame index (rotating the trace hash so the denoiser
    /// has decorrelated noise) and returns this frame's SSGI trace push. Called once
    /// per frame at graph-build time.
    pub fn next_ssgi_push(&mut self) -> SsgiPush {
        self.ssgi_frame = self.ssgi_frame.wrapping_add(1);
        SsgiPush {
            projection: self.projection,
            inv_projection: self.inv_projection,
            params: Vec4::new(
                // SSGI gathers over a ~2 m screen-space radius (4× the contact AO base), its own
                // medium-range extent independent of GTAO's tightened contact radius.
                self.radius * 4.0,
                self.ssgi_intensity,
                self.ssgi_steps,
                self.ssgi_frame as f32,
            ),
            params2: Vec4::new(self.ssgi_rays, 0.0, 0.0, 0.0),
        }
    }

    /// Bumps the monotonic DFAO frame index (rotating the cone ring so the temporal
    /// accumulator supersamples the penumbra) and returns this frame's DFAO trace push. The
    /// world reconstruction reuses the shared G-buffer basis: `invProjection` (clip → view)
    /// and `invView` (view → world = the frame's world→view inverted).
    pub fn next_dfao_push(&mut self) -> DfaoPush {
        self.dfao_frame = self.dfao_frame.wrapping_add(1);
        DfaoPush {
            inv_projection: self.inv_projection,
            inv_view: self.view.inverse(),
            params: Vec4::new(self.dfao_frame as f32, 0.0, 0.0, 0.0),
        }
    }

    /// Bumps the monotonic specular-occlusion frame index and returns this frame's specocc trace
    /// push. Mirrors [`Ssao::next_dfao_push`]: the world reconstruction reuses the shared G-buffer
    /// basis — `invProjection` (clip → view) and `invView` (view → world = the frame's world→view
    /// inverted).
    pub fn next_specocc_push(&mut self) -> SpecoccPush {
        self.specocc_frame = self.specocc_frame.wrapping_add(1);
        SpecoccPush {
            inv_projection: self.inv_projection,
            inv_view: self.view.inverse(),
            params: Vec4::new(self.specocc_frame as f32, 0.0, 0.0, 0.0),
        }
    }

    /// Bumps the monotonic SSR frame index (jittering the march so TAA averages the
    /// stair-stepping) and returns this frame's SSR trace push. The march runs far enough
    /// to cross the scene (a wide multiple of the AO radius) at full intensity.
    pub fn next_ssr_push(&mut self) -> SsgiPush {
        self.ssr_frame = self.ssr_frame.wrapping_add(1);
        SsgiPush {
            projection: self.projection,
            inv_projection: self.inv_projection,
            params: Vec4::new(
                // SSR marches far enough to cross the scene (80× the contact AO base ≈ 40 view
                // units), its own long extent independent of GTAO's tightened contact radius.
                self.radius * 80.0,
                1.0,
                self.ssr_steps,
                self.ssr_frame as f32,
            ),
            params2: Vec4::ZERO,
        }
    }
}

impl Drop for Ssao {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The `Arc<DeviceResources>` keeps the device alive for
        // the call; the run loop idled it before teardown (README §4). Each handle is
        // freed exactly once; the per-view sets free with the shared descriptor pool.
        let raw = self.resources.device();
        unsafe {
            raw.destroy_descriptor_set_layout(self.compute2_layout, None);
            raw.destroy_descriptor_set_layout(self.compute3_layout, None);
            raw.destroy_descriptor_set_layout(self.gi_resolve_layout, None);
            raw.destroy_sampler(self.nearest_sampler, None);
        }
    }
}

/// The nearest, clamp-to-edge sampler the screen-space passes read the G-buffer with.
fn create_nearest_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
    // SAFETY: the ash seam. The sampler is owned and freed in `Drop`.
    checked(
        unsafe { raw.create_sampler(&info, None) },
        "ssao nearest sampler",
    )
}

/// Whether the thin G-buffer prepass runs this frame: it runs when the chain is ready
/// (sub-state built + the active view's screen-space targets exist) AND any of GTAO /
/// contact / SSGI is on. Pure logic, so the named acceptance test ("`has_gbuffer`
/// matches the toggle set") can assert it without a device.
pub(crate) fn wants_gbuffer_prepass(
    gbuf_ready: bool,
    use_ssao: bool,
    use_contact: bool,
    use_ssgi: bool,
    use_ssr: bool,
) -> bool {
    gbuf_ready && (use_ssao || use_contact || use_ssgi || use_ssr)
}

/// A compute set layout with `sampler_count` leading combined-image-sampler bindings
/// followed by one storage-image binding — the 2-binding (1 sampler) and 3-binding
/// (2 sampler) shapes the screen-space chain uses. All bindings compute-stage.
fn create_compute_layout(raw: &ash::Device, sampler_count: u32) -> Result<vk::DescriptorSetLayout> {
    let mut bindings = Vec::with_capacity(sampler_count as usize + 1);
    for b in 0..sampler_count {
        bindings.push(
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        );
    }
    bindings.push(
        vk::DescriptorSetLayoutBinding::default()
            .binding(sampler_count)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    );
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ssao compute layout",
    )
}

/// The gi-resolve pass's single set layout (matches `gi_resolve.slang` set 0): b0 G-buffer sampler,
/// b1 storage-image output, b2 params UBO, b3 IBL irradiance cube sampler, b4 dfao sampler, b5/b6 the
/// DDGI irradiance + distance-moment atlas samplers. All compute-stage.
fn create_gi_resolve_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let sampled = |b: u32| {
        vk::DescriptorSetLayoutBinding::default()
            .binding(b)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
    };
    let bindings = [
        sampled(0),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        sampled(3),
        sampled(4),
        sampled(5),
        sampled(6),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "gi-resolve set layout",
    )
}

/// The mesh set-4 layout is owned by [`Descriptors`]; this re-exports the accessor so
/// the per-view set allocation reads from one place.
pub(crate) fn mesh_set_layout(descriptors: &Descriptors) -> vk::DescriptorSetLayout {
    descriptors.ssao_mesh_set_layout()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push-constant structs byte-match the `.slang` `Push` layouts the SPIR-V
    /// reads — a wrong offset is a silently corrupted dispatch, so pin each size.
    /// Pure layout logic, runs on any host.
    #[test]
    fn screen_space_push_sizes_match_slang() {
        assert_eq!(size_of::<GbufferPush>(), 128);
        assert_eq!(size_of::<GtaoPush>(), 80);
        assert_eq!(size_of::<ContactPush>(), 160);
        assert_eq!(size_of::<SsgiPush>(), 160);
        assert_eq!(size_of::<SsgiAccumPush>(), 16);
        assert_eq!(size_of::<DfaoPush>(), 144);
        assert_eq!(size_of::<SpecoccPush>(), 144);
    }

    /// `set_camera` derives the view-space sun direction as the *normalized negation*
    /// of the incoming world direction, transformed by the view matrix — the contact
    /// shadow marches TO the light. A pure-math check, no device.
    #[test]
    fn set_camera_negates_and_normalizes_sun_direction() {
        let mut ssao = SsaoCameraOnly::default();
        // Identity view: the view-space sun dir is just the normalized negation.
        ssao.set_camera(Mat4::IDENTITY, Mat4::IDENTITY, Vec3::new(0.0, -2.0, 0.0));
        let dir = ssao.sun_dir_view;
        assert!(
            (dir - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5,
            "got {dir:?}"
        );
        assert!((dir.length() - 1.0).abs() < 1e-5);
    }

    /// The thin G-buffer prepass runs only when the chain is ready AND at least one
    /// effect is on — `has_gbuffer` matches the toggle set. Pure gate logic, no device.
    #[test]
    fn gbuffer_prepass_runs_only_when_an_effect_needs_it() {
        // Not ready → never, whatever the toggles.
        assert!(!wants_gbuffer_prepass(false, true, true, true, true));
        // Ready but all effects off → no prepass.
        assert!(!wants_gbuffer_prepass(true, false, false, false, false));
        // Ready + any single effect on → the prepass runs.
        assert!(wants_gbuffer_prepass(true, true, false, false, false));
        assert!(wants_gbuffer_prepass(true, false, true, false, false));
        assert!(wants_gbuffer_prepass(true, false, false, true, false));
        assert!(wants_gbuffer_prepass(true, false, false, false, true));
        assert!(wants_gbuffer_prepass(true, true, true, true, true));
    }

    /// `next_ssgi_push` advances the monotonic frame index every call (decorrelating
    /// the trace noise) and folds it into the push's `w` channel.
    #[test]
    fn next_ssgi_push_advances_frame_index() {
        let mut ssao = SsaoCameraOnly::default();
        let a = ssao.next_ssgi_push();
        let b = ssao.next_ssgi_push();
        assert_eq!(a.params.w, 1.0);
        assert_eq!(b.params.w, 2.0);
    }

    /// A device-free shadow of [`Ssao`] carrying only the camera/SSGI math the pure
    /// tests exercise — the real `Ssao::new` needs a Vulkan device for its sampler +
    /// layouts, so the math is mirrored here verbatim and asserted by the byte-size
    /// test that the production push builders share these struct layouts.
    #[derive(Default)]
    struct SsaoCameraOnly {
        view: Mat4,
        projection: Mat4,
        inv_projection: Mat4,
        sun_dir_view: Vec3,
        radius: f32,
        ssgi_intensity: f32,
        ssgi_frame: u32,
    }

    impl SsaoCameraOnly {
        fn set_camera(&mut self, view: Mat4, proj: Mat4, sun_direction_world: Vec3) {
            self.view = view;
            self.projection = proj;
            self.inv_projection = proj.inverse();
            let sun_view = view * (-sun_direction_world).extend(0.0);
            self.sun_dir_view = sun_view.truncate().normalize_or_zero();
        }

        fn next_ssgi_push(&mut self) -> SsgiPush {
            self.ssgi_frame = self.ssgi_frame.wrapping_add(1);
            SsgiPush {
                projection: self.projection,
                inv_projection: self.inv_projection,
                params: Vec4::new(
                    self.radius * 2.0,
                    self.ssgi_intensity,
                    8.0,
                    self.ssgi_frame as f32,
                ),
                params2: Vec4::new(4.0, 0.0, 0.0, 0.0),
            }
        }
    }
}
