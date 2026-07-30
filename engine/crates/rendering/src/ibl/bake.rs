//! The bake scaffolding: the compute PSOs, their descriptor layouts and pool, and the
//! barrier / dispatch / descriptor-write helpers every bake step records through.

use super::*;

/// One transient compute pipeline + its layout (freed by [`BakeScratch::drop`]).
pub(super) struct ComputePso {
    pub(super) handle: vk::Pipeline,
    pub(super) layout: vk::PipelineLayout,
}

/// The transient storage views the environment bake writes into. A borrowing param bundle so
/// [`BakeScratch::write_sets`] reads as named fields.
pub(super) struct BakeStorageViews {
    pub(super) generated_env: vk::ImageView,
    pub(super) env: vk::ImageView,
    pub(super) sky_view: vk::ImageView,
}

/// The transient GPU state one bake (or one probe convolve) creates and frees: the command
/// pool + buffer + fence, the descriptor pool + the three set layouts, the compute
/// pipelines, the transient image views, and the allocated sets. A move-only Drop type so
/// every handle is released on the function's exit path — success or error.
pub(super) struct BakeScratch {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) command_pool: vk::CommandPool,
    pub(super) cmd: vk::CommandBuffer,
    pub(super) fence: vk::Fence,
    pub(super) descriptor_pool: vk::DescriptorPool,
    pub(super) layout_a: vk::DescriptorSetLayout,
    pub(super) layout_b: vk::DescriptorSetLayout,
    pub(super) layout_c: Option<vk::DescriptorSetLayout>,
    pub(super) layout_d: Option<vk::DescriptorSetLayout>,
    pub(super) layout_e: vk::DescriptorSetLayout,
    pub(super) transient_views: Vec<vk::ImageView>,

    pub(super) skygen: ComputePso,
    pub(super) equirect: ComputePso,
    pub(super) brdf: ComputePso,
    pub(super) atmos_transmittance: ComputePso,
    pub(super) atmos_multiscatter: ComputePso,
    pub(super) atmos_skyview: ComputePso,
    pub(super) atmos_skygen: ComputePso,
    pub(super) cube_blend: ComputePso,
    pub(super) pipelines: Vec<ComputePso>,

    pub(super) skygen_set: vk::DescriptorSet,
    pub(super) equirect_set: vk::DescriptorSet,
    pub(super) brdf_set: vk::DescriptorSet,
    pub(super) atmos_transmittance_set: vk::DescriptorSet,
    pub(super) atmos_multiscatter_set: vk::DescriptorSet,
    pub(super) atmos_skyview_set: vk::DescriptorSet,
    pub(super) atmos_skygen_set: vk::DescriptorSet,
    pub(super) cube_blend_set: vk::DescriptorSet,
}

impl BakeScratch {
    /// Allocates the bake's command pool/buffer/fence, the transient descriptor pool + the
    /// three set layouts (A = 1 storage, B = sampler + storage, C = 2 samplers + storage),
    /// and every compute pipeline (the atmosphere chain only when `atmosphere`).
    pub(super) fn new(raw: &ash::Device, device: &Device, atmosphere: bool) -> Result<Self> {
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
            layout_d: None,
            layout_e: vk::DescriptorSetLayout::null(),
            transient_views: Vec::new(),
            skygen: ComputePso::null(),
            equirect: ComputePso::null(),
            brdf: ComputePso::null(),
            atmos_transmittance: ComputePso::null(),
            atmos_multiscatter: ComputePso::null(),
            atmos_skyview: ComputePso::null(),
            atmos_skygen: ComputePso::null(),
            cube_blend: ComputePso::null(),
            pipelines: Vec::new(),
            skygen_set: vk::DescriptorSet::null(),
            equirect_set: vk::DescriptorSet::null(),
            brdf_set: vk::DescriptorSet::null(),
            atmos_transmittance_set: vk::DescriptorSet::null(),
            atmos_multiscatter_set: vk::DescriptorSet::null(),
            atmos_skyview_set: vk::DescriptorSet::null(),
            atmos_skygen_set: vk::DescriptorSet::null(),
            cube_blend_set: vk::DescriptorSet::null(),
        };

        scratch.descriptor_pool = create_bake_pool(raw)?;
        scratch.layout_a = create_layout_a(raw)?;
        scratch.layout_b = create_layout_b(raw)?;
        scratch.layout_e = create_layout_e(raw)?;
        if atmosphere {
            scratch.layout_c = Some(create_layout_c(raw)?);
            scratch.layout_d = Some(create_layout_d(raw)?);
        }

        // The compute pipelines. `pipelines` owns each one's Drop; the named fields are
        // copies of the (handle, layout) pair for the dispatch sites (no double-free —
        // only `pipelines` runs the ComputePso Drop, the named copies are `null` Drops).
        let dir = crate::pipelines::resolve_shader_dir();
        scratch.skygen = scratch.add_pipeline(raw, &dir, "ibl_skygen.spv", scratch.layout_a, 32)?;
        scratch.equirect =
            scratch.add_pipeline(raw, &dir, "ibl_equirect.spv", scratch.layout_b, 16)?;
        scratch.brdf = scratch.add_pipeline(raw, &dir, "ibl_brdf.spv", scratch.layout_a, 0)?;
        scratch.cube_blend = scratch.add_pipeline(
            raw,
            &dir,
            "ibl_cube_blend.spv",
            scratch.layout_e,
            size_of::<f32>() as u32,
        )?;
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
            let layout_d = scratch.layout_d.expect("layout_d built for atmosphere");
            scratch.atmos_skygen =
                scratch.add_pipeline(raw, &dir, "atmos_skygen.spv", layout_d, push)?;
        }
        Ok(scratch)
    }

    /// Builds one compute pipeline, pushes the owning `ComputePso` onto `pipelines`, and
    /// returns a (handle, layout) copy for the dispatch site.
    pub(super) fn add_pipeline(
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
    pub(super) fn write_sets(
        &mut self,
        raw: &ash::Device,
        ibl: &Ibl,
        views: &BakeStorageViews,
        atmosphere: bool,
        equirect: bool,
        blend: bool,
    ) -> Result<()> {
        let env_store = views.env;
        self.skygen_set = self.alloc_set(raw, self.layout_a)?;
        self.equirect_set = self.alloc_set(raw, self.layout_b)?;
        self.brdf_set = self.alloc_set(raw, self.layout_a)?;

        write_storage(raw, self.skygen_set, 0, views.generated_env);
        write_storage(raw, self.brdf_set, 0, ibl.brdf_lut.view);

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
            write_storage(raw, self.equirect_set, 1, views.generated_env);
        }

        if atmosphere {
            let layout_c = self.layout_c.expect("layout_c built for atmosphere");
            let layout_d = self.layout_d.expect("layout_d built for atmosphere");
            self.atmos_transmittance_set = self.alloc_set(raw, self.layout_a)?;
            self.atmos_multiscatter_set = self.alloc_set(raw, layout_c)?;
            self.atmos_skyview_set = self.alloc_set(raw, layout_c)?;
            self.atmos_skygen_set = self.alloc_set(raw, layout_d)?;

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
            write_storage(raw, self.atmos_skyview_set, 2, views.sky_view);
            write_sampler(raw, self.atmos_skygen_set, 0, ibl.sampler, views.sky_view);
            write_storage(raw, self.atmos_skygen_set, 1, views.generated_env);
            write_sampler(
                raw,
                self.atmos_skygen_set,
                2,
                ibl.sampler,
                ibl.transmittance_lut.view,
            );
        }
        if blend {
            self.cube_blend_set = self.alloc_set(raw, self.layout_e)?;
            write_sampler(raw, self.cube_blend_set, 0, ibl.sampler, ibl.front.env.view);
            write_sampler(
                raw,
                self.cube_blend_set,
                1,
                ibl.sampler,
                ibl.refresh_target.view,
            );
            write_storage(raw, self.cube_blend_set, 2, env_store);
        }
        Ok(())
    }

    /// Allocates one descriptor set of `layout` from the transient bake pool.
    pub(super) fn alloc_set(
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
            if let Some(layout_d) = self.layout_d.take() {
                raw.destroy_descriptor_set_layout(layout_d, None);
            }
            raw.destroy_descriptor_set_layout(self.layout_e, None);
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
    pub(super) fn null() -> Self {
        Self {
            handle: vk::Pipeline::null(),
            layout: vk::PipelineLayout::null(),
        }
    }
}

/// The transient descriptor pool the bake's sets allocate against (16 storage images +
/// 16 samplers, 32 sets).
pub(super) fn create_bake_pool(raw: &ash::Device) -> Result<vk::DescriptorPool> {
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
pub(super) fn create_layout_a(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_layout_b(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
pub(super) fn create_layout_c(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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

/// Layout D — sky-view sampler (0), env-cube storage output (1), and transmittance sampler (2).
pub(super) fn create_layout_d(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl layoutD",
    )
}

/// Layout E — committed cube (0), refreshed target cube (1), and blended storage cube (2).
pub(super) fn create_layout_e(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
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
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "ibl layoutE",
    )
}

/// Builds a transient compute pipeline from `dir/shader` over `set_layout` with an optional
/// compute-stage push of `push_size` bytes (0 = none). Entry point `computeMain`.
pub(super) fn build_compute_pipeline(
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

pub(super) fn sh_read_barrier(raw: &ash::Device, cmd: vk::CommandBuffer, buffer: vk::Buffer) {
    let barriers = [vk::BufferMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::FRAGMENT_SHADER,
        )
        .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_READ)
        .buffer(buffer)
        .offset(0)
        .size(vk::WHOLE_SIZE)];
    let dependency = vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dependency) };
}

pub(super) fn destroy_compute_pso(raw: &ash::Device, pipeline: &ComputePso) {
    unsafe {
        raw.destroy_pipeline(pipeline.handle, None);
        raw.destroy_pipeline_layout(pipeline.layout, None);
    }
}

pub(super) fn destroy_image_views(raw: &ash::Device, views: &[vk::ImageView]) {
    unsafe {
        for &view in views {
            raw.destroy_image_view(view, None);
        }
    }
}

pub(super) fn destroy_live_layouts(
    raw: &ash::Device,
    sh: vk::DescriptorSetLayout,
    prefilter: vk::DescriptorSetLayout,
) {
    unsafe {
        raw.destroy_descriptor_set_layout(prefilter, None);
        raw.destroy_descriptor_set_layout(sh, None);
    }
}

/// A sync2 image-layout transition over `[base_layer..base_layer+layer_count]` mips
/// `[0..mip_count]`, all 6 cube layers — the bake's per-stage barrier.
#[allow(clippy::too_many_arguments)]
pub(super) fn cube_barrier(
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

pub(super) fn writable_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    initialized: bool,
    mip_count: u32,
) {
    cube_barrier(
        raw,
        cmd,
        image,
        if initialized {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        },
        vk::ImageLayout::GENERAL,
        if initialized {
            vk::PipelineStageFlags2::ALL_COMMANDS
        } else {
            vk::PipelineStageFlags2::TOP_OF_PIPE
        },
        if initialized {
            vk::AccessFlags2::SHADER_SAMPLED_READ
        } else {
            vk::AccessFlags2::empty()
        },
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE,
        0,
        mip_count,
    );
}

pub(super) fn readable_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    mip_count: u32,
) {
    cube_barrier(
        raw,
        cmd,
        image,
        vk::ImageLayout::GENERAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE,
        vk::PipelineStageFlags2::ALL_COMMANDS,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
        0,
        mip_count,
    );
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
pub(super) unsafe fn generate_cube_mips(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    base_size: u32,
    mip_levels: u32,
    initialized: bool,
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
        // Dest mip: discard on first use, otherwise synchronize the prior sampled contents.
        cube_barrier(
            raw,
            cmd,
            image,
            if initialized {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            },
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            if initialized {
                vk::PipelineStageFlags2::ALL_COMMANDS
            } else {
                vk::PipelineStageFlags2::TOP_OF_PIPE
            },
            if initialized {
                vk::AccessFlags2::SHADER_SAMPLED_READ
            } else {
                vk::AccessFlags2::empty()
            },
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
pub(super) fn bind_dispatch(
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
pub(super) fn bind_dispatch_push(
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

/// Writes a storage image into `(set, binding)` in `GENERAL`.
pub(super) fn write_storage(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    view: vk::ImageView,
) {
    crate::vk_write::write_storage_image(raw, set, binding, view, vk::ImageLayout::GENERAL);
}

/// Writes a `SHADER_READ_ONLY`-layout combined image sampler into `(set, binding)`.
pub(super) fn write_sampler(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    sampler: vk::Sampler,
    view: vk::ImageView,
) {
    crate::vk_write::write_combined_sampler(
        raw,
        set,
        binding,
        view,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        sampler,
    );
}
