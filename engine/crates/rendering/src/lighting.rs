//! The lighting rig: the per-frame directional-light + ambient + eye UBO, the
//! punctual-light storage buffer, the clustered-forward froxel cull state, and the
//! directional / spot / point shadow transforms.
//!
//! Per the borrow discipline (README §2) the device is shared-immutable; this sub-state
//! owns its Vulkan handles and mutates them through `&mut self` methods plus `&Device`.
//! There is no `Arc<Mutex>` here — the host writes the per-frame UBO/SSBO on the render
//! thread only, after the frame's fence is waited, and the buffers are frame-indexed so
//! a host write never races a frame still reading on the GPU.
//!
//! # Clustered forward is the one lighting path
//!
//! [`Lighting::use_clustered`] defaults true; the light-cull compute pass fills the
//! per-cluster count+index SSBO the mesh fragment reads. When off, the fragment loops
//! all lights — kept only as a correctness oracle behind one bool. The
//! froxel-assignment math the cull pass runs on the GPU
//! ([`light_cull.slang`]) is mirrored here as pure CPU functions ([`cluster_aabb`],
//! [`light_intersects_cluster`]) so the cull is unit-testable with no device — a wrong
//! AABB or intersection test is a silently-dark or silently-overlit scene, not a
//! compile error.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, UVec4, Vec3, Vec4};

use crate::clouds::CLOUD_SHADOW_CASCADES;
use crate::descriptors::Descriptors;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::GpuLight;
use crate::resources::{Buffer, DeviceResources};
use crate::{Device, Result};

/// Froxel cluster grid: X×Y screen tiles, Z exponential view-space slices. Must match
/// `light_cull.slang` + `mesh.slang`.
pub const CLUSTER_GRID_X: u32 = 16;
/// Cluster grid Y (screen tiles).
pub const CLUSTER_GRID_Y: u32 = 9;
/// Cluster grid Z (exponential view-space slices).
pub const CLUSTER_GRID_Z: u32 = 24;
/// Total froxel clusters — the cull dispatch covers `ceil(CLUSTER_COUNT / 64)` groups.
pub const CLUSTER_COUNT: u32 = CLUSTER_GRID_X * CLUSTER_GRID_Y * CLUSTER_GRID_Z;
/// Max punctual lights one froxel cluster records — the per-cluster list cap.
pub const MAX_LIGHTS_PER_CLUSTER: u32 = 64;

/// One cluster's light list in the SSBO: a `count` u32 followed by a fixed
/// `MAX_LIGHTS_PER_CLUSTER` slot of light indices — matching the shader's `Cluster`
/// struct (std430, tight u32 array).
const CLUSTER_STRIDE: vk::DeviceSize =
    (1 + MAX_LIGHTS_PER_CLUSTER as u64) * size_of::<u32>() as u64;

/// Initial punctual-light buffer capacity (in [`GpuLight`] elements), grown on demand
/// thereafter.
const LIGHT_LIST_INITIAL: u32 = 16;

/// Constant depth bias for the shadow depth pass (units of D32 depth) — kills acne
/// without obvious peter-panning on llvmpipe.
pub const SHADOW_DEPTH_BIAS_CONSTANT: f32 = 1.25;
/// Slope-scaled depth bias for the shadow depth pass.
pub const SHADOW_DEPTH_BIAS_SLOPE: f32 = 2.0;

/// The per-frame directional + ambient + eye + shadow-transform UBO (set 1, binding 0).
/// std140-compatible: every member is a 16-byte-aligned `vec4`/`uvec4`/`mat4` block, so
/// the `#[repr(C)]` field sequence lays out with no implicit padding.
///
/// Byte-matched by the size assert + the offset test; the mesh fragment reads it by raw
/// bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightUbo {
    /// `xyz` normalized directional-light direction, `w` scalar ambient luminance.
    pub direction_ambient: Vec4,
    /// `rgb` directional color, `a` intensity.
    pub color_intensity: Vec4,
    /// `xyz` normalized moon-light travel direction, `w` moon intensity.
    pub moon_direction_intensity: Vec4,
    /// `rgb` moon-light color.
    pub moon_color: Vec4,
    /// `x` punctual count, `y` directional-shadow flag, `z` IBL-ambient flag, `w` SSAO flag.
    pub counts: UVec4,
    /// `xyz` world-space camera position.
    pub eye_position: Vec4,
    /// Shadowed spot light-space transform (perspective).
    pub spot_shadow_view_proj: Mat4,
    /// `x` shadowed spot's light index, `y` enabled (0/1).
    pub spot_shadow: UVec4,
    /// `xyz` shadowed point light world position, `w` far plane.
    pub point_shadow: Vec4,
    /// `x` shadowed point's light index, `y` enabled (0/1), `z` RT-shadow flag, `w` debug channel.
    pub point_shadow_meta: UVec4,
    /// `x` contact-shadow flag, `y` SSGI flag, `z` DDGI flag, `w` ReSTIR direct-lighting flag.
    pub screen_flags: UVec4,
    /// `xyz` DDGI volume world min corner.
    pub ddgi_volume_min: Vec4,
    /// `xyz` DDGI volume world size.
    pub ddgi_volume_extent: Vec4,
    /// `xyz` DDGI probes per axis, `w` irradiance octahedral interior.
    pub ddgi_probe_count: UVec4,
    /// `xyz` DDGI toroidal scroll base (`wrapMod(snapBase, count)`) — the mesh maps a logical probe
    /// index to its physical atlas tile; `w` reserved.
    pub ddgi_scroll_base: UVec4,
    /// `x` SDF-occluder instance count (the lighting set's binding-8 SSBO), `y` sky-occlusion
    /// enable (1 = the analytic IBL diffuse/specular are attenuated by the per-mesh SDF DFAO
    /// cone-trace); `zw` reserved.
    pub sdf_occlusion: UVec4,
    /// `rgb` scene-environment ambient (the non-IBL fallback), `a` reflection-probe count.
    pub ambient_color: Vec4,
    /// `rgb` artist-authored time-of-day tint for image-based ambient lighting.
    pub ibl_tint: Vec4,
    /// `x` screen-space-reflection flag, `y` ray-traced-reflection flag, `z` directional
    /// volumetric-scatter multiplier (float bits), `w` directional cast-volumetric-shadow gate (0/1).
    pub extra_flags: UVec4,
    /// Previous frame's view-proj (world → clip), reprojecting an RT reflection hit into
    /// `prev_color` for its reflected radiance.
    pub prev_view_proj: Mat4,
    /// Froxel volumetric-fog composite params for the forward transparent path: `x` = enabled (1
    /// when `fog.mode == volumetric` this frame, else 0), `y` = froxel near, `z` = froxel far (the
    /// exponential-Z distribution the transparent W mapping inverts), `w` reserved.
    pub froxel_fog: Vec4,
    /// Cloud-shadow light-plane right axis.
    pub cloud_shadow_right: Vec4,
    /// Cloud-shadow light-plane up axis.
    pub cloud_shadow_up: Vec4,
    /// Camera-snapped cloud-shadow centers and half extents.
    pub cloud_shadow_centers: [Vec4; CLOUD_SHADOW_CASCADES as usize],
    /// `x` ESM exponent, `y` cloud/fog strength, `z` surface strength, `w` enabled.
    pub cloud_shadow_meta: Vec4,
    /// `xy` mean wind direction (x, z), `z` speed m/s, `w` gust fraction.
    pub wind_dir_speed_gust: Vec4,
    /// `x` turbulence roughness, `y` gust frequency Hz, `z` reference height m,
    /// `w` height shear exponent.
    pub wind_params: Vec4,
    /// `x` turbulence octaves, `y` phase seed.
    pub wind_meta: UVec4,
    /// `x` simulation seconds this frame, `y` previous frame's seconds.
    pub wind_time: Vec4,
    /// The frame's local wind-source list (device address; the count rides
    /// `wind_meta.z`).
    pub wind_sources: u64,
    /// Reserved ABI tail word.
    pub wind_sources_reserved: u64,
    /// World → light rotation of the directional virtual shadow space.
    pub vsm_basis: Mat4,
    /// Per clip level: window origin (light-plane metres) in `xy`, extent in `z`.
    pub vsm_levels: [Vec4; crate::VSM_DIRECTIONAL_LEVELS as usize],
    /// `x` = light-space depth centre, `y` = depth half-span, `z` = enabled flag.
    pub vsm_params: Vec4,
    /// The frame's page-table device address (8 levels × 1024 packed entries).
    pub vsm_page_table: u64,
    /// Reserved ABI tail word.
    pub vsm_reserved: u64,
}

const _: () = assert!(
    size_of::<LightUbo>() == 832,
    "LightUbo must match the std140 shader layout"
);

impl Default for LightUbo {
    fn default() -> Self {
        Self {
            direction_ambient: Vec4::new(0.0, -1.0, 0.0, 0.0),
            color_intensity: Vec4::new(1.0, 1.0, 1.0, 0.0),
            moon_direction_intensity: Vec4::ZERO,
            moon_color: Vec4::ZERO,
            counts: UVec4::ZERO,
            eye_position: Vec4::ZERO,
            spot_shadow_view_proj: Mat4::IDENTITY,
            spot_shadow: UVec4::ZERO,
            point_shadow: Vec4::new(0.0, 0.0, 0.0, 1.0),
            point_shadow_meta: UVec4::ZERO,
            screen_flags: UVec4::ZERO,
            ddgi_volume_min: Vec4::ZERO,
            ddgi_volume_extent: Vec4::ZERO,
            ddgi_probe_count: UVec4::ZERO,
            ddgi_scroll_base: UVec4::ZERO,
            sdf_occlusion: UVec4::ZERO,
            ambient_color: Vec4::ZERO,
            ibl_tint: Vec4::ONE,
            extra_flags: UVec4::ZERO,
            prev_view_proj: Mat4::IDENTITY,
            froxel_fog: Vec4::ZERO,
            cloud_shadow_right: Vec4::X,
            cloud_shadow_up: Vec4::Z,
            cloud_shadow_centers: [Vec4::ZERO; CLOUD_SHADOW_CASCADES as usize],
            cloud_shadow_meta: Vec4::ZERO,
            wind_dir_speed_gust: Vec4::new(0.0, 1.0, 0.0, 0.0),
            wind_params: Vec4::new(0.55, 0.15, 10.0, 0.2),
            wind_meta: UVec4::ZERO,
            wind_time: Vec4::ZERO,
            wind_sources: 0,
            wind_sources_reserved: 0,
            vsm_basis: Mat4::IDENTITY,
            vsm_levels: [Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize],
            vsm_params: Vec4::ZERO,
            vsm_page_table: 0,
            vsm_reserved: 0,
        }
    }
}

/// The clustered-cull params UBO (set 1, binding 3 in the mesh set; binding 0 in the
/// cull compute set). std140-compatible.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ClusterParams {
    /// World → view (cull: light positions; fragment: froxel Z).
    pub view: Mat4,
    /// Clip → view (cull: tile AABB build).
    pub inverse_projection: Mat4,
    /// `xyz` grid dims, `w` punctual light count.
    pub grid_size: UVec4,
    /// `xy` offscreen pixel dims, `z` clustered-valid flag.
    pub screen_size: UVec4,
    /// `x` near plane, `y` far plane.
    pub z_planes: Vec4,
}

const _: () = assert!(
    size_of::<ClusterParams>() == 176,
    "ClusterParams must match the std140 shader layout (2 mat4 + 2 uvec4 + 1 vec4)"
);

impl Default for ClusterParams {
    fn default() -> Self {
        Self {
            view: Mat4::IDENTITY,
            inverse_projection: Mat4::IDENTITY,
            grid_size: UVec4::new(CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, 0),
            screen_size: UVec4::ZERO,
            z_planes: Vec4::ZERO,
        }
    }
}

/// The camera + viewport state the per-frame cluster params are derived from. Plain
/// `Copy` data the host fills from the active camera.
#[derive(Debug, Clone, Copy)]
pub struct ClusterCamera {
    /// World → view.
    pub view: Mat4,
    /// View → clip (its inverse is stored).
    pub projection: Mat4,
    /// Offscreen pixel width.
    pub width: u32,
    /// Offscreen pixel height.
    pub height: u32,
    /// Camera near plane.
    pub near: f32,
    /// Camera far plane.
    pub far: f32,
}

/// The scene-lighting state the per-frame light UBO is derived from. The directional
/// light + ambient + eye + the punctual list; the rest of the UBO's flags are folded in
/// The shared wind field's per-frame parameters, mirrored into the light UBO for
/// the shader-side sampler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneWind {
    /// Horizontal direction in degrees, clockwise from world +Z.
    pub orientation: f32,
    /// Mean advection speed in metres per second at the reference height.
    pub speed: f32,
    /// Turbulent fraction of the mean speed.
    pub gust: f32,
    /// Turbulence octave count.
    pub turbulence_octaves: u32,
    /// Per-octave amplitude falloff.
    pub turbulence_roughness: f32,
    /// Gust-front passage frequency in hertz.
    pub gust_frequency: f32,
    /// Height in metres at which `speed` is authored.
    pub reference_height: f32,
    /// Power-law shear exponent.
    pub height_exponent: f32,
    /// Deterministic phase seed.
    pub seed: u32,
    /// Monotonic simulation seconds.
    pub time_s: f64,
}

impl Default for SceneWind {
    fn default() -> Self {
        Self {
            orientation: 0.0,
            speed: 0.0,
            gust: 0.0,
            turbulence_octaves: 0,
            turbulence_roughness: 0.55,
            gust_frequency: 0.15,
            reference_height: 10.0,
            height_exponent: 0.2,
            seed: 0,
            time_s: 0.0,
        }
    }
}

impl SceneWind {
    /// The frame's parameters as the shared field's sampling profile.
    pub fn profile(&self) -> saffron_wind::WindProfile {
        saffron_wind::WindProfile {
            orientation: self.orientation,
            speed: self.speed,
            gust: self.gust,
            turbulence_octaves: self.turbulence_octaves,
            turbulence_roughness: self.turbulence_roughness,
            gust_frequency: self.gust_frequency,
            reference_height: self.reference_height,
            height_exponent: self.height_exponent,
            seed: self.seed,
        }
    }
}

/// by the renderer (IBL/SSAO/etc.).
#[derive(Debug, Clone)]
pub struct SceneLighting {
    /// The directional light direction (the way the light travels; normalized on write).
    pub direction: Vec3,
    /// The directional light color.
    pub color: Vec3,
    /// The directional light intensity.
    pub intensity: f32,
    /// The moon-light travel direction.
    pub moon_direction: Vec3,
    /// The atmosphere-coupled moon-light color.
    pub moon_color: Vec3,
    /// The atmosphere-coupled moon-light intensity.
    pub moon_intensity: f32,
    /// The scene-environment ambient (the non-IBL fallback term).
    pub ambient: Vec3,
    /// Per-frame artist tint applied to image-based ambient lighting.
    pub ibl_tint: Vec3,
    /// The world-space camera position.
    pub eye_position: Vec3,
    /// The directional light's per-light fog in-scatter multiplier (its shaft brightness).
    pub directional_volumetric: f32,
    /// Whether the directional light's shadow gates its in-scatter in volumetric fog.
    pub directional_cast_volumetric_shadow: bool,
    /// The punctual (point/spot) lights uploaded into the per-frame storage buffer.
    pub lights: Vec<GpuLight>,
}

impl Default for SceneLighting {
    fn default() -> Self {
        // The reset container carries no sun: zero intensity and zero ambient (a valid
        // down direction keeps the write's `normalize` finite). A scene supplies its own
        // directional light explicitly; this default asserts none.
        Self {
            direction: Vec3::NEG_Y,
            color: Vec3::ONE,
            intensity: 0.0,
            moon_direction: Vec3::Y,
            moon_color: Vec3::ZERO,
            moon_intensity: 0.0,
            ambient: Vec3::ZERO,
            ibl_tint: Vec3::ONE,
            eye_position: Vec3::ZERO,
            directional_volumetric: 0.0,
            directional_cast_volumetric_shadow: false,
            lights: Vec::new(),
        }
    }
}

/// One frame-in-flight's lighting buffers + descriptor sets: the directional light UBO
/// (binding 0), the grow-on-demand punctual SSBO (binding 1), the cluster lists SSBO
/// (binding 2, written by the cull compute), and the cluster params UBO (binding 3),
/// plus the compute cluster set the cull pass binds.
struct FrameLighting {
    light_set: vk::DescriptorSet,
    light_ubo: Buffer,
    light_list: Buffer,
    light_list_capacity: u32,
    cluster_set: vk::DescriptorSet,
    cluster_buffer: Buffer,
    cluster_params: Buffer,
}

/// The lighting rig sub-state.
///
/// Built once in [`Lighting::new`] (one light + cluster set per frame slot, the shadow
/// maps bound into every light set), then mutated through its own `&mut self` methods.
/// Owns an [`Arc`]`<`[`DeviceResources`]`>` so each [`Buffer`] (a Drop type) frees
/// without a live `&Device`; the descriptor sets free implicitly with the shared pool.
pub struct Lighting {
    resources: Arc<DeviceResources>,
    frames: Vec<FrameLighting>,

    /// Clustered-forward toggle; false = the fragment loops all lights (reference).
    pub use_clustered: bool,
    /// Master shadow toggle (`sa set-shadows`).
    pub use_shadows: bool,

    frame_light_count: u32,
    frame_probe_count: u32,
    frame_ibl_flag: bool,
    frame_ddgi_flag: bool,
    frame_ssr_flag: bool,
    frame_rt_reflections_flag: bool,
    frame_prev_view_proj: Mat4,
    frame_froxel_fog: Vec4,
    frame_cloud_shadow_right: Vec4,
    frame_cloud_shadow_up: Vec4,
    frame_cloud_shadow_centers: [Vec4; CLOUD_SHADOW_CASCADES as usize],
    frame_cloud_shadow_meta: Vec4,
    frame_wind_dir_speed_gust: Vec4,
    frame_wind_params: Vec4,
    frame_wind_meta: UVec4,
    frame_wind_time: Vec4,
    frame_wind_sources: u64,
    frame_vsm_basis: Mat4,
    frame_vsm_levels: [Vec4; crate::VSM_DIRECTIONAL_LEVELS as usize],
    frame_vsm_params: Vec4,
    frame_vsm_page_table: u64,
    frame_ddgi_volume_min: Vec4,
    frame_ddgi_volume_extent: Vec4,
    frame_ddgi_probe_count: UVec4,
    frame_ddgi_scroll_base: UVec4,
    frame_sdf_occlusion: UVec4,
    cluster_dispatch_pending: bool,

    shadow_pending: bool,
    spot_shadow_pending: bool,
    spot_shadow_view_proj: Mat4,
    spot_shadow_light_index: u32,
    point_shadow_pending: bool,
    point_shadow_pos: Vec3,
    point_shadow_far: f32,
    point_shadow_light_index: u32,
    /// Camera-independent hash of the cube's inputs (light + caster transforms), set each frame by
    /// `set_point_shadow`. The renderer compares it against the last rendered key (and the cube

    /// The debug view-mode channel the mesh fragment outputs instead of full shading
    /// (`0` lit/wireframe, `1` albedo, … `5` emissive), folded into the light UBO's
    /// `point_shadow_meta.w`.
    debug_channel: u32,
}

impl Lighting {
    /// Builds the per-frame light + cluster buffers and sets, binding the shadow maps
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing buffer/set allocation.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        vsm_atlas: vk::ImageView,
    ) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            frames.push(build_frame(&resources, descriptors, vsm_atlas)?);
        }
        Ok(Self {
            resources,
            frames,
            use_clustered: true,
            use_shadows: true,
            frame_light_count: 0,
            frame_probe_count: 0,
            frame_ibl_flag: false,
            frame_ddgi_flag: false,
            frame_ssr_flag: false,
            frame_rt_reflections_flag: false,
            frame_prev_view_proj: Mat4::IDENTITY,
            frame_froxel_fog: Vec4::ZERO,
            frame_cloud_shadow_right: Vec4::X,
            frame_cloud_shadow_up: Vec4::Z,
            frame_cloud_shadow_centers: [Vec4::ZERO; CLOUD_SHADOW_CASCADES as usize],
            frame_cloud_shadow_meta: Vec4::ZERO,
            frame_wind_dir_speed_gust: Vec4::new(0.0, 1.0, 0.0, 0.0),
            frame_wind_params: Vec4::new(0.55, 0.15, 10.0, 0.2),
            frame_wind_meta: UVec4::ZERO,
            frame_wind_time: Vec4::ZERO,
            frame_wind_sources: 0,
            frame_vsm_basis: Mat4::IDENTITY,
            frame_vsm_levels: [Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize],
            frame_vsm_params: Vec4::ZERO,
            frame_vsm_page_table: 0,
            frame_ddgi_volume_min: Vec4::ZERO,
            frame_ddgi_volume_extent: Vec4::ZERO,
            frame_ddgi_probe_count: UVec4::ZERO,
            frame_ddgi_scroll_base: UVec4::ZERO,
            frame_sdf_occlusion: UVec4::ZERO,
            cluster_dispatch_pending: false,
            shadow_pending: false,
            spot_shadow_pending: false,
            spot_shadow_view_proj: Mat4::IDENTITY,
            spot_shadow_light_index: 0,
            point_shadow_pending: false,
            point_shadow_pos: Vec3::ZERO,
            point_shadow_far: 1.0,
            point_shadow_light_index: 0,
            debug_channel: 0,
        })
    }

    /// Sets the debug view-mode channel folded into the next light UBO write (`0` = full
    /// shading).
    pub fn set_debug_channel(&mut self, channel: u32) {
        self.debug_channel = channel;
    }

    /// The frame slot's light descriptor set (set 1), bound once by the scene + shadow
    /// passes.
    pub fn light_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].light_set
    }

    /// Writes the renderer-owned per-mesh SDF-occluder instance SSBO into binding 8 of every
    /// frame slot's light set (set 1). The buffer is persistent (one allocation for the
    /// renderer's life, its prefix rewritten each frame by `set_sdf_scene`), so this is a
    /// one-time wire-up at construction — the cone-trace reads the first `sdf_occlusion.x`
    /// entries.
    pub fn bind_sdf_instances(
        &self,
        descriptors: &Descriptors,
        buffer: vk::Buffer,
        size: vk::DeviceSize,
    ) {
        for frame in &self.frames {
            descriptors.write_storage_buffer(frame.light_set, 8, buffer, size);
        }
    }

    /// Writes the froxel volumetric-fog integration volume (binding 11) into every frame slot's
    /// light set (set 1) with `sampler`. Bound at construction and rewritten whenever a fog quality
    /// switch reallocates the volume; its contents are rewritten each frame by the integrate pass. The
    /// forward transparent path gates its sample on `froxel_fog.x`, so the binding is valid even with
    /// fog off (the volume rests in `SHADER_READ_ONLY_OPTIMAL`).
    pub fn bind_froxel_integration(
        &self,
        device: &Device,
        view: vk::ImageView,
        sampler: vk::Sampler,
    ) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        for frame in &self.frames {
            let write = vk::WriteDescriptorSet::default()
                .dst_set(frame.light_set)
                .dst_binding(11)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&info);
            // SAFETY: the ash seam. The set + view + sampler outlive the call; single-threaded at the
            // (idle) build point, so no in-flight command buffer references the light set.
            unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
        }
    }

    /// Writes the Global-SDF cascade samplers (binding 9) + the params UBO (binding 10) into every
    /// frame slot's light set (set 1). The cascade volumes + UBO are persistent (one allocation for
    /// the renderer's life), so this is a one-time wire-up at construction; the consumers gate the
    /// far-field tap on the UBO's `enabled` control flag, so the binding is valid even with the GDF
    /// off (the cascades rest in `SHADER_READ_ONLY_OPTIMAL`).
    pub fn bind_gdf(&self, global_sdf: &crate::GlobalSdf) {
        for (i, frame) in self.frames.iter().enumerate() {
            global_sdf.write_light_set(frame.light_set, i);
        }
    }

    /// The frame slot's compute cluster set, bound by the light-cull pass.
    pub fn cluster_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].cluster_set
    }

    /// The frame slot's cluster lists SSBO (the cull writes it, the fragment reads it) —
    /// the render graph imports this to derive the compute→fragment barrier.
    pub fn cluster_buffer(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].cluster_buffer.handle()
    }

    /// The frame slot's cluster lists SSBO handle + byte size — the ReSTIR initial pass
    /// reads the froxel candidate lists, so it binds this buffer into its set per frame.
    pub fn cluster_buffer_with_size(&self, frame: usize) -> (vk::Buffer, vk::DeviceSize) {
        let buffer = &self.frames[frame].cluster_buffer;
        (buffer.handle(), buffer.size())
    }

    /// The frame slot's punctual light SSBO handle + byte size — the ReSTIR passes read it
    /// to sample candidate lights, so they bind this buffer into their sets per frame. The
    /// buffer regrows with the light count, so it is rebound each frame.
    pub fn light_list_buffer(&self, frame: usize) -> (vk::Buffer, vk::DeviceSize) {
        let buffer = &self.frames[frame].light_list;
        (buffer.handle(), buffer.size())
    }

    /// Whether a cull dispatch is armed this frame (clustered on + at least one punctual
    /// light). Consumed (cleared) by the renderer when it schedules the pass.
    pub fn take_cluster_dispatch_pending(&mut self) -> bool {
        std::mem::take(&mut self.cluster_dispatch_pending)
    }

    /// Whether a directional shadow caster is present this frame (arms the `shadow` pass).
    pub fn shadow_pending(&self) -> bool {
        self.shadow_pending
    }

    /// Whether a shadow-casting spot light is present this frame (arms `spot-shadow`).
    pub fn spot_shadow_pending(&self) -> bool {
        self.spot_shadow_pending
    }

    /// The shadowed spot's perspective light-space transform.
    pub fn spot_shadow_view_proj(&self) -> Mat4 {
        self.spot_shadow_view_proj
    }

    /// Whether a shadow-casting point light is present this frame (arms `point-shadow`).
    pub fn point_shadow_pending(&self) -> bool {
        self.point_shadow_pending
    }

    /// The shadowed point light's world position.
    pub fn point_shadow_pos(&self) -> Vec3 {
        self.point_shadow_pos
    }

    /// The shadowed point light's far plane.
    pub fn point_shadow_far(&self) -> f32 {
        self.point_shadow_far
    }

    /// The punctual lights uploaded this frame.
    pub fn frame_light_count(&self) -> u32 {
        self.frame_light_count
    }

    /// Writes the current frame's directional + ambient + eye + punctual lights. Grows
    /// the punctual SSBO if needed, uploads the light list, and fills the light UBO's
    /// directional + ambient + counts + shadow-transform fields.
    ///
    /// `frame` is the in-flight slot (its fence was already waited, so no GPU read races
    /// the write). The renderer folds in the IBL/SSAO/DDGI/ReSTIR flags via
    /// [`Lighting::set_frame_flags`] before this; here they default to off.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if growing the punctual SSBO fails.
    pub fn set_scene_lighting(
        &mut self,
        descriptors: &Descriptors,
        frame: usize,
        scene: &SceneLighting,
    ) -> Result<()> {
        let count = scene.lights.len() as u32;
        if count > 0 {
            self.ensure_light_capacity(descriptors, frame, count)?;
            let bytes: &[u8] = bytemuck::cast_slice(&scene.lights);
            let dst = self.frames[frame]
                .light_list
                .mapped_bytes()
                .expect("punctual light buffer is mapped");
            dst[..bytes.len()].copy_from_slice(bytes);
        }

        let dir = scene.direction.normalize_or_zero();
        let ambient_luma = (scene.ambient.x + scene.ambient.y + scene.ambient.z) / 3.0;
        let ubo = LightUbo {
            direction_ambient: dir.extend(ambient_luma),
            color_intensity: scene.color.extend(scene.intensity),
            moon_direction_intensity: scene
                .moon_direction
                .normalize_or_zero()
                .extend(scene.moon_intensity),
            moon_color: scene.moon_color.extend(0.0),
            counts: UVec4::new(
                count,
                u32::from(self.shadow_pending),
                u32::from(self.frame_ibl_flag),
                0,
            ),
            eye_position: scene.eye_position.extend(0.0),
            spot_shadow_view_proj: self.spot_shadow_view_proj,
            spot_shadow: UVec4::new(
                self.spot_shadow_light_index,
                u32::from(self.spot_shadow_pending),
                0,
                0,
            ),
            point_shadow: self.point_shadow_pos.extend(self.point_shadow_far),
            // .z = RT-shadow flag (folded by the RT phase); .w = the debug view-mode
            // channel the mesh fragment outputs instead of shading.
            point_shadow_meta: UVec4::new(
                self.point_shadow_light_index,
                u32::from(self.point_shadow_pending),
                0,
                self.debug_channel,
            ),
            // screen_flags = (contact, ssgi, ddgi, restir); the mesh gates the DDGI
            // sample on `screen_flags.z`.
            screen_flags: UVec4::new(0, 0, u32::from(self.frame_ddgi_flag), 0),
            ddgi_volume_min: self.frame_ddgi_volume_min,
            ddgi_volume_extent: self.frame_ddgi_volume_extent,
            ddgi_probe_count: self.frame_ddgi_probe_count,
            ddgi_scroll_base: self.frame_ddgi_scroll_base,
            sdf_occlusion: self.frame_sdf_occlusion,
            ambient_color: scene.ambient.extend(f32::from_bits(self.frame_probe_count)),
            ibl_tint: scene.ibl_tint.extend(0.0),
            extra_flags: UVec4::new(
                u32::from(self.frame_ssr_flag),
                u32::from(self.frame_rt_reflections_flag),
                scene.directional_volumetric.to_bits(),
                u32::from(scene.directional_cast_volumetric_shadow),
            ),
            prev_view_proj: self.frame_prev_view_proj,
            froxel_fog: self.frame_froxel_fog,
            cloud_shadow_right: self.frame_cloud_shadow_right,
            cloud_shadow_up: self.frame_cloud_shadow_up,
            cloud_shadow_centers: self.frame_cloud_shadow_centers,
            cloud_shadow_meta: self.frame_cloud_shadow_meta,
            wind_dir_speed_gust: self.frame_wind_dir_speed_gust,
            wind_params: self.frame_wind_params,
            wind_meta: self.frame_wind_meta,
            wind_time: self.frame_wind_time,
            wind_sources: self.frame_wind_sources,
            wind_sources_reserved: 0,
            vsm_basis: self.frame_vsm_basis,
            vsm_levels: self.frame_vsm_levels,
            vsm_params: self.frame_vsm_params,
            vsm_page_table: self.frame_vsm_page_table,
            vsm_reserved: 0,
        };
        let dst = self.frames[frame]
            .light_ubo
            .mapped_bytes()
            .expect("light UBO is mapped");
        dst[..size_of::<LightUbo>()].copy_from_slice(bytemuck::bytes_of(&ubo));
        self.frame_light_count = count;
        Ok(())
    }

    /// Folds the IBL-ambient flag (`counts.z`) + the reflection-probe count
    /// (`ambient_color.w`) into the next [`Lighting::set_scene_lighting`] write. The
    /// renderer reads its sibling `Ibl`/`ReflectionProbes` sub-state and pushes them here
    /// before the UBO write.
    /// Folds the shared wind field's frame words into the next
    /// [`Lighting::set_scene_lighting`] write (the previous time comes from the last
    /// fold, so motion evaluates last frame's wind exactly).
    pub(crate) fn set_frame_wind(
        &mut self,
        dir_speed_gust: Vec4,
        params: Vec4,
        meta: UVec4,
        time_s: f32,
        sources: u64,
    ) {
        let previous = self.frame_wind_time.x;
        self.frame_wind_dir_speed_gust = dir_speed_gust;
        self.frame_wind_params = params;
        self.frame_wind_meta = meta;
        self.frame_wind_time = Vec4::new(time_s, previous, 0.0, 0.0);
        self.frame_wind_sources = sources;
    }

    /// The frame slot's light UBO buffer + size (the VSM demand pass binds it).
    pub(crate) fn frame_ubo(&self, frame: usize) -> (vk::Buffer, u64) {
        let ubo = &self.frames[frame].light_ubo;
        (ubo.handle(), ubo.size())
    }

    /// Folds the frame's directional virtual-shadow space into the next
    /// [`Lighting::set_scene_lighting`] write: the light basis, per-level snapped
    /// windows, depth span, and the frame's page-table address.
    pub(crate) fn set_frame_vsm(
        &mut self,
        basis: Mat4,
        levels: [Vec4; crate::VSM_DIRECTIONAL_LEVELS as usize],
        params: Vec4,
        page_table: u64,
    ) {
        self.frame_vsm_basis = basis;
        self.frame_vsm_levels = levels;
        self.frame_vsm_params = params;
        self.frame_vsm_page_table = page_table;
    }

    /// The frame's wind words as the wind deformation prepass push consumes them.
    pub(crate) fn wind_deform_push(&self) -> crate::WindDeformPush {
        crate::WindDeformPush {
            dir_speed_gust: self.frame_wind_dir_speed_gust.to_array(),
            params: self.frame_wind_params.to_array(),
            octaves: self.frame_wind_meta.x,
            seed: self.frame_wind_meta.y,
            time_current: self.frame_wind_time.x,
            time_previous: self.frame_wind_time.y,
            sources: self.frame_wind_sources,
            source_count: self.frame_wind_meta.z,
            reserved: 0,
        }
    }

    pub fn set_frame_ibl(&mut self, ibl_enabled: bool, probe_count: u32) {
        self.frame_ibl_flag = ibl_enabled;
        self.frame_probe_count = probe_count;
    }

    /// Folds this frame's DDGI flag (`screen_flags.z`) + the camera-centered probe-volume
    /// placement (`ddgi_volume_min`/`extent`) + the probe grid (`ddgi_probe_count`) + the toroidal
    /// scroll base (`ddgi_scroll_base`) into the next [`Lighting::set_scene_lighting`] write, so the
    /// mesh fragment samples the DDGI atlases at the right physical tile when the volume ran this
    /// frame. The renderer reads its `Ddgi` sub-state and pushes them here before the UBO write.
    pub fn set_frame_ddgi(
        &mut self,
        ddgi_enabled: bool,
        volume_min: Vec3,
        volume_extent: Vec3,
        probe_count: UVec4,
        scroll_base: UVec4,
    ) {
        self.frame_ddgi_flag = ddgi_enabled;
        self.frame_ddgi_volume_min = volume_min.extend(0.0);
        self.frame_ddgi_volume_extent = volume_extent.extend(0.0);
        self.frame_ddgi_probe_count = probe_count;
        self.frame_ddgi_scroll_base = scroll_base;
    }

    /// Folds this frame's GDF reflection-occlusion enable bit (`sdf_occlusion.y`) into the next
    /// [`Lighting::set_scene_lighting`] write. When enabled the mesh fragment sphere-marches the
    /// Global Distance Field along the reflection vector to occlude the reflected skybox — the one
    /// remaining per-pixel SDF consumer (indirect diffuse occlusion is DDGI ray-miss + GTAO).
    pub fn set_frame_sdf_occlusion(&mut self, reflection_occlusion_enabled: bool) {
        self.frame_sdf_occlusion = UVec4::new(0, u32::from(reflection_occlusion_enabled), 0, 0);
    }

    /// Folds this frame's SSR flag (`extra_flags.x`) into the next
    /// [`Lighting::set_scene_lighting`] write, so the mesh fragment blends the SSR map only
    /// when the trace ran this frame.
    pub fn set_frame_ssr(&mut self, ssr_enabled: bool) {
        self.frame_ssr_flag = ssr_enabled;
    }

    /// Folds this frame's RT-reflection flag (`extra_flags.y`) + the previous frame's
    /// view-proj (for reprojecting an RT hit into `prev_color`) into the next
    /// [`Lighting::set_scene_lighting`] write.
    pub fn set_frame_rt_reflections(&mut self, enabled: bool, prev_view_proj: Mat4) {
        self.frame_rt_reflections_flag = enabled;
        self.frame_prev_view_proj = prev_view_proj;
    }

    /// Folds this frame's froxel volumetric-fog params (`froxel_fog`) into the next
    /// [`Lighting::set_scene_lighting`] write, so the forward transparent path samples the
    /// integration volume only when volumetric fog is authored this frame. `near`/`far` are the
    /// froxel grid's exponential-Z extent (the transparent W mapping inverts them).
    pub fn set_frame_froxel_fog(&mut self, enabled: bool, near: f32, far: f32) {
        self.frame_froxel_fog = Vec4::new(if enabled { 1.0 } else { 0.0 }, near, far, 0.0);
    }

    /// Folds the camera-snapped cloud-shadow projection into the next light UBO write.
    pub(crate) fn set_frame_cloud_shadow(
        &mut self,
        projection: crate::clouds::CloudShadowProjection,
    ) {
        self.frame_cloud_shadow_right = projection.right;
        self.frame_cloud_shadow_up = projection.up;
        self.frame_cloud_shadow_centers = projection.centers;
        self.frame_cloud_shadow_meta = projection.meta;
    }

    /// Binds the one cloud-shadow cascade array for the mesh and fog consumers.
    pub(crate) fn bind_cloud_shadow(
        &self,
        device: &Device,
        view: vk::ImageView,
        sampler: vk::Sampler,
    ) {
        let info = [vk::DescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        for frame in &self.frames {
            let write = vk::WriteDescriptorSet::default()
                .dst_set(frame.light_set)
                .dst_binding(12)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&info);
            unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
        }
    }

    /// Writes the current frame's cluster params from the camera + viewport, and arms
    /// the cull dispatch when clustered is on and at least one punctual light exists.
    ///
    /// The clustered-valid flag (`screen_size.z`) means "the froxel lists are valid this
    /// frame": with zero lights the dispatch is skipped, the buffers hold stale lists,
    /// and the fragment must take the flat loop instead.
    pub fn set_cluster_camera(&mut self, frame: usize, camera: ClusterCamera) {
        let clustered_valid = self.use_clustered && self.frame_light_count > 0;
        let params = ClusterParams {
            view: camera.view,
            inverse_projection: camera.projection.inverse(),
            grid_size: UVec4::new(
                CLUSTER_GRID_X,
                CLUSTER_GRID_Y,
                CLUSTER_GRID_Z,
                self.frame_light_count,
            ),
            screen_size: UVec4::new(camera.width, camera.height, u32::from(clustered_valid), 0),
            z_planes: Vec4::new(camera.near, camera.far, 0.0, 0.0),
        };
        let dst = self.frames[frame]
            .cluster_params
            .mapped_bytes()
            .expect("cluster params UBO is mapped");
        dst[..size_of::<ClusterParams>()].copy_from_slice(bytemuck::bytes_of(&params));
        self.cluster_dispatch_pending = self.use_clustered && self.frame_light_count > 0;
    }

    /// Arms directional shadowing (gated by the master `use_shadows`).
    pub fn set_directional_shadow(&mut self, casting: bool) {
        self.shadow_pending = casting && self.use_shadows;
    }

    /// Sets the shadowed spot light's perspective transform + its index in the per-frame
    /// light list; `casting` arms its virtual-shadow space.
    pub fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool) {
        self.spot_shadow_view_proj = light_view_proj;
        self.spot_shadow_light_index = light_index;
        self.spot_shadow_pending = casting && self.use_shadows;
    }

    /// Sets the shadowed point light's world position + far plane + its index; `casting`
    /// arms its virtual face spaces.
    pub fn set_point_shadow(
        &mut self,
        light_pos: Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
    ) {
        self.point_shadow_pos = light_pos;
        self.point_shadow_far = far_plane;
        self.point_shadow_light_index = light_index;
        self.point_shadow_pending = casting && self.use_shadows;
    }

    /// Ensures the frame's punctual-light SSBO holds at least `count` [`GpuLight`]
    /// elements, growing to the next power of two (never shrinking) and rewriting both
    /// the fragment light set (binding 1) and the compute cluster set (binding 1) — both
    /// read this buffer.
    fn ensure_light_capacity(
        &mut self,
        descriptors: &Descriptors,
        frame: usize,
        count: u32,
    ) -> Result<()> {
        if self.frames[frame].light_list_capacity >= count {
            return Ok(());
        }
        let mut capacity = self.frames[frame]
            .light_list_capacity
            .max(LIGHT_LIST_INITIAL);
        while capacity < count {
            capacity *= 2;
        }
        let size = u64::from(capacity) * size_of::<GpuLight>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        descriptors.write_storage_buffer(
            self.frames[frame].light_set,
            1,
            buffer.handle(),
            buffer.size(),
        );
        descriptors.write_storage_buffer(
            self.frames[frame].cluster_set,
            1,
            buffer.handle(),
            buffer.size(),
        );
        self.frames[frame].light_list = buffer;
        self.frames[frame].light_list_capacity = capacity;
        Ok(())
    }
}

/// Builds one frame slot's lighting buffers + sets, binding the virtual-shadow atlas
/// into the light set and wiring the cluster set's params/light/cluster bindings.
fn build_frame(
    resources: &Arc<DeviceResources>,
    descriptors: &Descriptors,
    vsm_atlas: vk::ImageView,
) -> Result<FrameLighting> {
    let raw = resources.device();

    let light_ubo = make_mapped_uniform_buffer(resources, size_of::<LightUbo>() as vk::DeviceSize)?;
    let light_set = descriptors.allocate_set(descriptors.light_set_layout())?;
    descriptors.write_uniform_buffer(light_set, 0, light_ubo.handle(), light_ubo.size());

    // Binding 13: the virtual-shadow atlas behind its immutable compare sampler. The
    // graph guarantees ShaderReadOnly when the scene samples.
    write_shadow_samplers(raw, light_set, vsm_atlas);

    let light_list = make_mapped_storage_buffer(
        resources,
        u64::from(LIGHT_LIST_INITIAL) * size_of::<GpuLight>() as u64,
    )?;
    descriptors.write_storage_buffer(light_set, 1, light_list.handle(), light_list.size());

    let cluster_buffer =
        make_device_storage_buffer(resources, u64::from(CLUSTER_COUNT) * CLUSTER_STRIDE)?;
    let cluster_params =
        make_mapped_uniform_buffer(resources, size_of::<ClusterParams>() as vk::DeviceSize)?;

    // Light set bindings 2 (cluster lists) + 3 (cluster params).
    descriptors.write_storage_buffer(light_set, 2, cluster_buffer.handle(), cluster_buffer.size());
    descriptors.write_uniform_buffer(light_set, 3, cluster_params.handle(), cluster_params.size());

    // Compute cluster set: params UBO (0) + punctual list read (1) + cluster lists write (2).
    let cluster_set = descriptors.allocate_set(descriptors.cluster_set_layout())?;
    descriptors.write_uniform_buffer(
        cluster_set,
        0,
        cluster_params.handle(),
        cluster_params.size(),
    );
    descriptors.write_storage_buffer(cluster_set, 1, light_list.handle(), light_list.size());
    descriptors.write_storage_buffer(
        cluster_set,
        2,
        cluster_buffer.handle(),
        cluster_buffer.size(),
    );

    Ok(FrameLighting {
        light_set,
        light_ubo,
        light_list,
        light_list_capacity: LIGHT_LIST_INITIAL,
        cluster_set,
        cluster_buffer,
        cluster_params,
    })
}

/// Binds the virtual-shadow atlas (13, behind the layout's immutable compare
/// sampler) into `light_set`.
fn write_shadow_samplers(
    raw: &ash::Device,
    light_set: vk::DescriptorSet,
    vsm_atlas: vk::ImageView,
) {
    let atlas = [vk::DescriptorImageInfo {
        sampler: vk::Sampler::null(),
        image_view: vsm_atlas,
        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    }];
    let writes = [vk::WriteDescriptorSet::default()
        .dst_set(light_set)
        .dst_binding(13)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&atlas)];
    // SAFETY: the ash seam; the set and view outlive the call.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
}

/// A host-visible, persistently-mapped uniform buffer of `size` bytes — the per-frame
/// light + cluster-params UBO backing.
fn make_mapped_uniform_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::Auto,
        flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
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

/// A host-visible, persistently-mapped storage buffer of `size` bytes — the punctual
/// light list backing.
fn make_mapped_storage_buffer(
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
        vk::BufferUsageFlags::STORAGE_BUFFER,
        &alloc_info,
    )
}

/// A device-local storage buffer of `size` bytes — the cluster lists the cull compute
/// writes.
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
        vk::BufferUsageFlags::STORAGE_BUFFER,
        &alloc_info,
    )
}

/// The 6 cube-face world→clip transforms for an omnidirectional point shadow at `pos`
/// with `far_plane`. A 90° perspective per face with the Vulkan Y-flip, in the +X, −X,
/// +Y, −Y, +Z, −Z face order.
pub fn point_shadow_face_matrices(pos: Vec3, far_plane: f32) -> [Mat4; 6] {
    // Cube faces render in the fixed cube-map sampling convention (GL-standard look-at directions +
    // up vectors), which is origin-agnostic — so, unlike the screen and 2D shadow-map projections,
    // the point-shadow projection takes no window Y-flip. Adding one vertically mirrors every face.
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.05, far_plane.max(0.1));
    let fwd = [
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 0.0, -1.0),
    ];
    let up = [
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    ];
    let mut result = [Mat4::IDENTITY; 6];
    for i in 0..6 {
        result[i] = proj * Mat4::look_at_rh(pos, pos + fwd[i], up[i]);
    }
    result
}

/// Maps a screen pixel + Vulkan NDC depth (0 = near) to a view-space point through the
/// clip→view inverse projection — the CPU mirror of `screenToView` in `light_cull.slang`.
/// `inverse_projection` is `proj.inverse()`; `screen` is the offscreen pixel dims.
pub fn screen_to_view(inverse_projection: Mat4, screen: [u32; 2], px: Vec3) -> Vec3 {
    let tex_x = px.x / screen[0] as f32;
    let tex_y = px.y / screen[1] as f32;
    let ndc = Vec4::new(tex_x * 2.0 - 1.0, tex_y * 2.0 - 1.0, px.z, 1.0);
    let view = inverse_projection * ndc;
    view.truncate() / view.w
}

/// Intersects the eye ray through `p` (eye at the view-space origin) with the plane
/// `z = z_dist` — the CPU mirror of `rayToZ` in `light_cull.slang`.
pub fn ray_to_z(p: Vec3, z_dist: f32) -> Vec3 {
    p * (z_dist / p.z)
}

/// Builds the view-space AABB of cluster `(gx, gy, gz)` for `params`, exactly as the
/// cull shader does: the screen tile's near-plane extent, the slice's exponential
/// view-space Z planes, and the four corner rays clamped to each Z. Returns
/// `(aabb_min, aabb_max)`. The CPU mirror of the AABB build in `light_cull.slang`.
pub fn cluster_aabb(params: &ClusterParams, gx: u32, gy: u32, gz: u32) -> (Vec3, Vec3) {
    let screen = [params.screen_size.x, params.screen_size.y];
    let grid = [
        params.grid_size.x as f32,
        params.grid_size.y as f32,
        params.grid_size.z as f32,
    ];
    let tile_w = screen[0] as f32 / grid[0];
    let tile_h = screen[1] as f32 / grid[1];
    let min_ss = (gx as f32 * tile_w, gy as f32 * tile_h);
    let max_ss = ((gx + 1) as f32 * tile_w, (gy + 1) as f32 * tile_h);

    let inv = params.inverse_projection;
    let min_near = screen_to_view(inv, screen, Vec3::new(min_ss.0, min_ss.1, 0.0));
    let max_near = screen_to_view(inv, screen, Vec3::new(max_ss.0, max_ss.1, 0.0));

    let near = params.z_planes.x;
    let far = params.z_planes.y;
    let tile_near = -near * (far / near).powf(gz as f32 / grid[2]);
    let tile_far = -near * (far / near).powf((gz + 1) as f32 / grid[2]);

    let a = ray_to_z(min_near, tile_near);
    let b = ray_to_z(max_near, tile_near);
    let c = ray_to_z(min_near, tile_far);
    let d = ray_to_z(max_near, tile_far);

    let aabb_min = a.min(b).min(c.min(d));
    let aabb_max = a.max(b).max(c.max(d));
    (aabb_min, aabb_max)
}

/// Whether a punctual light at world `light_pos` with `radius` intersects the cluster
/// AABB `[aabb_min, aabb_max]` (both view-space). The light is transformed to view space
/// by `params.view`, then a sphere-vs-AABB test — the CPU mirror of the cull loop's
/// per-light test in `light_cull.slang`.
pub fn light_intersects_cluster(
    params: &ClusterParams,
    light_pos: Vec3,
    radius: f32,
    aabb_min: Vec3,
    aabb_max: Vec3,
) -> bool {
    let pos_view = (params.view * light_pos.extend(1.0)).truncate();
    let closest = pos_view.clamp(aabb_min, aabb_max);
    let delta = pos_view - closest;
    delta.dot(delta) <= radius * radius
}

/// Runs the full clustered cull on the CPU for `params` + `lights`, returning each
/// cluster's light-index list (capped at [`MAX_LIGHTS_PER_CLUSTER`]). This is the exact
/// logic the `light_cull.slang` compute dispatch runs per froxel, extracted as pure CPU
/// code so the cull is unit-testable with no device — the phase's named cull-correctness
/// gate. Cluster index encoding: `x + y*gridX + z*gridX*gridY`.
pub fn cull_clusters_cpu(params: &ClusterParams, lights: &[GpuLight]) -> Vec<Vec<u32>> {
    let gx = params.grid_size.x;
    let gy = params.grid_size.y;
    let gz = params.grid_size.z;
    let total = (gx * gy * gz) as usize;
    let mut out = vec![Vec::new(); total];
    let light_count = params.grid_size.w.min(lights.len() as u32);
    for cluster_index in 0..total as u32 {
        let cx = cluster_index % gx;
        let cy = (cluster_index / gx) % gy;
        let cz = cluster_index / (gx * gy);
        let (aabb_min, aabb_max) = cluster_aabb(params, cx, cy, cz);
        let list = &mut out[cluster_index as usize];
        for i in 0..light_count {
            let light = lights[i as usize];
            let pos = light.position_range.truncate();
            let radius = light.position_range.w;
            if light_intersects_cluster(params, pos, radius, aabb_min, aabb_max)
                && list.len() < MAX_LIGHTS_PER_CLUSTER as usize
            {
                list.push(i);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::pipelines::Pipelines;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use std::mem::offset_of;
    use std::sync::Mutex;

    /// `LightUbo` is exactly 656 bytes with each field at the std140 offset the mesh
    /// fragment reads — the contract the shaded path reads by raw bytes.
    #[test]
    fn light_ubo_byte_layout_matches_std140() {
        assert_eq!(size_of::<LightUbo>(), 832);
        assert_eq!(offset_of!(LightUbo, vsm_basis), 608);
        assert_eq!(offset_of!(LightUbo, vsm_levels), 672);
        assert_eq!(offset_of!(LightUbo, vsm_params), 800);
        assert_eq!(offset_of!(LightUbo, vsm_page_table), 816);
        assert_eq!(align_of::<LightUbo>(), 16);
        assert_eq!(offset_of!(LightUbo, direction_ambient), 0);
        assert_eq!(offset_of!(LightUbo, color_intensity), 16);
        assert_eq!(offset_of!(LightUbo, moon_direction_intensity), 32);
        assert_eq!(offset_of!(LightUbo, moon_color), 48);
        assert_eq!(offset_of!(LightUbo, counts), 64);
        assert_eq!(offset_of!(LightUbo, eye_position), 80);
        assert_eq!(offset_of!(LightUbo, spot_shadow_view_proj), 96);
        assert_eq!(offset_of!(LightUbo, spot_shadow), 160);
        assert_eq!(offset_of!(LightUbo, point_shadow), 176);
        assert_eq!(offset_of!(LightUbo, point_shadow_meta), 192);
        assert_eq!(offset_of!(LightUbo, screen_flags), 208);
        assert_eq!(offset_of!(LightUbo, ddgi_volume_min), 224);
        assert_eq!(offset_of!(LightUbo, ddgi_volume_extent), 240);
        assert_eq!(offset_of!(LightUbo, ddgi_probe_count), 256);
        assert_eq!(offset_of!(LightUbo, ddgi_scroll_base), 272);
        assert_eq!(offset_of!(LightUbo, sdf_occlusion), 288);
        assert_eq!(offset_of!(LightUbo, ambient_color), 304);
        assert_eq!(offset_of!(LightUbo, ibl_tint), 320);
        assert_eq!(offset_of!(LightUbo, extra_flags), 336);
        assert_eq!(offset_of!(LightUbo, prev_view_proj), 352);
        assert_eq!(offset_of!(LightUbo, froxel_fog), 416);
        assert_eq!(offset_of!(LightUbo, cloud_shadow_right), 432);
        assert_eq!(offset_of!(LightUbo, cloud_shadow_up), 448);
        assert_eq!(offset_of!(LightUbo, cloud_shadow_centers), 464);
        assert_eq!(offset_of!(LightUbo, cloud_shadow_meta), 512);
        assert_eq!(offset_of!(LightUbo, wind_dir_speed_gust), 528);
        assert_eq!(offset_of!(LightUbo, wind_params), 544);
        assert_eq!(offset_of!(LightUbo, wind_meta), 560);
        assert_eq!(offset_of!(LightUbo, wind_time), 576);
        assert_eq!(offset_of!(LightUbo, wind_sources), 592);
    }

    /// `ClusterParams` is exactly 192 bytes with each field at the std140 offset both
    /// the cull compute and the mesh fragment read.
    #[test]
    fn cluster_params_byte_layout_matches_std140() {
        assert_eq!(size_of::<ClusterParams>(), 176);
        assert_eq!(align_of::<ClusterParams>(), 16);
        assert_eq!(offset_of!(ClusterParams, view), 0);
        assert_eq!(offset_of!(ClusterParams, inverse_projection), 64);
        assert_eq!(offset_of!(ClusterParams, grid_size), 128);
        assert_eq!(offset_of!(ClusterParams, screen_size), 144);
        assert_eq!(offset_of!(ClusterParams, z_planes), 160);
    }

    /// The cluster constants match the shader (`light_cull.slang`) + the dispatch group
    /// count — a drift here silently mis-sizes the cluster SSBO or culls the wrong grid.
    #[test]
    fn cluster_grid_matches_shader() {
        assert_eq!(CLUSTER_COUNT, 16 * 9 * 24);
        assert_eq!(MAX_LIGHTS_PER_CLUSTER, 64);
        // One cluster's SSBO record: a count u32 + a 64-slot index array.
        assert_eq!(CLUSTER_STRIDE, 4 * (1 + 64));
    }

    /// A standard perspective camera looking down −Z with one viewport-sized grid. The
    /// cull math is built against this fixture in the tests below.
    fn camera_params(lights: u32) -> ClusterParams {
        // A 90° vertical FOV perspective; near 0.1, far 100, 1600×900 (the 16×9 grid's
        // native pixel ratio). The view is identity (camera at the origin looking −Z).
        let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1600.0 / 900.0, 0.1, 100.0);
        ClusterParams {
            view: Mat4::IDENTITY,
            inverse_projection: proj.inverse(),
            grid_size: UVec4::new(CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, lights),
            screen_size: UVec4::new(1600, 900, 1, 0),
            z_planes: Vec4::new(0.1, 100.0, 0.0, 0.0),
        }
    }

    /// The exponential Z slicing places slice 0 at the near plane and the last slice's
    /// far edge at the far plane (the froxel depth partition the cull derives), and every
    /// cluster AABB is non-degenerate (min < max on each axis the slice spans).
    #[test]
    fn cluster_aabbs_partition_the_frustum_depth() {
        let params = camera_params(0);
        // Center column/row cluster at the near and far slices.
        let cx = CLUSTER_GRID_X / 2;
        let cy = CLUSTER_GRID_Y / 2;
        let (near_min, near_max) = cluster_aabb(&params, cx, cy, 0);
        let (far_min, far_max) = cluster_aabb(&params, cx, cy, CLUSTER_GRID_Z - 1);

        // The camera looks down −Z, so the near slice straddles smaller |z| than the far.
        assert!(
            near_max.z <= 0.0 && far_min.z < near_min.z,
            "the far slice is deeper (more negative Z) than the near slice \
             (near_max.z={}, far_min.z={})",
            near_max.z,
            far_min.z
        );
        // The near slice begins at ~−near (exponential slice 0 lower edge = −near).
        assert!(
            (near_max.z - (-params.z_planes.x)).abs() < 1.0,
            "slice 0 starts near the near plane (near_max.z={})",
            near_max.z
        );
        // Every AABB spans a real volume in Z.
        assert!(near_min.z < near_max.z);
        assert!(far_min.z < far_max.z);
    }

    /// A point light placed inside a specific froxel is culled into that cluster (its
    /// list count is ≥ 1) — the cull is correct, not empty. This is the phase's named
    /// cull-fills-a-known-froxel gate, run entirely on the CPU (no device).
    #[test]
    fn cull_fills_the_froxel_containing_a_point_light() {
        let params = camera_params(1);

        // Pick the center cluster at a mid Z slice; place a light at its AABB center.
        let cx = CLUSTER_GRID_X / 2;
        let cy = CLUSTER_GRID_Y / 2;
        let cz = CLUSTER_GRID_Z / 2;
        let (aabb_min, aabb_max) = cluster_aabb(&params, cx, cy, cz);
        let center = (aabb_min + aabb_max) * 0.5;
        // The view is identity, so the view-space center is also the world position.
        let light = GpuLight {
            position_range: center.extend(1.0),
            color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        };

        let clusters = cull_clusters_cpu(&params, &[light]);
        let target = (cx + cy * CLUSTER_GRID_X + cz * CLUSTER_GRID_X * CLUSTER_GRID_Y) as usize;
        assert_eq!(
            clusters[target],
            vec![0],
            "the light landed in its own froxel's list (cluster {target})"
        );

        // A tiny light far outside every froxel touches no cluster (no spurious fill).
        let far_light = GpuLight {
            position_range: Vec3::new(0.0, 0.0, 1000.0).extend(0.01),
            color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        };
        let empty = cull_clusters_cpu(&params, &[far_light]);
        assert!(
            empty.iter().all(Vec::is_empty),
            "a light behind the camera at radius 0.01 lands in no froxel"
        );
    }

    /// A large-radius light intersects many clusters; the per-cluster list never exceeds
    /// the cap, and a cluster within the light's reach records it. The intersection test
    /// is a sphere-vs-AABB, so a light whose sphere overlaps the AABB is recorded.
    #[test]
    fn cull_respects_the_per_cluster_cap_and_records_overlap() {
        let params = camera_params(1);
        // A light at the frustum center with a huge radius reaches many clusters.
        let light = GpuLight {
            position_range: Vec3::new(0.0, 0.0, -10.0).extend(1000.0),
            color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        };
        let clusters = cull_clusters_cpu(&params, &[light]);
        let touched = clusters.iter().filter(|c| !c.is_empty()).count();
        assert!(touched > 0, "a huge light reaches at least one cluster");
        for list in &clusters {
            assert!(
                list.len() <= MAX_LIGHTS_PER_CLUSTER as usize,
                "a cluster never records more than the cap"
            );
        }
    }

    /// The 6 point-shadow face matrices are distinct and project the light's own position
    /// to the clip origin (each face's eye is the light), so the cube renders 6 valid
    /// 90°-FOV views around the light. Pure math, no device.
    #[test]
    fn point_shadow_faces_are_six_distinct_views_centered_on_the_light() {
        let pos = Vec3::new(2.0, 3.0, -4.0);
        let faces = point_shadow_face_matrices(pos, 50.0);
        for (i, face) in faces.iter().enumerate() {
            // The light's own position maps to clip w≈0 (it is the eye), so the
            // homogeneous w is near zero — distinct from any scene point in front.
            let clip = *face * pos.extend(1.0);
            assert!(
                clip.w.abs() < 1e-3,
                "face {i} places its own eye at the camera (w={})",
                clip.w
            );
        }
        // No two faces are the same transform (each looks a different axis).
        for i in 0..6 {
            for j in (i + 1)..6 {
                assert_ne!(
                    faces[i].to_cols_array(),
                    faces[j].to_cols_array(),
                    "faces {i} and {j} look different directions"
                );
            }
        }
    }

    /// A world direction from the light must render onto the *same* cube texel the sampler later
    /// reads for that direction — otherwise shadows land on the wrong side. This pins the face
    /// orientation against the fixed cube-map `(s, t)` selection (Vulkan spec 16.5.4), so a stray
    /// window Y-flip on the projection (which vertically mirrors every face) fails here. Pure math.
    #[test]
    fn point_shadow_faces_match_the_cube_sampling_convention() {
        // The cube-map face + `(s, t)` for a sample direction: major axis picks the face, then
        // `s = (sc/|ma| + 1)/2`, `t = (tc/|ma| + 1)/2`, with `t = 0` at the top of the face image.
        fn cube_st(d: Vec3) -> (usize, f32, f32) {
            let a = d.abs();
            let (face, sc, tc, ma) = if a.x >= a.y && a.x >= a.z {
                if d.x > 0.0 {
                    (0, -d.z, -d.y, d.x)
                } else {
                    (1, d.z, -d.y, -d.x)
                }
            } else if a.y >= a.z {
                if d.y > 0.0 {
                    (2, d.x, d.z, d.y)
                } else {
                    (3, d.x, -d.z, -d.y)
                }
            } else if d.z > 0.0 {
                (4, d.x, -d.y, d.z)
            } else {
                (5, -d.x, -d.y, -d.z)
            };
            (face, (sc / ma + 1.0) * 0.5, (tc / ma + 1.0) * 0.5)
        }

        let faces = point_shadow_face_matrices(Vec3::ZERO, 50.0);
        // One off-centre direction per face (so a mirror is visible), incl. the `-Y` face an
        // overhead light's shadow uses — the exact case that rendered on the wrong side.
        let dirs = [
            Vec3::new(1.0, 0.3, -0.4),
            Vec3::new(-1.0, 0.3, 0.4),
            Vec3::new(0.2, 1.0, 0.5),
            Vec3::new(0.2, -1.0, 0.5),
            Vec3::new(0.3, -0.4, 1.0),
            Vec3::new(0.3, -0.4, -1.0),
        ];
        for dir in dirs {
            let d = dir.normalize();
            let (face, s, t) = cube_st(d);
            // Render a point one unit out from the light along `d` through its face.
            let clip = faces[face] * d.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            // Vulkan framebuffer: NDC `(-1, -1)` is the top-left texel, so `u = (x+1)/2`, `v = (y+1)/2`.
            let u = (ndc.x + 1.0) * 0.5;
            let v = (ndc.y + 1.0) * 0.5;
            assert!(
                (u - s).abs() < 1e-3 && (v - t).abs() < 1e-3,
                "face {face}: rendered texel ({u:.3}, {v:.3}) must match the cube sample ({s:.3}, {t:.3})"
            );
        }
    }

    /// The lighting rig builds against a real device — the per-frame light + cluster sets,
    /// the shadow maps bound into every light set — and tears down validation-clean. This
    /// is the GPU-side acceptance for the descriptor wiring on llvmpipe. Skips when no
    /// Vulkan device is present.
    #[test]
    fn lighting_rig_builds_and_teardown_is_validation_clean() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let vsm = crate::vsm::VsmGpu::new(&device).expect("VsmGpu::new");
        let mut lighting =
            Lighting::new(&device, &descriptors, vsm.atlas.view()).expect("Lighting::new");

        // Defaults: clustered + shadows on, no lights uploaded yet.
        assert!(lighting.use_clustered);
        assert!(lighting.use_shadows);
        assert_eq!(lighting.frame_light_count(), 0);

        // A scene-lighting write with two punctual lights uploads the list + fills the UBO.
        let lights = vec![
            GpuLight {
                position_range: Vec3::new(1.0, 2.0, -3.0).extend(5.0),
                color_intensity: Vec4::new(1.0, 0.5, 0.25, 10.0),
                direction_type: Vec4::ZERO,
                spot_cos: Vec4::ZERO,
            },
            GpuLight {
                position_range: Vec3::new(-2.0, 1.0, -4.0).extend(3.0),
                color_intensity: Vec4::new(0.2, 0.8, 1.0, 6.0),
                direction_type: Vec4::new(0.0, -1.0, 0.0, 1.0),
                spot_cos: Vec4::new(0.9, 0.8, 0.0, 0.0),
            },
        ];
        let scene = SceneLighting {
            lights,
            ..SceneLighting::default()
        };
        lighting
            .set_scene_lighting(&descriptors, 0, &scene)
            .expect("set_scene_lighting");
        assert_eq!(lighting.frame_light_count(), 2);

        // The cluster camera arms a cull dispatch (clustered on + lights present).
        lighting.set_cluster_camera(
            0,
            ClusterCamera {
                view: Mat4::IDENTITY,
                projection: Mat4::perspective_rh(
                    std::f32::consts::FRAC_PI_2,
                    16.0 / 9.0,
                    0.1,
                    100.0,
                ),
                width: 1600,
                height: 900,
                near: 0.1,
                far: 100.0,
            },
        );
        assert!(
            lighting.take_cluster_dispatch_pending(),
            "clustered + lights arms the cull"
        );

        drop(lighting);
        drop(vsm);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the lighting rig's construct + lighting writes + teardown must be \
             validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The light-cull compute dispatch runs on a real device and fills the cluster SSBO:
    /// the froxel containing a known point light reports count ≥ 1, matching the
    /// [`cull_clusters_cpu`] oracle, validation-clean. This is the phase's GPU-runtime
    /// cull gate — compute is fully supported on llvmpipe (no ray-tracing / present).
    /// Skips when no Vulkan device is present.
    #[test]
    fn light_cull_dispatch_fills_the_froxel_on_gpu() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let cull = pipelines
            .request_light_cull()
            .expect("light-cull compute PSO builds on llvmpipe");

        // Place one light at the center of a known froxel (mid grid, mid Z slice).
        let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1600.0 / 900.0, 0.1, 100.0);
        let params = ClusterParams {
            view: Mat4::IDENTITY,
            inverse_projection: proj.inverse(),
            grid_size: UVec4::new(CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, 1),
            screen_size: UVec4::new(1600, 900, 1, 0),
            z_planes: Vec4::new(0.1, 100.0, 0.0, 0.0),
        };
        let cx = CLUSTER_GRID_X / 2;
        let cy = CLUSTER_GRID_Y / 2;
        let cz = CLUSTER_GRID_Z / 2;
        let (aabb_min, aabb_max) = cluster_aabb(&params, cx, cy, cz);
        let center = (aabb_min + aabb_max) * 0.5;
        let light = GpuLight {
            position_range: center.extend(1.0),
            color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        };
        let target = (cx + cy * CLUSTER_GRID_X + cz * CLUSTER_GRID_X * CLUSTER_GRID_Y) as usize;

        let counts = run_cull_dispatch(&device, &descriptors, &cull, &params, &[light])
            .expect("cull dispatch + readback");
        assert!(
            counts[target] >= 1,
            "the GPU cull filled the froxel containing the light (cluster {target} count={})",
            counts[target]
        );
        // The CPU oracle agrees: the same froxel is the (only) one with a fill.
        let oracle = cull_clusters_cpu(&params, &[light]);
        assert_eq!(
            oracle[target],
            vec![0],
            "the CPU oracle agrees the light lands in cluster {target}"
        );

        drop(cull);
        drop(pipelines);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the light-cull dispatch must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Allocates a host-visible params UBO + light SSBO + cluster SSBO, writes a compute
    /// cluster set, runs the cull PSO on a one-off command buffer, and reads back each
    /// cluster's `count` (the first u32 of its `CLUSTER_STRIDE` record). The
    /// GPU-runtime mirror of [`cull_clusters_cpu`].
    fn run_cull_dispatch(
        device: &Device,
        descriptors: &Descriptors,
        cull: &Arc<crate::Pipeline>,
        params: &ClusterParams,
        lights: &[GpuLight],
    ) -> Result<Vec<u32>> {
        let resources = device.resources();
        let raw = device.raw();

        let mut params_buf =
            make_mapped_uniform_buffer(resources, size_of::<ClusterParams>() as vk::DeviceSize)?;
        let params_bytes = bytemuck::bytes_of(params);
        params_buf.mapped_bytes().expect("params mapped")[..params_bytes.len()]
            .copy_from_slice(params_bytes);
        let mut light_buf = make_mapped_storage_buffer(
            resources,
            (lights.len().max(1) * size_of::<GpuLight>()) as vk::DeviceSize,
        )?;
        let light_bytes: &[u8] = bytemuck::cast_slice(lights);
        light_buf.mapped_bytes().expect("light mapped")[..light_bytes.len()]
            .copy_from_slice(light_bytes);
        // Host-visible cluster buffer (the real rig's is device-local; here it is mapped
        // so the test reads the counts back directly).
        let cluster_buf =
            make_mapped_storage_buffer(resources, u64::from(CLUSTER_COUNT) * CLUSTER_STRIDE)?;

        let set = descriptors.allocate_set(descriptors.cluster_set_layout())?;
        descriptors.write_uniform_buffer(set, 0, params_buf.handle(), params_buf.size());
        descriptors.write_storage_buffer(set, 1, light_buf.handle(), light_buf.size());
        descriptors.write_storage_buffer(set, 2, cluster_buf.handle(), cluster_buf.size());

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end of the function.
        let pool = crate::checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = crate::checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = crate::checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let groups = CLUSTER_COUNT.div_ceil(64);
        let record = || -> Result<()> {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the ash seam. The dispatch reads the params/light SSBOs and writes
            // the cluster SSBO; a host-visible buffer needs a host-read barrier after.
            unsafe {
                crate::checked(raw.begin_command_buffer(cmd, &begin), "begin")?;
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, cull.handle());
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    cull.layout(),
                    0,
                    &[set],
                    &[],
                );
                raw.cmd_dispatch(cmd, groups, 1, 1);
                // COMPUTE write → HOST read barrier so the mapped readback sees the cull.
                let barrier = vk::MemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                    .dst_access_mask(vk::AccessFlags2::HOST_READ);
                let barriers = [barrier];
                let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
                raw.cmd_pipeline_barrier2(cmd, &dep);
                crate::checked(raw.end_command_buffer(cmd), "end")?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. Single-threaded queue use in the test.
            unsafe {
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "submit")?;
                crate::checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        };
        let result = record();

        let mut counts = vec![0u32; CLUSTER_COUNT as usize];
        if result.is_ok() {
            let ptr = cluster_buf.mapped_ptr();
            for (i, slot) in counts.iter_mut().enumerate() {
                // The count is the first u32 of each cluster's CLUSTER_STRIDE record.
                let offset = i * CLUSTER_STRIDE as usize;
                // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized CLUSTER_COUNT *
                // CLUSTER_STRIDE; `offset` is within it and 4-byte aligned.
                *slot = unsafe { std::ptr::read_unaligned(ptr.add(offset).cast::<u32>()) };
            }
        }
        // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        result.map(|()| counts)
    }
}
