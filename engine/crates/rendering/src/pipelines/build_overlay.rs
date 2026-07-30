//! Vulkan create-info assembly for the passes drawn on the resolved colour: the ground grid, the
//! editor overlay, the lit-wireframe lines, and the depth upscale that lets them occlude.

use super::*;

impl Pipelines {
    pub(super) fn build_wireframe_overlay(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/wireframe_overlay.spv")?;
        let result = self.build_wireframe_overlay_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    /// Builds the Lit Wireframe overlay PSO: one base vertex stream + the per-instance set,
    /// `PolygonMode::LINE`, depth-tested (`LESS_OR_EQUAL`) without write, single-sampled (it
    /// draws on the 1× resolved color after tonemap), offscreen color, a single `viewProj`
    /// push.
    fn build_wireframe_overlay_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
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

        // Record-driven vertex pulling: no vertex input bindings exist.
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::LINE)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        // The overlay draws on the 1× resolved color after tonemap.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Depth-test against the persisted 1× scene depth so hidden edges are occluded; never
        // write (the scene already laid the depth down).
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(crate::MESH_EXECUTOR_PUSH_SIZE)]; // viewProj
        let set_layouts = &self.set_layouts[..3];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layouts outlive the call; the layout is owned by the
        // returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (wireframe-overlay)",
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
                    context: "create_graphics_pipelines (wireframe-overlay)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds the analytic ground-grid PSO from `grid.slang`: a fullscreen triangle (no
    /// vertex buffer), depth-tested `LESS_OR_EQUAL` without writing (it emits `SV_Depth`
    /// to occlude against the persisted 1× scene depth), alpha-blended over the resolved
    /// color, single-sampled, the `viewProj + invViewProj` push (vertex+fragment), no
    /// descriptor sets.
    pub(super) fn build_depth_upscale(
        &self,
        set_layout: vk::DescriptorSetLayout,
    ) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/depth_upscale.spv")?;
        let result = self.build_depth_upscale_with_module(raw, module, set_layout);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_depth_upscale_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
        set_layout: vk::DescriptorSetLayout,
    ) -> Result<Pipeline> {
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
        // Fullscreen triangle — no vertex buffer.
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
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Depth-write-always: the fragment emits the point-sampled input depth as SV_Depth for
        // every display pixel (compare ALWAYS so the write is unconditional). No color attachment.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::ALWAYS);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        // Depth-only: no color attachment, depth attachment format only.
        let mut rendering_info =
            vk::PipelineRenderingCreateInfo::default().depth_attachment_format(DEPTH_FORMAT);

        // The push is the input depth extent (for the nearest-texel-centre fetch), fragment-only.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(2 * size_of::<f32>() as u32)];
        let set_layouts = [set_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The set layout + push range outlive the call; the layout is
        // owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (depth-upscale)",
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
                    context: "create_graphics_pipelines (depth-upscale)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    pub(super) fn build_grid(&self) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/grid.spv")?;
        let result = self.build_grid_with_module(raw, module);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_grid_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
    ) -> Result<Pipeline> {
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
        // Fullscreen triangle — no vertex buffer.
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
        // 1× post-resolve, like the overlay.
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Test against the persisted 1× scene depth without writing it; the fragment
        // emits SV_Depth so geometry in front of the plane occludes the grid.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [alpha_blend_attachment()];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        // The push is viewProj + invViewProj, read in the vertex AND fragment stages.
        let push_constant = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(2 * size_of::<saffron_geometry::glam::Mat4>() as u32)];
        let layout_info =
            vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push_constant);
        // SAFETY: the ash seam. The push range outlives the call; the layout is owned by
        // the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (grid)",
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
                    context: "create_graphics_pipelines (grid)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }

    /// Builds an editor-overlay PSO from `gizmo_overlay.slang`: the four-attribute
    /// [`crate::OverlayVertex`] stream (position / color / edge / depth), no descriptor
    /// sets, alpha-blended, single-sampled, no depth write. `depth_test` selects the
    /// occluded variant (`LESS_OR_EQUAL` against the scene depth) vs the on-top variant
    /// (no test); both declare the depth format so the PSO stays render-pass compatible
    /// with the overlay pass's depth attachment.
    pub(super) fn build_overlay(&self, depth_test: bool) -> Result<Pipeline> {
        let raw = self.resources.device();
        let module = self.load_shader_module("shaders/gizmo_overlay.spv")?;
        let result = self.build_overlay_with_module(raw, module, depth_test);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freeing it
        // after creation is valid and required.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    fn build_overlay_with_module(
        &self,
        raw: &ash::Device,
        module: vk::ShaderModule,
        depth_test: bool,
    ) -> Result<Pipeline> {
        use crate::overlay::OverlayVertex;

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

        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(size_of::<OverlayVertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, position) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, color) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, edge) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(3)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(std::mem::offset_of!(OverlayVertex, depth) as u32),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

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
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // The depth-tested variant occludes against the scene depth without touching it
        // (LESS_OR_EQUAL matches the scene pass's compare); the on-top variant never
        // tests. Neither writes depth.
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(depth_test)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let blend_attachment = [alpha_blend_attachment()];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        // Both variants run in the overlay pass, which binds a depth attachment; declare
        // its format so the PSO is render-pass compatible even when depth testing is off.
        let color_formats = [OFFSCREEN_COLOR_FORMAT];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(DEPTH_FORMAT);

        let layout_info = vk::PipelineLayoutCreateInfo::default();
        // SAFETY: the ash seam. The layout is owned by the returned `Pipeline`.
        let layout = checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (overlay)",
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
                    context: "create_graphics_pipelines (overlay)",
                    result,
                });
            }
        };
        Ok(Pipeline::from_parts(&self.resources, pipeline, layout))
    }
}
