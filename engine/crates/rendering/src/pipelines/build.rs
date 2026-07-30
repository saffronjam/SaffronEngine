//! Vulkan create-info assembly for the scene raster PSOs: the mesh übershader permutations and
//! the depth, G-buffer, motion, and shadow passes.

use super::*;

impl Pipelines {
    /// Builds one mesh PSO for `key`: loads the shader, sets the unlit spec constant,
    /// selects the vertex or mesh geometry stage, the wireframe polygon mode, and the
    /// dynamic-rendering color/depth formats.
    pub(super) fn build_mesh_pipeline(&self, key: &PsoKey) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module(&key.shader)?;
        // Free the shader module however the rest of this function returns.
        let result = self.build_mesh_pipeline_with_module(raw, key, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing
        // it after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_mesh_pipeline_with_module(
        &self,
        raw: &ash::Device,
        key: &PsoKey,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        // Fragment spec constants: id 0 = unlit branch, id 1 = alpha-to-coverage (masked+MSAA,
        // the fragment sharpens the cutout into per-sample coverage when set), id 2 = translucent
        // (the blend permutation — gates off the opaque G-buffer's screen-space terms and uses
        // world-space indirect instead; see mesh.slang / lighting.slang).
        let unlit_value: vk::Bool32 = u32::from(key.unlit);
        let a2c_value: vk::Bool32 = u32::from(key.alpha_to_coverage);
        let translucent_value: vk::Bool32 = u32::from(key.blend);
        let mut spec_data = [0u8; 12];
        spec_data[0..4].copy_from_slice(&unlit_value.to_ne_bytes());
        spec_data[4..8].copy_from_slice(&a2c_value.to_ne_bytes());
        spec_data[8..12].copy_from_slice(&translucent_value.to_ne_bytes());
        let spec_entries = [
            vk::SpecializationMapEntry::default()
                .constant_id(0)
                .offset(0)
                .size(std::mem::size_of::<vk::Bool32>()),
            vk::SpecializationMapEntry::default()
                .constant_id(1)
                .offset(4)
                .size(std::mem::size_of::<vk::Bool32>()),
            vk::SpecializationMapEntry::default()
                .constant_id(2)
                .offset(8)
                .size(std::mem::size_of::<vk::Bool32>()),
        ];
        let spec_info = vk::SpecializationInfo::default()
            .map_entries(&spec_entries)
            .data(&spec_data);

        let (geometry_stage, geometry_entry): (vk::ShaderStageFlags, &CStr) = if key.mesh_shader {
            (vk::ShaderStageFlags::MESH_EXT, c"meshMainExecutor")
        } else {
            (vk::ShaderStageFlags::VERTEX, c"vertexMainExecutor")
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(geometry_stage)
                .module(module)
                .name(geometry_entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain")
                .specialization_info(&spec_info),
        ];

        // No vertex input: geometry pulls through buffer device addresses.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);

        let polygon_mode = if key.wireframe {
            vk::PolygonMode::LINE
        } else {
            vk::PolygonMode::FILL
        };
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(polygon_mode)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);

        // Alpha-to-coverage: a masked material under MSAA turns its sharpened cutout alpha into
        // a per-sample coverage mask — anti-aliased foliage edges, order-independent, no blending.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(key.sample_count)
            .alpha_to_coverage_enable(key.alpha_to_coverage);

        // Translucent geometry tests against the opaque depth but does not write it (so
        // stacked translucent layers all shade); opaque/masked writes depth as usual.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(!key.blend)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);

        // Opaque/masked: blend off, depth-written. Translucent: straight-alpha `over`
        // (`src.rgb*src.a + dst.rgb*(1-src.a)`), the shader emitting `surf.opacity` in alpha.
        let blend_attachment = [if key.blend {
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)
        } else {
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA)
        }];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);

        // Backface culling is dynamic (set per submesh by the scene pass — two-sided materials cull
        // NONE, solid geometry culls BACK); the baked `cull_mode(NONE)` above is the fallback.
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::CULL_MODE,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // viewProj, plus the mesh executor's command-slice base. The range covers the base for
        // every variant even though only the mesh entry reads it — one push shape across the
        // family is worth four unread bytes, and a range wider than the shader uses is legal.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(if key.mesh_shader {
                vk::ShaderStageFlags::MESH_EXT
            } else {
                vk::ShaderStageFlags::VERTEX
            })
            .offset(0)
            .size(crate::MESH_EXECUTOR_PUSH_SIZE)];

        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&self.set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call (owned by the
        // descriptors sub-state); the layout is owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (mesh)",
        )?;

        let mut pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .push_next(&mut rendering_info)
            .stages(&stages)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        // A mesh pipeline names neither vertex input nor input assembly: the mesh stage produces
        // its own primitives, and supplying either is invalid rather than merely ignored.
        if !key.mesh_shader {
            pipeline_info = pipeline_info
                .vertex_input_state(&vertex_input)
                .input_assembly_state(&input_assembly);
        }

        // SAFETY: the ash seam. The create-info chain outlives the call; the cache
        // (`VK_NULL_HANDLE`) is the no-cache path. On failure the layout is freed.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above and is freed
                // exactly once on the error path.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (mesh)",
                    result,
                });
            }
        };

        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the vertex-only depth pre-pass PSO from the übershader's `vertexMain`:
    /// binding 0 = the base [`Vertex`] stream (position/normal/uv0), no color, depth
    /// `LESS` + write, sets 0/1/2, the viewProj push.
    pub(super) fn build_depth_prepass(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_depth_prepass_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_depth_prepass_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        // Vertex writes depth; the fragment does only the alpha-clip discard (masked materials must
        // not write depth at cutout texels). Opaque materials fall straight through — depth-only.
        let a2c_value: vk::Bool32 = u32::from(self.sample_count != vk::SampleCountFlags::TYPE_1);
        let a2c_entry = [vk::SpecializationMapEntry::default()
            .constant_id(1)
            .offset(0)
            .size(size_of::<vk::Bool32>())];
        let a2c_data = a2c_value.to_ne_bytes();
        let a2c_info = vk::SpecializationInfo::default()
            .map_entries(&a2c_entry)
            .data(&a2c_data);
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMainExecutor"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"depthPrepassFragment")
                .specialization_info(&a2c_info),
        ];

        // No vertex input: geometry pulls through buffer device addresses.
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
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.sample_count)
            .alpha_to_coverage_enable(self.sample_count != vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        // No color attachments — depth only.
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(crate::MESH_EXECUTOR_PUSH_SIZE)];
        // The depth pre-pass binds the same set prefix as the mesh layout (0 bindless,
        // 1 light, 2 instance) so the viewProj push + instance read match the scene pass.
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (depth-prepass)",
        )?;

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
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (depth-prepass)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the TAA reactive-coverage PSO from `mesh.spv` (`vertex_entry` +
    /// `reactiveCoverageFragment`): no vertex input (record-driven pulling), one `R8_UNORM` color
    /// the constant-1.0 fragment writes, depth `LESS_OR_EQUAL` **read-only** (test against the
    /// resolved scene depth so occluded translucents don't mark), single-sampled (the reactive
    /// target + TAA scratch are 1×), cull off (translucents are often double-sided), sets 0/1/2 +
    /// the viewProj push.
    pub(super) fn build_reactive_coverage_entry(
        &self,
        vertex_entry: &'static CStr,
    ) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_reactive_coverage_with_module(raw, module, vertex_entry);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it after
        // creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_reactive_coverage_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
        vertex_entry: &'static CStr,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(vertex_entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"reactiveCoverageFragment"),
        ];

        // Record-driven vertex pulling: no vertex input bindings exist.
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
        // The reactive mask + the TAA scratch it feeds are always single-sampled (TAA and MSAA
        // are mutually exclusive), so this PSO is not sample-count baked.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Read-only depth test against the resolved scene depth: mark only visible translucent
        // fragments (never write, so the scene depth the later passes read is untouched).
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::R)
            .blend_enable(false)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::REACTIVE_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(crate::MESH_EXECUTOR_PUSH_SIZE)];
        // Same set prefix as the mesh layout (0 bindless, 1 light, 2 instance); the coverage pass
        // binds only set 2 + the viewProj push, matching the depth prepass.
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by the
        // returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (reactive-coverage)",
        )?;

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
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the layout is
        // freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (reactive-coverage)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the thin G-buffer prepass PSO from `gbuffer.slang`: binding 0 = the base
    /// [`Vertex`] stream, two colors (`R16G16B16A16_SFLOAT` view normal rgb + view-Z, then
    /// `R8_UNORM` roughness), depth `LESS` + write, single-sampled (the G-buffer is post-resolve),
    /// sets 0/1/2, the `viewProj + view` push.
    pub(super) fn build_gbuffer(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/gbuffer.spv")?;
        let result = self.build_gbuffer_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_gbuffer_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMainExecutor"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        // No vertex input: geometry pulls through buffer device addresses.
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
        // The G-buffer is always single-sampled (the screen-space effects are post-resolve).
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        // Two color attachments (view normal + view-Z, then roughness); blend disabled on both.
        let blend_attachment = [
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA),
            vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(false)
                .color_write_mask(vk::ColorComponentFlags::RGBA),
        ];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::ssao::G_NORMAL_FORMAT, crate::ssao::ROUGHNESS_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // The push is two mat4s (viewProj + view), vertex stage.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)];
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (gbuffer)",
        )?;

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
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (gbuffer)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the motion-vector prepass PSO from `motion.slang`: two vertex bindings (cur
    /// position on binding 0, prev position on binding 1), instanced (sets 0/1/2),
    /// single-sampled (the motion target is 1×), depth `LESS` + write, rg16f color, the
    /// cur/prev viewProj push.
    pub(super) fn build_motion(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/motion.spv")?;
        let result = self.build_motion_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_motion_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMainExecutor"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"fragmentMain"),
        ];

        // No vertex input: both the current and previous micro-vertex streams pull through
        // buffer device addresses.
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
        // Always single-sampled: the motion target is 1×, sampled by the TAA / SSGI resolve.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [crate::aa::MOTION_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)]; // cur + prev viewProj
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (motion)",
        )?;

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
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (motion)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the canonical-coverage, depth-biased shadow PSO from the übershader's
    /// `vertexMain` + `depthPrepassFragment`: binding 0 = the base [`Vertex`] stream,
    /// no color, depth `LESS` + write, dynamic depth-bias, single-sampled, sets 0/1/2,
    /// the light-viewProj push.
    pub(super) fn build_shadow_depth(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/mesh.spv")?;
        let result = self.build_shadow_depth_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_shadow_depth_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(c"vertexMainExecutor"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(c"depthPrepassFragment"),
        ];

        // No vertex input: geometry pulls through buffer device addresses.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        // Depth-biased (set dynamically per shadow pass) to remove shadow acne.
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .depth_bias_enable(true)
            .line_width(1.0);
        // The shadow map is never multisampled.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::DEPTH_BIAS,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(crate::MESH_EXECUTOR_PUSH_SIZE)];
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (shadow)",
        )?;

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
        // SAFETY: the ash seam. The create-info chain outlives the call; on failure the
        // layout is freed exactly once.
        let created = unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        };
        let pipeline = match created {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created above; freed once here.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                return Err(Error::Vk {
                    context: "create_graphics_pipelines (shadow)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }
}
