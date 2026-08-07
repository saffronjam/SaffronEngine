//! The fence-owned off-loop submission that rebuilds dynamic LUTs and the environment cube
//! without stalling the render loop.

use super::*;

/// Persistent realtime sky-lighting state shared by startup and render-graph captures.
pub(super) struct LiveCapture {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) sh_coefficients: Buffer,
    pub(super) prefiltered: IblCube,
    pub(super) prefiltered_layout: vk::ImageLayout,
    pub(super) sh_set_layout: vk::DescriptorSetLayout,
    pub(super) prefilter_set_layout: vk::DescriptorSetLayout,
    pub(super) sh_pipeline: ComputePso,
    pub(super) prefilter_pipeline: ComputePso,
    pub(super) sh_set: vk::DescriptorSet,
    pub(super) prefilter_sets: Vec<vk::DescriptorSet>,
    pub(super) prefilter_views: Vec<vk::ImageView>,
}

impl LiveCapture {
    pub(super) fn new(
        resources: &Arc<DeviceResources>,
        descriptors: &Descriptors,
        sampler: vk::Sampler,
        env_view: vk::ImageView,
        prefilter_mips: u32,
    ) -> Result<Self> {
        let raw = resources.device();
        let sh_coefficients = Buffer::new(
            resources,
            SKY_SH_COEFFICIENTS * size_of::<Vec4>() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            },
        )?;
        let prefiltered = IblCube::new(resources, IBL_PREFILTER_SIZE, prefilter_mips)?;
        let mut prefilter_views = Vec::with_capacity(prefilter_mips as usize);
        for mip in 0..prefilter_mips {
            match prefiltered.storage_view(mip) {
                Ok(view) => prefilter_views.push(view),
                Err(err) => {
                    destroy_image_views(raw, &prefilter_views);
                    return Err(err);
                }
            }
        }

        let sh_set_layout =
            match create_live_capture_layout(raw, vk::DescriptorType::STORAGE_BUFFER) {
                Ok(layout) => layout,
                Err(err) => {
                    destroy_image_views(raw, &prefilter_views);
                    return Err(err);
                }
            };
        let prefilter_set_layout =
            match create_live_capture_layout(raw, vk::DescriptorType::STORAGE_IMAGE) {
                Ok(layout) => layout,
                Err(err) => {
                    destroy_image_views(raw, &prefilter_views);
                    unsafe { raw.destroy_descriptor_set_layout(sh_set_layout, None) };
                    return Err(err);
                }
            };
        let dir = crate::pipelines::resolve_shader_dir();
        let sh_pipeline =
            match build_compute_pipeline(raw, &dir, "sh_project.spv", sh_set_layout, 0) {
                Ok(pipeline) => pipeline,
                Err(err) => {
                    destroy_image_views(raw, &prefilter_views);
                    destroy_live_layouts(raw, sh_set_layout, prefilter_set_layout);
                    return Err(err);
                }
            };
        let prefilter_pipeline = match build_compute_pipeline(
            raw,
            &dir,
            "ibl_prefilter.spv",
            prefilter_set_layout,
            size_of::<PrefilterPush>() as u32,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                destroy_image_views(raw, &prefilter_views);
                destroy_compute_pso(raw, &sh_pipeline);
                destroy_live_layouts(raw, sh_set_layout, prefilter_set_layout);
                return Err(err);
            }
        };

        let sh_set = match descriptors.allocate_set(sh_set_layout) {
            Ok(set) => set,
            Err(err) => {
                destroy_image_views(raw, &prefilter_views);
                destroy_compute_pso(raw, &prefilter_pipeline);
                destroy_compute_pso(raw, &sh_pipeline);
                destroy_live_layouts(raw, sh_set_layout, prefilter_set_layout);
                return Err(err);
            }
        };
        let mut prefilter_sets = Vec::with_capacity(prefilter_mips as usize);
        for _ in 0..prefilter_mips {
            match descriptors.allocate_set(prefilter_set_layout) {
                Ok(set) => prefilter_sets.push(set),
                Err(err) => {
                    destroy_image_views(raw, &prefilter_views);
                    destroy_compute_pso(raw, &prefilter_pipeline);
                    destroy_compute_pso(raw, &sh_pipeline);
                    destroy_live_layouts(raw, sh_set_layout, prefilter_set_layout);
                    return Err(err);
                }
            }
        }

        let sh_info = [vk::DescriptorBufferInfo::default()
            .buffer(sh_coefficients.handle())
            .offset(0)
            .range(sh_coefficients.size())];
        let sh_write = [vk::WriteDescriptorSet::default()
            .dst_set(sh_set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&sh_info)];
        unsafe { raw.update_descriptor_sets(&sh_write, &[]) };
        for (&set, &view) in prefilter_sets.iter().zip(&prefilter_views) {
            write_storage(raw, set, 1, view);
        }

        let capture = Self {
            resources: Arc::clone(resources),
            sh_coefficients,
            prefiltered,
            prefiltered_layout: vk::ImageLayout::UNDEFINED,
            sh_set_layout,
            prefilter_set_layout,
            sh_pipeline,
            prefilter_pipeline,
            sh_set,
            prefilter_sets,
            prefilter_views,
        };
        capture.bind_env(raw, sampler, env_view);
        Ok(capture)
    }

    pub(super) fn bind_env(
        &self,
        raw: &ash::Device,
        sampler: vk::Sampler,
        env_view: vk::ImageView,
    ) {
        write_sampler(raw, self.sh_set, 0, sampler, env_view);
        for &set in &self.prefilter_sets {
            write_sampler(raw, set, 0, sampler, env_view);
        }
    }

    pub(super) fn record_sh(&self, raw: &ash::Device, cmd: vk::CommandBuffer) {
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.sh_pipeline.handle);
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.sh_pipeline.layout,
                0,
                &[self.sh_set],
                &[],
            );
            raw.cmd_dispatch(cmd, 1, 1, 1);
        }
    }

    pub(super) fn record_prefilter_slice(
        &self,
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        mip: u32,
        row_offset: u32,
        row_count: u32,
        blend_alpha: f32,
    ) {
        let size = (IBL_PREFILTER_SIZE >> mip).max(1);
        let push = PrefilterPush {
            roughness: mip as f32 / (IBL_PREFILTER_MIPS - 1) as f32,
            blend_alpha,
            row_offset,
            row_count,
        };
        unsafe {
            raw.cmd_bind_pipeline(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.prefilter_pipeline.handle,
            );
            raw.cmd_push_constants(
                cmd,
                self.prefilter_pipeline.layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(&push),
            );
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.prefilter_pipeline.layout,
                0,
                &[self.prefilter_sets[mip as usize]],
                &[],
            );
            raw.cmd_dispatch(cmd, group(size), group(row_count), 6);
        }
    }

    pub(super) fn record_startup(&mut self, raw: &ash::Device, cmd: vk::CommandBuffer) {
        self.record_sh(raw, cmd);
        sh_read_barrier(raw, cmd, self.sh_coefficients.handle());
        writable_image(raw, cmd, self.prefiltered.image, false, IBL_PREFILTER_MIPS);
        for mip in 0..IBL_PREFILTER_MIPS {
            let size = (IBL_PREFILTER_SIZE >> mip).max(1);
            self.record_prefilter_slice(raw, cmd, mip, 0, size, 1.0);
        }
        readable_image(raw, cmd, self.prefiltered.image, IBL_PREFILTER_MIPS);
        self.prefiltered_layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    }
}

impl Drop for LiveCapture {
    fn drop(&mut self) {
        let raw = self.resources.device();
        destroy_image_views(raw, &self.prefilter_views);
        destroy_compute_pso(raw, &self.prefilter_pipeline);
        destroy_compute_pso(raw, &self.sh_pipeline);
        destroy_live_layouts(raw, self.sh_set_layout, self.prefilter_set_layout);
    }
}

pub(super) fn create_live_capture_layout(
    raw: &ash::Device,
    output_type: vk::DescriptorType,
) -> Result<vk::DescriptorSetLayout> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(output_type)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    let binding_flags = [
        vk::DescriptorBindingFlags::UPDATE_AFTER_BIND,
        vk::DescriptorBindingFlags::empty(),
    ];
    let mut flags_info =
        vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&binding_flags);
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
        .bindings(&bindings)
        .push_next(&mut flags_info);
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl live capture layout",
    )
}
