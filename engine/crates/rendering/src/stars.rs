//! Catalog-driven stars and the equatorial Milky Way cube.

use std::mem::size_of;
use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};

use crate::descriptors::Descriptors;
use crate::resources::{Buffer, DeviceResources, Image};
use crate::{Device, Error, NightSkyParams, Result, Uploader, checked};

const CATALOG_MAGIC: [u8; 8] = *b"SABSC5\0\x01";
const CATALOG_BYTES: &[u8] = include_bytes!("../../../assets/night/bsc5.bin");
const MILKY_WAY_SIZE: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StarGpu {
    direction_luminance: Vec4,
    color: Vec4,
}

const _: () = assert!(size_of::<StarGpu>() == 32);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StarPush {
    view_proj: Mat4,
    world_from_equatorial: Vec4,
    /// `xy` viewport, `z` radiance scale, `w` atmosphere height (`0` disables extinction).
    params: Vec4,
}

const _: () = assert!(size_of::<StarPush>() == 96);

/// Persistent BSC5 star records, their draw pipeline, and the equatorial Milky Way cube.
pub struct StarCatalog {
    resources: Arc<DeviceResources>,
    _catalog_buffer: Buffer,
    count: u32,
    milky_way: Image,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
}

impl StarCatalog {
    /// Builds the catalog SSBO, Milky Way cube, descriptor set, and point-spread draw PSO.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        uploader: &Uploader,
        transmittance_view: vk::ImageView,
        sky_view: vk::ImageView,
        atmosphere_sampler: vk::Sampler,
        sample_count: vk::SampleCountFlags,
    ) -> Result<Self> {
        let records = decode_catalog(CATALOG_BYTES)?;
        let bytes = bytemuck::cast_slice(&records);
        let alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let mut catalog = Buffer::new(
            device.resources(),
            bytes.len() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &alloc,
        )?;
        catalog
            .mapped_bytes()
            .ok_or_else(|| Error::InvalidUploadData("star catalog buffer is not mapped".into()))?
            .copy_from_slice(bytes);

        let milky_way = uploader.upload_cube_float(&build_milky_way_cube(), MILKY_WAY_SIZE)?;
        let raw = device.raw();
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        // SAFETY: the sampler is owned and destroyed by this catalog.
        let sampler = checked(
            unsafe { raw.create_sampler(&sampler_info, None) },
            "createSampler (night sky)",
        )?;

        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: destroyed in Drop.
        let set_layout = match checked(
            unsafe { raw.create_descriptor_set_layout(&layout_info, None) },
            "createDescriptorSetLayout (stars)",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: sampler was created above and has no users yet.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };
        let set = match descriptors.allocate_set(set_layout) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: these handles have no submitted users.
                unsafe {
                    raw.destroy_descriptor_set_layout(set_layout, None);
                    raw.destroy_sampler(sampler, None);
                }
                return Err(err);
            }
        };
        let buffer_info = [vk::DescriptorBufferInfo::default()
            .buffer(catalog.handle())
            .range(catalog.size())];
        let image_infos = [
            vk::DescriptorImageInfo::default()
                .sampler(atmosphere_sampler)
                .image_view(transmittance_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorImageInfo::default()
                .sampler(atmosphere_sampler)
                .image_view(sky_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&buffer_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_infos[0..1]),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_infos[1..2]),
        ];
        // SAFETY: set and backing resources outlive the catalog.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        let (pipeline, pipeline_layout) = match build_pipeline(device, set_layout, sample_count) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                // SAFETY: the set frees with the shared pool; owned handles have no GPU users.
                unsafe {
                    raw.destroy_descriptor_set_layout(set_layout, None);
                    raw.destroy_sampler(sampler, None);
                }
                return Err(err);
            }
        };
        Ok(Self {
            resources: Arc::clone(device.resources()),
            _catalog_buffer: catalog,
            count: records.len() as u32,
            milky_way,
            sampler,
            set_layout,
            set,
            pipeline,
            pipeline_layout,
        })
    }

    /// Number of valid BSC5 records in the GPU table.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Sampling view of the equatorial Milky Way HDR cube.
    pub fn milky_way_view(&self) -> vk::ImageView {
        self.milky_way.view()
    }

    /// Clamp sampler paired with the Milky Way cube.
    pub fn milky_way_sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// Rebuilds the sample-count-baked star PSO after an MSAA change.
    pub fn set_sample_count(
        &mut self,
        device: &Device,
        sample_count: vk::SampleCountFlags,
    ) -> Result<()> {
        let (pipeline, layout) = build_pipeline(device, self.set_layout, sample_count)?;
        // SAFETY: the caller idled the device before changing AA.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
        }
        self.pipeline = pipeline;
        self.pipeline_layout = layout;
        Ok(())
    }

    /// Resolves one instanced star draw for a render-graph body.
    pub fn draw_data(
        &self,
        view_proj: Mat4,
        extent: vk::Extent2D,
        night: NightSkyParams,
    ) -> StarDraw {
        StarDraw {
            pipeline: self.pipeline,
            layout: self.pipeline_layout,
            set: self.set,
            count: self.count,
            push: StarPush {
                view_proj,
                world_from_equatorial: night.world_from_equatorial,
                params: Vec4::new(
                    extent.width as f32,
                    extent.height as f32,
                    night.star_intensity,
                    if night.atmosphere_live {
                        night.atmosphere_height.max(1.0)
                    } else {
                        0.0
                    },
                ),
            },
        }
    }
}

impl Drop for StarCatalog {
    fn drop(&mut self) {
        // SAFETY: renderer teardown idles first; each owned handle is destroyed once.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_set_layout(self.set_layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
    }
}

/// Copy-only state captured by the render-graph star pass.
#[derive(Clone, Copy)]
pub struct StarDraw {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set: vk::DescriptorSet,
    count: u32,
    push: StarPush,
}

/// Records one six-vertex PSF quad per BSC5 catalog row.
pub fn record_stars(raw: &ash::Device, cmd: vk::CommandBuffer, draw: &StarDraw) {
    // SAFETY: pipeline/set/push are live for this frame and the open dynamic-rendering pass.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, draw.pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            draw.layout,
            0,
            &[draw.set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            draw.layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytemuck::bytes_of(&draw.push),
        );
        raw.cmd_draw(cmd, 6, draw.count, 0, 0);
    }
}

fn decode_catalog(bytes: &[u8]) -> Result<Vec<StarGpu>> {
    if bytes.len() < 12 || bytes[..8] != CATALOG_MAGIC {
        return Err(Error::InvalidUploadData(
            "baked star catalog has an invalid header".into(),
        ));
    }
    let count = u32::from_le_bytes(bytes[8..12].try_into().expect("four-byte count")) as usize;
    let expected = 12 + count * size_of::<StarGpu>();
    if bytes.len() != expected {
        return Err(Error::InvalidUploadData(format!(
            "baked star catalog has {} bytes, expected {expected}",
            bytes.len()
        )));
    }
    let mut records = Vec::with_capacity(count);
    for chunk in bytes[12..].chunks_exact(32) {
        let mut values = [0.0_f32; 8];
        for (index, value) in values.iter_mut().enumerate() {
            let begin = index * 4;
            *value = f32::from_le_bytes(chunk[begin..begin + 4].try_into().expect("four bytes"));
        }
        records.push(StarGpu {
            direction_luminance: Vec4::from_array(values[0..4].try_into().expect("four values")),
            color: Vec4::from_array(values[4..8].try_into().expect("four values")),
        });
    }
    Ok(records)
}

fn build_milky_way_cube() -> Vec<f32> {
    let mut pixels = Vec::with_capacity((MILKY_WAY_SIZE * MILKY_WAY_SIZE * 6 * 4) as usize);
    for face in 0..6 {
        for y in 0..MILKY_WAY_SIZE {
            for x in 0..MILKY_WAY_SIZE {
                let uv = Vec3::new(
                    (x as f32 + 0.5) / MILKY_WAY_SIZE as f32 * 2.0 - 1.0,
                    (y as f32 + 0.5) / MILKY_WAY_SIZE as f32 * 2.0 - 1.0,
                    0.0,
                );
                let equatorial = match face {
                    0 => Vec3::new(1.0, -uv.y, -uv.x),
                    1 => Vec3::new(-1.0, -uv.y, uv.x),
                    2 => Vec3::new(uv.x, 1.0, uv.y),
                    3 => Vec3::new(uv.x, -1.0, -uv.y),
                    4 => Vec3::new(uv.x, -uv.y, 1.0),
                    _ => Vec3::new(-uv.x, -uv.y, -1.0),
                }
                .normalize();
                let galactic = Vec3::new(
                    -0.054_875_56 * equatorial.x
                        - 0.873_437_1 * equatorial.y
                        - 0.483_835 * equatorial.z,
                    0.494_109_42 * equatorial.x - 0.444_829_64 * equatorial.y
                        + 0.746_982_2 * equatorial.z,
                    -0.867_666_1 * equatorial.x - 0.198_076_37 * equatorial.y
                        + 0.455_983_8 * equatorial.z,
                );
                let latitude = galactic.z.clamp(-1.0, 1.0).asin();
                let longitude = galactic.y.atan2(galactic.x);
                let narrow = (-latitude.abs() / 0.075).exp();
                let broad = (-latitude.abs() / 0.3).exp();
                let bulge = (-(longitude * longitude) / 0.3 - latitude * latitude / 0.08).exp();
                let noise = (equatorial.dot(Vec3::new(12.9898, 78.233, 45.164)).sin() * 43_758.547)
                    .rem_euclid(1.0);
                let dust = 1.0 - 0.55 * narrow * (0.35 + 0.65 * noise);
                let luminance = (0.1 * broad + 0.8 * narrow + 1.6 * bulge) * dust * 0.000_12;
                let warm = bulge.clamp(0.0, 1.0);
                let color = Vec3::new(0.46, 0.58, 0.9).lerp(Vec3::new(1.0, 0.72, 0.42), warm);
                pixels.extend_from_slice(&[
                    color.x * luminance,
                    color.y * luminance,
                    color.z * luminance,
                    1.0,
                ]);
            }
        }
    }
    pixels
}

fn build_pipeline(
    device: &Device,
    set_layout: vk::DescriptorSetLayout,
    sample_count: vk::SampleCountFlags,
) -> Result<(vk::Pipeline, vk::PipelineLayout)> {
    let raw = device.raw();
    let path = crate::pipelines::resolve_shader_dir().join("stars.spv");
    let bytes = std::fs::read(&path)
        .map_err(|err| Error::ShaderLoad(format!("cannot read '{}': {err}", path.display())))?;
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}'",
            path.display()
        )));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_ne_bytes(chunk.try_into().expect("four bytes")))
        .collect();
    let module_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: module is destroyed after pipeline creation.
    let module = checked(
        unsafe { raw.create_shader_module(&module_info, None) },
        "createShaderModule (stars)",
    )?;
    let push_ranges = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .size(size_of::<StarPush>() as u32)];
    let layouts = [set_layout];
    let layout_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&layouts)
        .push_constant_ranges(&push_ranges);
    // SAFETY: layout inputs are valid; destroyed on failure or by StarCatalog.
    let layout = match checked(
        unsafe { raw.create_pipeline_layout(&layout_info, None) },
        "createPipelineLayout (stars)",
    ) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: module is owned here.
            unsafe { raw.destroy_shader_module(module, None) };
            return Err(err);
        }
    };
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
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(sample_count);
    let depth = vk::PipelineDepthStencilStateCreateInfo::default();
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ZERO)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let formats = [crate::pipelines::OFFSCREEN_COLOR_FORMAT];
    let mut rendering =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .push_next(&mut rendering)
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout);
    // SAFETY: create info references live local arrays; returned pipeline is owned by catalog.
    let created =
        unsafe { raw.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
    // SAFETY: creation has consumed the shader module.
    unsafe { raw.destroy_shader_module(module, None) };
    match created {
        Ok(pipelines) => Ok((pipelines[0], layout)),
        Err((_, result)) => {
            // SAFETY: pipeline layout is owned here on failure.
            unsafe { raw.destroy_pipeline_layout(layout, None) };
            Err(Error::Vk {
                context: "createGraphicsPipelines (stars)",
                result,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_catalog_contains_the_bright_star_catalog() {
        let stars = decode_catalog(CATALOG_BYTES).expect("valid baked BSC5 asset");
        assert_eq!(stars.len(), 9_096);
        assert!(stars.iter().all(|star| star.direction_luminance.w > 0.0));
    }

    #[test]
    fn milky_way_cube_is_hdr_and_nonuniform() {
        let cube = build_milky_way_cube();
        assert_eq!(
            cube.len(),
            (MILKY_WAY_SIZE * MILKY_WAY_SIZE * 6 * 4) as usize
        );
        let peak = cube
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);
        assert!(peak > 0.000_1);
    }
}
