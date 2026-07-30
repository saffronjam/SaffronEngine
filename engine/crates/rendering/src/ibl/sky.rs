//! The visible sky: the fullscreen pass drawn before the scene and its graphics PSO.

use super::*;

/// The visible-sky pass: a fullscreen graphics pass before the scene that fills the scene
/// color target. Procedural mode samples the IBL env cube (set 1), Texture mode a bindless
/// panorama (set 0), Color mode a flat fill.
///
/// Owns its set layout + descriptor set (allocated from the shared pool) + the fullscreen
/// PSO. The PSO bakes the sample count, so it is rebuilt on an AA change.
pub struct Sky {
    pub(super) resources: Arc<DeviceResources>,
    /// 0 = Color, 1 = Texture, 2 = Procedural (matches `SkyMode`).
    pub mode: u32,
    /// Color-mode flat fill (also the sky-pass clear color).
    pub clear_color: Vec3,
    /// Overall sky intensity.
    pub intensity: f32,
    /// Per-frame artist tint.
    pub tint: Vec3,
    /// Yaw rotation (radians).
    pub rotation: f32,
    /// Whether the visible-sky pass runs.
    pub visible: bool,
    /// Bindless panorama slot (Texture mode).
    pub texture_index: u32,
    /// Equatorial night-content orientation and radiance controls.
    pub night: NightSkyParams,
    pub(super) set_layout: vk::DescriptorSetLayout,
    pub(super) set: vk::DescriptorSet,
    pub(super) pipeline: vk::Pipeline,
    pub(super) pipeline_layout: vk::PipelineLayout,
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

        let bindings = [0, 1, 2, 3].map(|binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        });
        let binding_flags = [vk::DescriptorBindingFlags::UPDATE_AFTER_BIND; 4];
        let mut flags_info =
            vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&binding_flags);
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default()
            .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
            .bindings(&bindings)
            .push_next(&mut flags_info);
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
            tint: Vec3::ONE,
            rotation: 0.0,
            visible: true,
            texture_index: 0,
            night: NightSkyParams::default(),
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
        self.tint = settings.tint;
        self.rotation = settings.rotation;
        self.visible = settings.visible;
        self.texture_index = settings.texture_index;
        self.night = settings.night;
    }

    /// Writes the env-cube descriptor (set 1, binding 0) so the procedural-sky pass samples
    /// the same cube the IBL bake produced, then marks the sky ready. Called once after the
    /// first IBL bake.
    pub fn bind_env_cube(&mut self, ibl: &Ibl) {
        let info = [vk::DescriptorImageInfo::default()
            .sampler(ibl.sampler)
            .image_view(ibl.front.env.view)
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

    /// Binds the persistent Milky Way cube and the atmosphere LUTs used for extinction.
    pub fn bind_night_sky(&mut self, ibl: &Ibl, catalog: &crate::StarCatalog) {
        let infos = [
            vk::DescriptorImageInfo::default()
                .sampler(catalog.milky_way_sampler())
                .image_view(catalog.milky_way_view())
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorImageInfo::default()
                .sampler(ibl.sampler())
                .image_view(ibl.transmittance_view())
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorImageInfo::default()
                .sampler(ibl.sampler())
                .image_view(ibl.sky_view_lut_view())
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[0..1]),
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[1..2]),
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[2..3]),
        ];
        // SAFETY: the catalog and IBL resources outlive this descriptor set.
        unsafe { self.resources.device().update_descriptor_sets(&writes, &[]) };
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
    /// aggregate, so the sky pass captures a [`SkyDraw`] instead.
    ///
    /// `mode_override` forces the visible-sky mode for this draw (the offscreen thumbnail view
    /// passes the studio-gradient mode so the backdrop is independent of the IBL); `None` uses the
    /// submitted scene mode.
    pub fn draw_data(
        &self,
        view_proj: saffron_geometry::glam::Mat4,
        mode_override: Option<u32>,
    ) -> SkyDraw {
        let mode = mode_override.unwrap_or(self.mode);
        let color_or_tint = match mode {
            0 => self.clear_color * self.tint,
            3 => Vec3::ONE,
            _ => self.tint,
        };
        SkyDraw {
            pipeline: self.pipeline,
            layout: self.pipeline_layout,
            set: self.set,
            push: SkyPush {
                inv_view_proj: view_proj.inverse(),
                params: Vec4::new(
                    self.intensity,
                    self.rotation,
                    mode as f32,
                    self.texture_index as f32,
                ),
                clear_color: color_or_tint.extend(1.0),
                world_from_equatorial: self.night.world_from_equatorial,
                night: Vec4::new(
                    self.night.milky_way_intensity,
                    self.night.atmosphere_height,
                    if self.night.atmosphere_live { 1.0 } else { 0.0 },
                    0.0,
                ),
            },
        }
    }

    /// The submitted night-sky state used by the instanced star pass.
    pub fn night(&self) -> NightSkyParams {
        self.night
    }
}

/// The resolved fullscreen-sky draw a render-graph pass body captures: the PSO, its layout,
/// the env-cube set, and the per-frame push. All `Copy`, so the `'static` closure holds no
/// borrow of the renderer.
#[derive(Clone, Copy)]
pub struct SkyDraw {
    pub(super) pipeline: vk::Pipeline,
    pub(super) layout: vk::PipelineLayout,
    pub(super) set: vk::DescriptorSet,
    pub(super) push: SkyPush,
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

/// Builds the fullscreen sky PSO from `sky.slang`: no vertex input, a triangle-list
/// fullscreen triangle, no depth test/write, the scene color format, sets 0 (bindless) + 1
/// (env cube), the `SkyPush` in the fragment stage, the scene's sample count baked in.
/// Returns `(pipeline, layout)`.
pub(super) fn build_sky_pipeline(
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
pub(super) fn create_ibl_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
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
