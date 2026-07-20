//! Persistent volumetric-cloud shape resources and their GPU-authored weather field.

use std::mem::size_of;
use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};

use crate::froxel_fog::create_compute_layout;
use crate::{Buffer, Device, GpuTexture, Image, Image3D, ImageDesc, Pipeline, Pipelines, checked};

/// Width, height, and depth of the channel-packed Perlin-Worley base field.
pub const CLOUD_BASE_DIM: u32 = 128;
/// Width, height, and depth of the high-frequency Worley erosion field.
pub const CLOUD_DETAIL_DIM: u32 = 32;
/// Width and height of the curl-warp field.
pub const CLOUD_CURL_DIM: u32 = 128;
/// Width and height of the single sampled weather map.
pub const CLOUD_WEATHER_DIM: u32 = 128;
/// Edge length of each cloud-shadow cascade.
pub const CLOUD_SHADOW_DIM: u32 = 512;
/// Concentric cloud-shadow cascade count.
pub const CLOUD_SHADOW_CASCADES: u32 = 3;

const CLOUD_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// Scene-authored cloud density settings resolved for the renderer, including the optional loaded
/// weather-map override. Shape is shared by the density debugger and the production cloud march.
#[derive(Clone)]
pub struct CloudRenderSettings {
    /// Whether cloud density is evaluated.
    pub enabled: bool,
    /// Global coverage multiplier.
    pub coverage: f32,
    /// Stratus-to-cumulonimbus profile bias.
    pub cloud_type: f32,
    /// Precipitation bias carried by the weather map.
    pub precipitation: f32,
    /// Cumulonimbus anvil spread.
    pub anvil_bias: f32,
    /// Cloud-layer bottom in world metres.
    pub layer_altitude: f32,
    /// Cloud-layer thickness in metres.
    pub layer_height: f32,
    /// World-to-base-noise frequency.
    pub base_scale: f32,
    /// World-to-detail-noise frequency.
    pub detail_scale: f32,
    /// Detail value-erosion strength.
    pub detail_strength: f32,
    /// Curl displacement in world metres.
    pub curl_strength: f32,
    /// World-XZ-to-weather-map frequency.
    pub weather_scale: f32,
    /// Weather-map scroll offset.
    pub weather_offset: Vec3,
    /// Painted weather-map asset id, or zero for procedural weather.
    pub weather_texture_id: u64,
    /// Loaded painted weather map, when present.
    pub weather_texture: Option<Arc<GpuTexture>>,
    /// Maximum adaptive view-ray samples.
    pub primary_steps: u32,
    /// Cone-march samples toward the sun.
    pub light_steps: u32,
    /// Water-droplet diameter in micrometres for the analytic Mie phase fit.
    pub droplet_diameter: f32,
    /// Fresh-sample weight used by temporal reconstruction.
    pub temporal_factor: f32,
    /// Whether the density-integrated cloud shadow is active.
    pub cast_cloud_shadows: bool,
    /// Cloud-shadow strength for clouds and volumetric fog.
    pub cloud_shadow_strength: f32,
    /// Cloud-shadow strength on opaque surfaces.
    pub cloud_shadow_on_surface_strength: f32,
    /// Horizontal wind direction in degrees, clockwise from world +Z.
    pub wind_orientation: f32,
    /// Mean wind speed in metres per second.
    pub wind_speed: f32,
    /// Curl-warp turbulence amplitude.
    pub wind_gust: f32,
    /// Phase-3 normalized time-of-day scalar.
    pub time_of_day: f32,
}

/// Per-view, per-frame camera and physically-coupled lighting state for the cloud passes.
pub(crate) struct CloudFrameState {
    pub inv_view_proj: Mat4,
    pub prev_view_proj: Mat4,
    pub camera: Vec3,
    pub sun_direction: Vec3,
    pub sun_color: Vec3,
    pub sun_intensity: f32,
    pub moon_direction: Vec3,
    pub moon_color: Vec3,
    pub moon_intensity: f32,
    pub planet_radius: f32,
    pub atmosphere_height: f32,
    pub jitter_index: u32,
    pub reduced_extent: vk::Extent2D,
    pub history_valid: bool,
    pub atmosphere_live: bool,
}

/// Camera-snapped concentric cloud-shadow projection shared by cloud fill and light consumers.
#[derive(Clone, Copy)]
pub(crate) struct CloudShadowProjection {
    pub right: Vec4,
    pub up: Vec4,
    pub centers: [Vec4; CLOUD_SHADOW_CASCADES as usize],
    pub light: Vec4,
    pub meta: Vec4,
}

/// Stable image bindings for one viewport's cloud passes.
pub(crate) struct CloudViewBindings {
    pub color: vk::ImageView,
    pub depth: vk::ImageView,
    pub motion: vk::ImageView,
    pub reduced: [vk::ImageView; 2],
    pub reduced_depth: vk::ImageView,
    pub full_color: vk::ImageView,
    pub full_depth: vk::ImageView,
}

impl Default for CloudRenderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            coverage: 0.5,
            cloud_type: 0.4,
            precipitation: 0.0,
            anvil_bias: 0.0,
            layer_altitude: 1500.0,
            layer_height: 2500.0,
            base_scale: 8.0e-5,
            detail_scale: 1.0e-3,
            detail_strength: 0.35,
            curl_strength: 120.0,
            weather_scale: 2.0e-5,
            weather_offset: Vec3::ZERO,
            weather_texture_id: 0,
            weather_texture: None,
            primary_steps: 64,
            light_steps: 6,
            droplet_diameter: 20.0,
            temporal_factor: 0.1,
            cast_cloud_shadows: true,
            cloud_shadow_strength: 1.0,
            cloud_shadow_on_surface_strength: 1.0,
            wind_orientation: 0.0,
            wind_speed: 10.0,
            wind_gust: 0.25,
            time_of_day: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WeatherKey {
    coverage: f32,
    cloud_type: f32,
    precipitation: f32,
    weather_scale: f32,
    weather_offset: Vec3,
    texture_id: u64,
}

impl From<&CloudRenderSettings> for WeatherKey {
    fn from(settings: &CloudRenderSettings) -> Self {
        Self {
            coverage: settings.coverage,
            cloud_type: settings.cloud_type,
            precipitation: settings.precipitation,
            weather_scale: settings.weather_scale,
            weather_offset: settings.weather_offset,
            texture_id: settings.weather_texture_id,
        }
    }
}

/// GPU uniform consumed by the procedural weather fill and the shared density sampler.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CloudParams {
    inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    camera_coverage: [f32; 4],
    layer: [f32; 4],
    shape: [f32; 4],
    weather: [f32; 4],
    weather_offset: [f32; 4],
    sun_direction_intensity: [f32; 4],
    sun_color_droplet: [f32; 4],
    march: [f32; 4],
    frame: [u32; 4],
    atmosphere: [f32; 4],
    moon_direction_intensity: [f32; 4],
    moon_color_shadow: [f32; 4],
    wind_time: [f32; 4],
    shadow: [f32; 4],
    shadow_right: [f32; 4],
    shadow_up: [f32; 4],
    shadow_centers: [[f32; 4]; CLOUD_SHADOW_CASCADES as usize],
    shadow_light: [f32; 4],
}

impl CloudParams {
    fn new(state: &CloudFrameState, settings: &CloudRenderSettings) -> Self {
        let projection = cloud_shadow_projection(
            state.camera,
            state.sun_direction,
            state.sun_intensity,
            state.moon_direction,
            state.moon_intensity,
            settings,
        );
        Self {
            inv_view_proj: state.inv_view_proj.to_cols_array_2d(),
            prev_view_proj: state.prev_view_proj.to_cols_array_2d(),
            camera_coverage: [
                state.camera.x,
                state.camera.y,
                state.camera.z,
                settings.coverage,
            ],
            layer: [
                settings.layer_altitude,
                settings.layer_height,
                settings.cloud_type,
                settings.precipitation,
            ],
            shape: [
                settings.anvil_bias,
                settings.base_scale,
                settings.detail_scale,
                settings.detail_strength,
            ],
            weather: [
                settings.curl_strength,
                settings.weather_scale,
                if settings.enabled { 1.0 } else { 0.0 },
                if settings.weather_texture.is_some() {
                    1.0
                } else {
                    0.0
                },
            ],
            weather_offset: [
                settings.weather_offset.x,
                settings.weather_offset.y,
                settings.weather_offset.z,
                0.0,
            ],
            sun_direction_intensity: [
                state.sun_direction.x,
                state.sun_direction.y,
                state.sun_direction.z,
                state.sun_intensity,
            ],
            sun_color_droplet: [
                state.sun_color.x,
                state.sun_color.y,
                state.sun_color.z,
                settings.droplet_diameter,
            ],
            march: [
                settings.primary_steps as f32,
                settings.light_steps as f32,
                settings.temporal_factor,
                if state.history_valid { 1.0 } else { 0.0 },
            ],
            frame: [
                state.jitter_index,
                state.reduced_extent.width,
                state.reduced_extent.height,
                if state.atmosphere_live { 1 } else { 0 },
            ],
            atmosphere: [state.planet_radius, state.atmosphere_height, 1.0e-3, 0.0],
            moon_direction_intensity: [
                state.moon_direction.x,
                state.moon_direction.y,
                state.moon_direction.z,
                state.moon_intensity,
            ],
            moon_color_shadow: [
                state.moon_color.x,
                state.moon_color.y,
                state.moon_color.z,
                if settings.cast_cloud_shadows {
                    1.0
                } else {
                    0.0
                },
            ],
            wind_time: [
                settings.wind_orientation.to_radians().sin(),
                settings.wind_orientation.to_radians().cos(),
                settings.wind_speed,
                settings.time_of_day.rem_euclid(1.0) * 86_400.0,
            ],
            shadow: [
                settings.wind_gust,
                settings.cloud_shadow_strength,
                settings.cloud_shadow_on_surface_strength,
                0.0,
            ],
            shadow_right: projection.right.to_array(),
            shadow_up: projection.up.to_array(),
            shadow_centers: projection.centers.map(|center| center.to_array()),
            shadow_light: projection.light.to_array(),
        }
    }
}

fn cloud_shadow_projection(
    camera: Vec3,
    sun_direction: Vec3,
    sun_intensity: f32,
    moon_direction: Vec3,
    moon_intensity: f32,
    settings: &CloudRenderSettings,
) -> CloudShadowProjection {
    let key_light = if sun_direction.y > 0.0 && sun_intensity > 0.0 {
        sun_direction
    } else if moon_direction.y > 0.0 && moon_intensity > 0.0 {
        moon_direction
    } else {
        Vec3::Y
    };
    let right = {
        let axis = Vec3::Y.cross(key_light).normalize_or_zero();
        if axis.length_squared() > 0.0 {
            axis
        } else {
            Vec3::X
        }
    };
    let up = key_light.cross(right).normalize_or_zero();
    let extents = [8_000.0, 32_000.0, 128_000.0];
    let centers = extents.map(|extent| {
        let texel = 2.0 * extent / CLOUD_SHADOW_DIM as f32;
        let right_pos = (camera.dot(right) / texel).round() * texel;
        let up_pos = (camera.dot(up) / texel).round() * texel;
        let light_pos = camera.dot(key_light);
        (right * right_pos + up * up_pos + key_light * light_pos).extend(extent)
    });
    CloudShadowProjection {
        right: right.extend(0.0),
        up: up.extend(0.0),
        centers,
        light: key_light.extend(6.0),
        meta: Vec4::new(
            6.0,
            settings.cloud_shadow_strength,
            settings.cloud_shadow_on_surface_strength,
            if settings.enabled && settings.cast_cloud_shadows {
                1.0
            } else {
                0.0
            },
        ),
    }
}

/// Persistent channel-packed cloud noise, curl, and weather-map images plus the descriptors shared
/// by the unlit density debugger and the later lit cloud march.
pub struct Clouds {
    raw: ash::Device,
    base_noise: Image3D,
    detail_noise: Image3D,
    curl_noise: Image,
    weather_map: Image,
    cloud_shadow: Image,
    params: Buffer,
    params_stride: u64,
    repeat_sampler: vk::Sampler,
    linear_sampler: vk::Sampler,
    depth_sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    debug_layout: vk::DescriptorSetLayout,
    weather_layout: vk::DescriptorSetLayout,
    raymarch_layout: vk::DescriptorSetLayout,
    reconstruct_layout: vk::DescriptorSetLayout,
    upscale_layout: vk::DescriptorSetLayout,
    shadow_layout: vk::DescriptorSetLayout,
    debug_sets: Vec<vk::DescriptorSet>,
    weather_set: vk::DescriptorSet,
    shadow_set: vk::DescriptorSet,
    raymarch_sets: Vec<[vk::DescriptorSet; 2]>,
    reconstruct_sets: Vec<[vk::DescriptorSet; 2]>,
    upscale_sets: Vec<[vk::DescriptorSet; 2]>,
    view_count: usize,
    settings: CloudRenderSettings,
    weather_key: WeatherKey,
    weather_source_view: vk::ImageView,
    weather_dirty: bool,
}

impl Clouds {
    /// Allocates and GPU-bakes the static noise fields, then creates one persistent debug set per
    /// viewport plus the weather-fill set. Every image rests in `SHADER_READ_ONLY_OPTIMAL`.
    pub fn new(
        device: &Device,
        pipelines: &Pipelines,
        ibl: &crate::Ibl,
        view_count: usize,
    ) -> crate::Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = device.raw().clone();
        let usage = vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED;
        let mut base_noise = Image3D::new(
            &resources,
            vk::Extent3D {
                width: CLOUD_BASE_DIM,
                height: CLOUD_BASE_DIM,
                depth: CLOUD_BASE_DIM,
            },
            CLOUD_FORMAT,
            1,
            usage,
        )?;
        let mut detail_noise = Image3D::new(
            &resources,
            vk::Extent3D {
                width: CLOUD_DETAIL_DIM,
                height: CLOUD_DETAIL_DIM,
                depth: CLOUD_DETAIL_DIM,
            },
            CLOUD_FORMAT,
            1,
            usage,
        )?;
        let mut curl_noise = Image::new(
            &resources,
            &ImageDesc::color_2d(
                vk::Extent2D {
                    width: CLOUD_CURL_DIM,
                    height: CLOUD_CURL_DIM,
                },
                CLOUD_FORMAT,
                usage,
            ),
        )?;
        let mut weather_map = Image::new(
            &resources,
            &ImageDesc::color_2d(
                vk::Extent2D {
                    width: CLOUD_WEATHER_DIM,
                    height: CLOUD_WEATHER_DIM,
                },
                CLOUD_FORMAT,
                usage,
            ),
        )?;
        let mut cloud_shadow = Image::new(
            &resources,
            &ImageDesc {
                extent: vk::Extent2D {
                    width: CLOUD_SHADOW_DIM,
                    height: CLOUD_SHADOW_DIM,
                },
                format: vk::Format::R16_SFLOAT,
                usage,
                aspect: vk::ImageAspectFlags::COLOR,
                view_type: vk::ImageViewType::TYPE_2D_ARRAY,
                mip_levels: 1,
                array_layers: CLOUD_SHADOW_CASCADES,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;

        bake_static_fields(
            device,
            pipelines,
            &base_noise,
            &detail_noise,
            &curl_noise,
            &weather_map,
            &cloud_shadow,
        )?;
        base_noise.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        detail_noise.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        curl_noise.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        weather_map.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        cloud_shadow.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        let repeat_sampler =
            create_sampler(&raw, vk::Filter::LINEAR, vk::SamplerAddressMode::REPEAT)?;
        let linear_sampler = create_sampler(
            &raw,
            vk::Filter::LINEAR,
            vk::SamplerAddressMode::CLAMP_TO_EDGE,
        )?;
        let depth_sampler = create_sampler(
            &raw,
            vk::Filter::NEAREST,
            vk::SamplerAddressMode::CLAMP_TO_EDGE,
        )?;

        let params_stride = align_up(
            size_of::<CloudParams>() as u64,
            device.capabilities.min_uniform_buffer_offset_alignment,
        );
        let alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let params = Buffer::new(
            &resources,
            params_stride * crate::MAX_FRAMES_IN_FLIGHT as u64 * view_count as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &alloc,
        )?;

        let debug_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (4, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (6, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
            ],
        )?;
        let weather_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
            ],
        )?;
        let raymarch_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::STORAGE_IMAGE),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (4, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (5, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (6, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (7, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (8, vk::DescriptorType::STORAGE_BUFFER),
                (9, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
                (10, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
            ],
        )?;
        let reconstruct_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (4, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
            ],
        )?;
        let upscale_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (4, vk::DescriptorType::STORAGE_IMAGE),
                (5, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
            ],
        )?;
        let shadow_layout = create_compute_layout(
            &raw,
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (2, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (3, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (4, vk::DescriptorType::COMBINED_IMAGE_SAMPLER),
                (5, vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC),
            ],
        )?;
        let pool = create_pool(&raw, view_count)?;
        let mut layouts = vec![debug_layout; view_count];
        layouts.extend(std::iter::repeat_n(raymarch_layout, view_count * 2));
        layouts.extend(std::iter::repeat_n(reconstruct_layout, view_count * 2));
        layouts.extend(std::iter::repeat_n(upscale_layout, view_count * 2));
        layouts.push(weather_layout);
        layouts.push(shadow_layout);
        let alloc_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&alloc_info) },
            "cloud descriptor sets",
        )?;
        let mut sets = sets.into_iter();
        let debug_sets = sets.by_ref().take(view_count).collect();
        let pair_sets = |sets: &mut std::vec::IntoIter<vk::DescriptorSet>| {
            (0..view_count)
                .map(|_| {
                    [
                        sets.next().expect("cloud parity set 0"),
                        sets.next().expect("cloud parity set 1"),
                    ]
                })
                .collect::<Vec<_>>()
        };
        let raymarch_sets = pair_sets(&mut sets);
        let reconstruct_sets = pair_sets(&mut sets);
        let upscale_sets = pair_sets(&mut sets);
        let weather_set = sets.next().expect("weather descriptor set");
        let shadow_set = sets.next().expect("cloud shadow descriptor set");
        debug_assert!(sets.next().is_none());

        let settings = CloudRenderSettings::default();
        let weather_key = WeatherKey::from(&settings);
        let weather_source_view = curl_noise.view();
        let clouds = Self {
            raw,
            base_noise,
            detail_noise,
            curl_noise,
            weather_map,
            cloud_shadow,
            params,
            params_stride,
            repeat_sampler,
            linear_sampler,
            depth_sampler,
            pool,
            debug_layout,
            weather_layout,
            raymarch_layout,
            reconstruct_layout,
            upscale_layout,
            shadow_layout,
            debug_sets,
            weather_set,
            shadow_set,
            raymarch_sets,
            reconstruct_sets,
            upscale_sets,
            view_count,
            settings,
            weather_key,
            weather_source_view,
            weather_dirty: true,
        };
        clouds.write_persistent_descriptors();
        clouds.bind_lighting(ibl);
        Ok(clouds)
    }

    /// Updates the authored settings and the painted-weather source. Weather is refilled only when
    /// one of its own inputs changes.
    pub fn submit(&mut self, settings: CloudRenderSettings) {
        let key = WeatherKey::from(&settings);
        if key != self.weather_key {
            self.weather_key = key;
            self.weather_dirty = true;
        }
        let source_view = settings
            .weather_texture
            .as_ref()
            .map_or(self.curl_noise.view(), |texture| texture.view());
        if self.weather_source_view != source_view {
            self.write_weather_source(settings.weather_texture.as_ref());
            self.weather_source_view = source_view;
            self.weather_dirty = true;
        }
        self.settings = settings;
    }

    /// Binds a viewport's persistent display color and scene depth into its debug descriptor set.
    /// Called at creation and after a resize, while the renderer has idled the device.
    pub(crate) fn bind_view(&self, index: usize, bindings: CloudViewBindings) {
        let CloudViewBindings {
            color,
            depth,
            motion,
            reduced,
            reduced_depth,
            full_color,
            full_depth,
        } = bindings;
        let set = self.debug_sets[index];
        let color_info = [vk::DescriptorImageInfo::default()
            .image_view(color)
            .image_layout(vk::ImageLayout::GENERAL)];
        let depth_info = [vk::DescriptorImageInfo::default()
            .sampler(self.depth_sampler)
            .image_view(depth)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&color_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&depth_info),
        ];
        unsafe { self.raw.update_descriptor_sets(&writes, &[]) };
        for parity in 0..2 {
            let storage = |view| {
                [vk::DescriptorImageInfo::default()
                    .image_view(view)
                    .image_layout(vk::ImageLayout::GENERAL)]
            };
            let sampled = |sampler, view| {
                [vk::DescriptorImageInfo::default()
                    .sampler(sampler)
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            };
            let reduced_storage = storage(reduced[parity]);
            let reduced_depth_storage = storage(reduced_depth);
            let scene_depth = sampled(self.depth_sampler, depth);
            let previous = sampled(self.linear_sampler, reduced[1 - parity]);
            let reduced_depth_sampled = sampled(self.depth_sampler, reduced_depth);
            let motion_sampled = sampled(self.depth_sampler, motion);
            let reduced_sampled = sampled(self.linear_sampler, reduced[parity]);
            let full_color_storage = storage(full_color);
            let full_depth_storage = storage(full_depth);
            let raymarch = self.raymarch_sets[index][parity];
            let reconstruct = self.reconstruct_sets[index][parity];
            let upscale = self.upscale_sets[index][parity];
            let mut writes = Vec::with_capacity(12);
            for (target_set, binding, kind, info) in [
                (
                    raymarch,
                    0,
                    vk::DescriptorType::STORAGE_IMAGE,
                    &reduced_storage,
                ),
                (
                    raymarch,
                    1,
                    vk::DescriptorType::STORAGE_IMAGE,
                    &reduced_depth_storage,
                ),
                (
                    raymarch,
                    2,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &scene_depth,
                ),
                (
                    reconstruct,
                    0,
                    vk::DescriptorType::STORAGE_IMAGE,
                    &reduced_storage,
                ),
                (
                    reconstruct,
                    1,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &previous,
                ),
                (
                    reconstruct,
                    2,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &reduced_depth_sampled,
                ),
                (
                    reconstruct,
                    3,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &motion_sampled,
                ),
                (
                    upscale,
                    0,
                    vk::DescriptorType::STORAGE_IMAGE,
                    &full_color_storage,
                ),
                (
                    upscale,
                    1,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &reduced_sampled,
                ),
                (
                    upscale,
                    2,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &reduced_depth_sampled,
                ),
                (
                    upscale,
                    3,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    &scene_depth,
                ),
                (
                    upscale,
                    4,
                    vk::DescriptorType::STORAGE_IMAGE,
                    &full_depth_storage,
                ),
            ] {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(target_set)
                        .dst_binding(binding)
                        .descriptor_type(kind)
                        .image_info(info),
                );
            }
            unsafe { self.raw.update_descriptor_sets(&writes, &[]) };
        }
    }

    /// Writes this frame slot's camera and shape state into the persistently mapped UBO.
    pub(crate) fn write_params(&mut self, view: usize, frame: usize, state: &CloudFrameState) {
        let params = CloudParams::new(state, &self.settings);
        let offset = self.params_stride as usize * (view * crate::MAX_FRAMES_IN_FLIGHT + frame);
        let src = bytemuck::bytes_of(&params);
        let dst = self
            .params
            .mapped_bytes()
            .expect("cloud params UBO is mapped");
        dst[offset..offset + src.len()].copy_from_slice(src);
    }

    pub(crate) fn shadow_projection(
        &self,
        camera: Vec3,
        sun_direction: Vec3,
        sun_intensity: f32,
        moon_direction: Vec3,
        moon_intensity: f32,
    ) -> CloudShadowProjection {
        cloud_shadow_projection(
            camera,
            sun_direction,
            sun_intensity,
            moon_direction,
            moon_intensity,
            &self.settings,
        )
    }

    /// Dynamic UBO offset for `frame`.
    pub(crate) fn params_offset(&self, view: usize, frame: usize) -> u32 {
        (self.params_stride * (view * crate::MAX_FRAMES_IN_FLIGHT + frame) as u64) as u32
    }

    /// Descriptor set for one viewport's density debugger.
    pub(crate) fn debug_set(&self, view: usize) -> vk::DescriptorSet {
        self.debug_sets[view]
    }

    /// Descriptor set for procedural/painted weather-map resolution.
    pub(crate) fn weather_set(&self) -> vk::DescriptorSet {
        self.weather_set
    }

    /// Descriptor set for the density-integrated cloud-shadow fill.
    pub(crate) fn shadow_set(&self) -> vk::DescriptorSet {
        self.shadow_set
    }

    /// Density-debug compute layout.
    pub(crate) fn debug_layout(&self) -> vk::DescriptorSetLayout {
        self.debug_layout
    }

    /// Weather-fill compute layout.
    pub(crate) fn weather_layout(&self) -> vk::DescriptorSetLayout {
        self.weather_layout
    }

    pub(crate) fn shadow_layout(&self) -> vk::DescriptorSetLayout {
        self.shadow_layout
    }

    pub(crate) fn raymarch_layout(&self) -> vk::DescriptorSetLayout {
        self.raymarch_layout
    }

    pub(crate) fn reconstruct_layout(&self) -> vk::DescriptorSetLayout {
        self.reconstruct_layout
    }

    pub(crate) fn upscale_layout(&self) -> vk::DescriptorSetLayout {
        self.upscale_layout
    }

    pub(crate) fn raymarch_set(&self, view: usize, parity: usize) -> vk::DescriptorSet {
        self.raymarch_sets[view][parity]
    }

    pub(crate) fn reconstruct_set(&self, view: usize, parity: usize) -> vk::DescriptorSet {
        self.reconstruct_sets[view][parity]
    }

    pub(crate) fn upscale_set(&self, view: usize, parity: usize) -> vk::DescriptorSet {
        self.upscale_sets[view][parity]
    }

    /// Current authored settings.
    pub fn settings(&self) -> &CloudRenderSettings {
        &self.settings
    }

    /// Whether the single weather map needs regeneration.
    pub(crate) fn weather_dirty(&self) -> bool {
        self.weather_dirty
    }

    /// Marks the scheduled weather fill current.
    pub(crate) fn mark_weather_clean(&mut self) {
        self.weather_dirty = false;
    }

    pub(crate) fn base_noise(&self) -> &Image3D {
        &self.base_noise
    }

    pub(crate) fn detail_noise(&self) -> &Image3D {
        &self.detail_noise
    }

    pub(crate) fn curl_noise(&self) -> &Image {
        &self.curl_noise
    }

    pub(crate) fn weather_map(&self) -> &Image {
        &self.weather_map
    }

    pub(crate) fn cloud_shadow(&self) -> &Image {
        &self.cloud_shadow
    }

    pub(crate) fn shadow_sampler(&self) -> vk::Sampler {
        self.linear_sampler
    }

    pub(crate) fn set_base_layout(&mut self, layout: vk::ImageLayout) {
        self.base_noise.layout = layout;
    }

    pub(crate) fn set_detail_layout(&mut self, layout: vk::ImageLayout) {
        self.detail_noise.layout = layout;
    }

    pub(crate) fn set_curl_layout(&mut self, layout: vk::ImageLayout) {
        self.curl_noise.layout = layout;
    }

    pub(crate) fn set_weather_layout(&mut self, layout: vk::ImageLayout) {
        self.weather_map.layout = layout;
    }

    pub(crate) fn set_shadow_layout(&mut self, layout: vk::ImageLayout) {
        self.cloud_shadow.layout = layout;
    }

    fn write_persistent_descriptors(&self) {
        let sampled = |view| {
            [vk::DescriptorImageInfo::default()
                .sampler(self.repeat_sampler)
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
        };
        let base = sampled(self.base_noise.view());
        let detail = sampled(self.detail_noise.view());
        let curl = sampled(self.curl_noise.view());
        let weather = sampled(self.weather_map.view());
        let shadow_sampled = [vk::DescriptorImageInfo::default()
            .sampler(self.linear_sampler)
            .image_view(self.cloud_shadow.view())
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let params = [vk::DescriptorBufferInfo::default()
            .buffer(self.params.handle())
            .range(size_of::<CloudParams>() as u64)];
        let mut writes = Vec::with_capacity(self.debug_sets.len() * 5 + self.view_count * 16 + 2);
        for &set in &self.debug_sets {
            for (binding, info) in [(2, &base), (3, &detail), (4, &curl), (5, &weather)] {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(binding)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(info),
                );
            }
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(6)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                    .buffer_info(&params),
            );
        }
        for view in 0..self.view_count {
            for parity in 0..2 {
                let raymarch = self.raymarch_sets[view][parity];
                for (binding, info) in [(3, &base), (4, &detail), (5, &curl), (6, &weather)] {
                    writes.push(
                        vk::WriteDescriptorSet::default()
                            .dst_set(raymarch)
                            .dst_binding(binding)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(info),
                    );
                }
                for (set, binding) in [
                    (raymarch, 9),
                    (self.reconstruct_sets[view][parity], 4),
                    (self.upscale_sets[view][parity], 5),
                ] {
                    writes.push(
                        vk::WriteDescriptorSet::default()
                            .dst_set(set)
                            .dst_binding(binding)
                            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                            .buffer_info(&params),
                    );
                }
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(raymarch)
                        .dst_binding(10)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&shadow_sampled),
                );
            }
        }
        let weather_storage = [vk::DescriptorImageInfo::default()
            .image_view(self.weather_map.view())
            .image_layout(vk::ImageLayout::GENERAL)];
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(self.weather_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&weather_storage),
        );
        let shadow_storage = [vk::DescriptorImageInfo::default()
            .image_view(self.cloud_shadow.view())
            .image_layout(vk::ImageLayout::GENERAL)];
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(self.shadow_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&shadow_storage),
        );
        for (binding, info) in [(1, &base), (2, &detail), (3, &curl), (4, &weather)] {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(self.shadow_set)
                    .dst_binding(binding)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(info),
            );
        }
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(self.shadow_set)
                .dst_binding(5)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                .buffer_info(&params),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(self.weather_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                .buffer_info(&params),
        );
        unsafe { self.raw.update_descriptor_sets(&writes, &[]) };
        self.write_weather_source(None);
    }

    fn bind_lighting(&self, ibl: &crate::Ibl) {
        let transmittance = [vk::DescriptorImageInfo::default()
            .sampler(ibl.sampler())
            .image_view(ibl.transmittance_view())
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let sh = [vk::DescriptorBufferInfo::default()
            .buffer(ibl.sh_coefficients().handle())
            .range(ibl.sh_coefficients().size())];
        let mut writes = Vec::with_capacity(self.view_count * 4);
        for sets in &self.raymarch_sets {
            for &set in sets {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(7)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&transmittance),
                );
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(8)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .buffer_info(&sh),
                );
            }
        }
        unsafe { self.raw.update_descriptor_sets(&writes, &[]) };
    }

    fn write_weather_source(&self, source: Option<&Arc<GpuTexture>>) {
        let view = source.map_or(self.curl_noise.view(), |texture| texture.view());
        let info = [vk::DescriptorImageInfo::default()
            .sampler(self.repeat_sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(self.weather_set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info)];
        unsafe { self.raw.update_descriptor_sets(&write, &[]) };
    }
}

impl Drop for Clouds {
    fn drop(&mut self) {
        unsafe {
            self.raw.destroy_descriptor_pool(self.pool, None);
            self.raw
                .destroy_descriptor_set_layout(self.debug_layout, None);
            self.raw
                .destroy_descriptor_set_layout(self.weather_layout, None);
            self.raw
                .destroy_descriptor_set_layout(self.raymarch_layout, None);
            self.raw
                .destroy_descriptor_set_layout(self.reconstruct_layout, None);
            self.raw
                .destroy_descriptor_set_layout(self.upscale_layout, None);
            self.raw
                .destroy_descriptor_set_layout(self.shadow_layout, None);
            self.raw.destroy_sampler(self.repeat_sampler, None);
            self.raw.destroy_sampler(self.linear_sampler, None);
            self.raw.destroy_sampler(self.depth_sampler, None);
        }
    }
}

fn create_sampler(
    raw: &ash::Device,
    filter: vk::Filter,
    address: vk::SamplerAddressMode,
) -> crate::Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(filter)
        .min_filter(filter)
        .address_mode_u(address)
        .address_mode_v(address)
        .address_mode_w(address);
    checked(unsafe { raw.create_sampler(&info, None) }, "cloud sampler")
}

fn create_pool(raw: &ash::Device, view_count: usize) -> crate::Result<vk::DescriptorPool> {
    let sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count((view_count * 11 + 2) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count((view_count * 31 + 5) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .descriptor_count((view_count * 7 + 2) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count((view_count * 2) as u32),
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets((view_count * 7 + 2) as u32)
        .pool_sizes(&sizes);
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "cloud descriptor pool",
    )
}

fn bake_static_fields(
    device: &Device,
    pipelines: &Pipelines,
    base: &Image3D,
    detail: &Image3D,
    curl: &Image,
    weather: &Image,
    shadow: &Image,
) -> crate::Result<()> {
    let raw = device.raw();
    let layout = create_compute_layout(raw, &[(0, vk::DescriptorType::STORAGE_IMAGE)])?;
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_IMAGE)
        .descriptor_count(3)];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(3)
        .pool_sizes(&sizes);
    let pool = match checked(
        unsafe { raw.create_descriptor_pool(&pool_info, None) },
        "cloud bake descriptor pool",
    ) {
        Ok(pool) => pool,
        Err(err) => {
            unsafe { raw.destroy_descriptor_set_layout(layout, None) };
            return Err(err);
        }
    };
    let result = (|| -> crate::Result<()> {
        let layouts = [layout; 3];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&alloc) },
            "cloud bake descriptor sets",
        )?;
        let infos = [base.view(), detail.view(), curl.view()].map(|view| {
            [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::GENERAL)]
        });
        let writes: Vec<_> = sets
            .iter()
            .zip(&infos)
            .map(|(&set, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(info)
            })
            .collect();
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        let base_pipeline = pipelines.build_compute("shaders/cloud_noise_base.spv", layout, 0)?;
        let detail_pipeline =
            pipelines.build_compute("shaders/cloud_noise_detail.spv", layout, 0)?;
        let curl_pipeline = pipelines.build_compute("shaders/cloud_curl.spv", layout, 0)?;
        record_bake(
            device,
            &[
                (&base_pipeline, sets[0], (16, 16, 16)),
                (&detail_pipeline, sets[1], (4, 4, 4)),
                (&curl_pipeline, sets[2], (16, 16, 1)),
            ],
            &[
                (base.handle(), 1),
                (detail.handle(), 1),
                (curl.handle(), 1),
                (weather.handle(), 1),
                (shadow.handle(), CLOUD_SHADOW_CASCADES),
            ],
        )
    })();
    unsafe {
        raw.destroy_descriptor_pool(pool, None);
        raw.destroy_descriptor_set_layout(layout, None);
    }
    result
}

fn record_bake(
    device: &Device,
    dispatches: &[(&Pipeline, vk::DescriptorSet, (u32, u32, u32))],
    images: &[(vk::Image, u32)],
) -> crate::Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "cloud bake command pool",
    )?;
    let result = (|| -> crate::Result<()> {
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "cloud bake command buffer",
        )?[0];
        let to_general: Vec<_> = images
            .iter()
            .map(|&(image, layer_count)| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count,
                    })
            })
            .collect();
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "cloud bake begin")?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&to_general);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            for &(pipeline, set, groups) in dispatches {
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline.layout(),
                    0,
                    &[set],
                    &[],
                );
                raw.cmd_dispatch(cmd, groups.0, groups.1, groups.2);
            }
            let to_read: Vec<_> = images
                .iter()
                .map(|&(image, layer_count)| {
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count,
                        })
                })
                .collect();
            let dep = vk::DependencyInfo::default().image_memory_barriers(&to_read);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "cloud bake end")?;
        }
        let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmds)];
        device
            .graphics_queue
            .submit2(raw, &submits, vk::Fence::null(), "cloud bake submit")?;
        device.wait_idle()
    })();
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}

const fn align_up(value: u64, align: u64) -> u64 {
    if align <= 1 {
        value
    } else {
        value.div_ceil(align) * align
    }
}
