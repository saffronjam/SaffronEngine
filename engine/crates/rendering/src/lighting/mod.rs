//! The lighting rig: the per-frame directional-light + ambient + eye UBO, the punctual-light
//! storage buffer, the clustered-forward froxel cull state, and the directional / spot / point
//! shadow transforms.
//!
//! The per-frame UBO/SSBO are written on the render thread after the frame's fence is waited, and
//! are frame-indexed, so a host write never races a frame still reading on the GPU. The
//! froxel-assignment math the cull pass runs on the GPU is mirrored here as pure CPU functions
//! ([`cluster_aabb`], [`light_intersects_cluster`]) so the cull is testable with no device.

mod cull;
mod setup;

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, UVec4, Vec3, Vec4};

use crate::clouds::CLOUD_SHADOW_CASCADES;
use crate::descriptors::Descriptors;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::GpuLight;
use crate::resources::{Buffer, DeviceResources};
use crate::{Device, Result};

pub use cull::*;
use setup::*;

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
    frame_rt_shadows_flag: bool,
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
    /// `set_point_shadow`. The renderer compares it against the last rendered key (and the cube.

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
            frame_rt_shadows_flag: false,
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
    /// renderer's life, its prefix rewritten each frame on device by the
    /// `gi-occluder-scatter` pass), so this is a one-time wire-up at construction — the
    /// cone-trace reads the first `sdf_occlusion.x` entries.
    pub fn bind_sdf_instances(
        &self,
        descriptors: &Descriptors,
        buffer: vk::Buffer,
        slot_bytes: vk::DeviceSize,
        meta: vk::Buffer,
        meta_slot_bytes: vk::DeviceSize,
    ) {
        for (slot, frame) in self.frames.iter().enumerate() {
            descriptors.write_storage_buffer_slice(
                frame.light_set,
                8,
                buffer,
                slot as vk::DeviceSize * slot_bytes,
                slot_bytes,
            );
            descriptors.write_storage_buffer_slice(
                frame.light_set,
                15,
                meta,
                slot as vk::DeviceSize * meta_slot_bytes,
                meta_slot_bytes,
            );
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
            // .z = the ray-query shadow gate; .w = the debug view-mode channel the mesh
            // fragment outputs instead of shading.
            point_shadow_meta: UVec4::new(
                self.point_shadow_light_index,
                u32::from(self.point_shadow_pending),
                u32::from(self.frame_rt_shadows_flag),
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

    /// The frame's wind words as the wind deformation prepass push consumes them. The
    /// interaction cascade centres are the renderer's, filled in at the dispatch.
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
            prev_center0: [0; 2],
            prev_center1: [0; 2],
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

    /// Folds this frame's ray-query shadow flag (`point_shadow_meta.z`) into the next
    /// [`Lighting::set_scene_lighting`] write. It gates every punctual and directional
    /// shadow term onto a traced ray instead of the shadow-map sample, so it is only set
    /// when a TLAS was actually built this frame.
    pub fn set_frame_rt_shadows(&mut self, enabled: bool) {
        self.frame_rt_shadows_flag = enabled;
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

#[cfg(test)]
mod tests;
