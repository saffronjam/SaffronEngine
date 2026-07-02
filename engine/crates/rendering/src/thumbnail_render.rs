//! The offscreen thumbnail + material-preview render primitives.
//!
//! The minimal mesh-thumbnail render, the studio-lit
//! material-preview render (default pipeline or a codegen'd material `.spv`), the
//! textured model render, and the downscale-and-read-back PNG encoders. Each render is
//! a self-contained one-off submit — allocate a `size`×`size` (optionally MSAA-resolved)
//! color target + depth, draw the framed geometry, transition the result to
//! `SHADER_READ_ONLY_OPTIMAL`, and hand back an [`Arc`]`<`[`GpuTexture`]`>` the caller
//! reads with [`ThumbnailRenderer::encode_texture_thumbnail_png`].
//!
//! The thumbnail/preview PSOs + the unit sphere are cached on a
//! [`ThumbnailRenderer`] sub-state the [`crate::Renderer`] owns and delegates to.
//! Keeping the sub-state separate from the swapchain-backed `Renderer` lets the render +
//! read-back be exercised against a bare [`Device`] (the headless swapchain WSI crashes
//! lavapipe, but the offscreen render does not).
//!
//! All draws record into a transient one-off command pool and submit on the graphics
//! queue with a fresh fence — the same idle-then-submit discipline as
//! [`crate::Renderer::capture_viewport`]; the thumbnail render runs on the control-drain
//! thread between frames, so the queue is not contended.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat4, UVec4, Vec2, Vec3, Vec4};
use saffron_geometry::{Mesh, Submesh, Vertex};
use vk_mem::Alloc;

use crate::descriptors::Descriptors;
use crate::draw_list::SubmeshMaterial;
use crate::resources::{DeviceResources, GpuMesh, GpuTexture, GpuTextureParts, Pipeline};
use crate::thumbnail::{PngTransfer, format_pixel_bytes};
use crate::{DEFAULT_WHITE_SLOT, Device, Error, GpuQueue, Result, Uploader, checked};

/// Encoded PNG bytes plus the actual encoded pixel dimensions (so a reply reports the
/// truthful width/height rather than the requested size).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailPng {
    /// The encoded PNG bytes.
    pub bytes: Vec<u8>,
    /// The encoded image width.
    pub width: u32,
    /// The encoded image height.
    pub height: u32,
}

/// The material-preview push constant — matches `preview.slang`'s `PreviewPush`
/// (112 bytes: `mat4` + `vec4` + `uvec4` + `vec4`, std140/std430 identical here).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PreviewPush {
    view_proj: Mat4,
    base_color: Vec4,
    /// x = albedo, y = metallic-roughness, z = normal bindless index, w = feature bits.
    tex: UVec4,
    /// x = metallic, y = roughness, z = normalStrength, w = 0.
    pbr: Vec4,
}

/// The mesh-thumbnail push constant — `mvp` + `normalMatrix`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ThumbnailPush {
    mvp: Mat4,
    normal_matrix: Mat4,
}

/// `FEATURE_NORMAL` bit in `PreviewPush::tex.w` (matches `preview.slang`).
const FEATURE_NORMAL: u32 = 1;

/// The offscreen thumbnail + material-preview render sub-state: the lazy thumbnail /
/// preview PSOs + the preview sphere, the render-target color format, and the
/// render-to-texture + PNG read-back primitives.
///
/// Holds an [`Arc`]`<`[`DeviceResources`]`>` so its cached resources free without a live
/// `&Device`. The render entry points take the `&Device` + `&Descriptors` per call (the
/// queue + physical-device probes + bindless set live there). The thumbnail/preview PSOs
/// + the preview sphere are grouped here.
pub struct ThumbnailRenderer {
    resources: Arc<DeviceResources>,
    /// The color attachment format the thumbnail/preview PSOs render into — the
    /// swapchain format (the editor reads thumbnails over the control plane, not the
    /// swapchain, but matching it keeps the PSO render-pass-compatible).
    color_format: vk::Format,
    /// The minimal mesh-thumbnail PSO (vertex input + a 2×mat4 push, no descriptor sets),
    /// built lazily on the first thumbnail render.
    thumbnail_pipeline: Option<Arc<Pipeline>>,
    /// The studio-lit material-preview PSO (binds the bindless set, a 112-byte
    /// `PreviewPush`), built lazily.
    preview_pipeline: Option<Arc<Pipeline>>,
    /// The unit UV sphere the material preview renders, built lazily.
    preview_sphere: Option<Arc<GpuMesh>>,
}

/// A multisampled color target plus its (optional) 1× resolve image and the depth
/// image: the three offscreen attachments every thumbnail/preview render allocates.
struct ThumbnailTargets {
    color: ManagedImage,
    resolve: Option<ManagedImage>,
    depth: ManagedImage,
    samples: vk::SampleCountFlags,
}

/// A raw VMA image + view this module owns and frees itself (or hands its ownership to
/// a [`GpuTexture`]). Distinct from [`crate::Image`] so the render-result image can have
/// its handles moved into a `GpuTexture` without a double free — when `allocation` is
/// taken the [`Drop`] is a no-op.
struct ManagedImage {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: Option<vk_mem::Allocation>,
    extent: vk::Extent2D,
    format: vk::Format,
}

impl ThumbnailRenderer {
    /// Creates the sub-state against the device's shared bundle + the target color format.
    pub fn new(resources: &Arc<DeviceResources>, color_format: vk::Format) -> Self {
        Self {
            resources: Arc::clone(resources),
            color_format,
            thumbnail_pipeline: None,
            preview_pipeline: None,
            preview_sphere: None,
        }
    }

    /// The highest MSAA count (≤8) valid for the thumbnail targets: the device's supported
    /// counts intersected with the color format's own counts. Thumbnails are tiny and
    /// rendered once, so always taking the maximum is cheap and hides geometry aliasing.
    fn sample_count(&self, device: &Device) -> vk::SampleCountFlags {
        let mut supported = device.supported_sample_counts(self.color_format, crate::DEPTH_FORMAT);
        // SAFETY: the ash seam. The physical-device handle + format query are read-only.
        let color_props = unsafe {
            device
                .instance()
                .get_physical_device_image_format_properties(
                    device.physical_device(),
                    self.color_format,
                    vk::ImageType::TYPE_2D,
                    vk::ImageTiling::OPTIMAL,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    vk::ImageCreateFlags::empty(),
                )
        };
        if let Ok(props) = color_props {
            supported &= props.sample_counts;
        }
        for candidate in [
            vk::SampleCountFlags::TYPE_8,
            vk::SampleCountFlags::TYPE_4,
            vk::SampleCountFlags::TYPE_2,
        ] {
            if supported.contains(candidate) {
                return candidate;
            }
        }
        vk::SampleCountFlags::TYPE_1
    }

    /// Builds (and caches) the lazy thumbnail/preview PSOs + the preview sphere up front,
    /// so a later render never initializes them on a contended path.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build or mesh-upload failure.
    pub fn prewarm(&mut self, device: &Device, descriptors: &Descriptors) -> Result<()> {
        self.ensure_thumbnail_pipeline(device)?;
        self.ensure_preview_pipeline(device, descriptors)?;
        self.ensure_preview_sphere(device, descriptors)?;
        Ok(())
    }

    /// Whether the lazy thumbnail PSO has been built (test/inspection).
    pub fn thumbnail_pipeline_built(&self) -> bool {
        self.thumbnail_pipeline.is_some()
    }

    /// Whether the lazy preview PSO has been built (test/inspection).
    pub fn preview_pipeline_built(&self) -> bool {
        self.preview_pipeline.is_some()
    }

    /// Whether the preview sphere has been built (test/inspection).
    pub fn preview_sphere_built(&self) -> bool {
        self.preview_sphere.is_some()
    }

    fn ensure_thumbnail_pipeline(&mut self, device: &Device) -> Result<Arc<Pipeline>> {
        if let Some(pipeline) = &self.thumbnail_pipeline {
            return Ok(Arc::clone(pipeline));
        }
        let samples = self.sample_count(device);
        let pipeline = Arc::new(self.build_thumbnail_pipeline(device, samples)?);
        self.thumbnail_pipeline = Some(Arc::clone(&pipeline));
        Ok(pipeline)
    }

    fn ensure_preview_pipeline(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
    ) -> Result<Arc<Pipeline>> {
        if let Some(pipeline) = &self.preview_pipeline {
            return Ok(Arc::clone(pipeline));
        }
        let samples = self.sample_count(device);
        let pipeline = Arc::new(self.build_preview_pipeline(
            device,
            descriptors,
            "shaders/preview.spv",
            samples,
        )?);
        self.preview_pipeline = Some(Arc::clone(&pipeline));
        Ok(pipeline)
    }

    fn ensure_preview_sphere(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
    ) -> Result<Arc<GpuMesh>> {
        if let Some(sphere) = &self.preview_sphere {
            return Ok(Arc::clone(sphere));
        }
        let sphere = make_preview_sphere(device, descriptors)?;
        self.preview_sphere = Some(Arc::clone(&sphere));
        Ok(sphere)
    }

    /// Renders `mesh` framed by its AABB under a fixed directional light (flat neutral
    /// albedo) into a `size`×`size` texture.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_mesh_thumbnail(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        mesh: &Arc<GpuMesh>,
        size: u32,
    ) -> Result<Arc<GpuTexture>> {
        let pipeline = self.ensure_thumbnail_pipeline(device)?;
        let (center, radius) = mesh_bounds(mesh);
        let view_proj = framed_view_proj(center, radius, Vec3::new(1.0, 0.7, 1.0));
        let push = ThumbnailPush {
            mvp: view_proj,
            normal_matrix: Mat4::IDENTITY,
        };
        let mesh = Arc::clone(mesh);
        self.render_to_texture(
            device,
            descriptors,
            size,
            [0.12, 0.12, 0.14, 1.0],
            move |raw, cmd| {
                // SAFETY: the ash seam. The pipeline + mesh outlive the submit.
                unsafe {
                    raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.handle());
                    raw.cmd_push_constants(
                        cmd,
                        pipeline.layout(),
                        vk::ShaderStageFlags::VERTEX,
                        0,
                        bytemuck::bytes_of(&push),
                    );
                    draw_submeshes(raw, cmd, &mesh);
                }
            },
        )
    }

    /// Renders a unit sphere with `material` under studio lighting into a `size`×`size`
    /// texture. `shader_spv` of `None` uses the cached default preview pipeline; a codegen
    /// material passes its compiled `.spv` path and gets a fresh per-call pipeline.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_material_preview(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        material: &SubmeshMaterial,
        size: u32,
        shader_spv: Option<&std::path::Path>,
    ) -> Result<Arc<GpuTexture>> {
        let samples = self.sample_count(device);
        let pipeline = match shader_spv {
            None => self.ensure_preview_pipeline(device, descriptors)?,
            Some(path) => {
                let spv = path.to_string_lossy().into_owned();
                Arc::new(self.build_preview_pipeline(device, descriptors, &spv, samples)?)
            }
        };
        let sphere = self.ensure_preview_sphere(device, descriptors)?;
        let view_proj = framed_view_proj(Vec3::ZERO, 1.0, Vec3::new(0.3, 0.4, 1.0));
        let push = preview_push(material, view_proj);
        let bindless_set = descriptors.bindless_set();
        let layout = pipeline.layout();
        let pipeline_handle = pipeline.handle();
        let texture = self.render_to_texture(
            device,
            descriptors,
            size,
            [0.10, 0.10, 0.12, 1.0],
            |raw, cmd| {
                // SAFETY: the ash seam. The pipeline + bindless set + sphere outlive the submit:
                // `pipeline` is held in this outer scope (dropped only after the submit waited),
                // so the per-call codegen pipeline is not destroyed mid-recording.
                unsafe {
                    raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline_handle);
                    raw.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        layout,
                        0,
                        &[bindless_set],
                        &[],
                    );
                    raw.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        bytemuck::bytes_of(&push),
                    );
                    draw_submeshes(raw, cmd, &sphere);
                }
            },
        );
        // Keep the pipeline alive until here — past the submit-and-wait inside
        // `render_to_texture` — so a per-call codegen pipeline (held only by this `Arc`) is
        // destroyed only after the GPU is done with it, not when the draw closure is consumed.
        drop(pipeline);
        texture
    }

    /// Renders `mesh` shaded per-submesh with its own material from `submesh_materials`
    /// (indexed by `Submesh::material_slot`, clamped) under the studio preview lighting,
    /// framed by the mesh bounds — the textured asset tile.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_model_thumbnail(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        mesh: &Arc<GpuMesh>,
        submesh_materials: &[SubmeshMaterial],
        size: u32,
    ) -> Result<Arc<GpuTexture>> {
        let pipeline = self.ensure_preview_pipeline(device, descriptors)?;
        let (center, radius) = mesh_bounds(mesh);
        let view_proj = framed_view_proj(center, radius, Vec3::new(0.3, 0.4, 1.0));

        let fallback = SubmeshMaterial::defaults();
        let pushes: Vec<(Submesh, PreviewPush)> = mesh
            .submeshes
            .iter()
            .map(|submesh| {
                let material = if submesh_materials.is_empty() {
                    &fallback
                } else {
                    let idx = (submesh.material_slot as usize).min(submesh_materials.len() - 1);
                    &submesh_materials[idx]
                };
                (*submesh, preview_push(material, view_proj))
            })
            .collect();
        let bindless_set = descriptors.bindless_set();
        let layout = pipeline.layout();
        let mesh = Arc::clone(mesh);
        self.render_to_texture(
            device,
            descriptors,
            size,
            [0.10, 0.10, 0.12, 1.0],
            move |raw, cmd| {
                // SAFETY: the ash seam. The pipeline + bindless set + mesh outlive the submit.
                unsafe {
                    raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.handle());
                    raw.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        layout,
                        0,
                        &[bindless_set],
                        &[],
                    );
                    raw.cmd_bind_vertex_buffers(cmd, 0, &[mesh.vertex_buffer()], &[0]);
                    raw.cmd_bind_index_buffer(cmd, mesh.index_buffer(), 0, vk::IndexType::UINT32);
                    for (submesh, push) in &pushes {
                        raw.cmd_push_constants(
                            cmd,
                            layout,
                            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                            0,
                            bytemuck::bytes_of(push),
                        );
                        raw.cmd_draw_indexed(
                            cmd,
                            submesh.index_count,
                            1,
                            submesh.first_index,
                            submesh.vertex_offset,
                            0,
                        );
                    }
                }
            },
        )
    }

    /// Renders the framed mesh to a `size`×`size` texture, then reads it back to a PNG.
    ///
    /// # Errors
    ///
    /// Propagates the render or read-back/encode failure.
    pub fn encode_asset_thumbnail_png(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        mesh: &Arc<GpuMesh>,
        size: u32,
    ) -> Result<ThumbnailPng> {
        let texture = self.render_mesh_thumbnail(device, descriptors, mesh, size)?;
        self.encode_texture_thumbnail_png(device, &texture, size, PngTransfer::Clamp)
    }

    /// Renders the framed, textured model to a `size`×`size` texture, then reads it back
    /// to a PNG.
    ///
    /// # Errors
    ///
    /// Propagates the render or read-back/encode failure.
    pub fn encode_model_thumbnail_png(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        mesh: &Arc<GpuMesh>,
        submesh_materials: &[SubmeshMaterial],
        size: u32,
    ) -> Result<ThumbnailPng> {
        let texture =
            self.render_model_thumbnail(device, descriptors, mesh, submesh_materials, size)?;
        self.encode_texture_thumbnail_png(device, &texture, size, PngTransfer::Clamp)
    }

    /// Allocates the offscreen targets, records `draw` inside a clear→render→read
    /// transition, submits the one-off, and hands back the resolved image as a sampled
    /// [`GpuTexture`] in `SHADER_READ_ONLY_OPTIMAL`.
    fn render_to_texture<F>(
        &self,
        device: &Device,
        _descriptors: &Descriptors,
        size: u32,
        clear: [f32; 4],
        draw: F,
    ) -> Result<Arc<GpuTexture>>
    where
        F: FnOnce(&ash::Device, vk::CommandBuffer),
    {
        let samples = self.sample_count(device);
        let mut targets = self.allocate_targets(device, size, samples)?;

        let raw = device.raw();
        // SAFETY: the ash seam. A transient one-off pool freed at the end of this call.
        let pool = checked(
            unsafe {
                raw.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                        .queue_family_index(device.graphics_queue_family),
                    None,
                )
            },
            "thumbnail: create_command_pool",
        )?;
        let recorded = record_and_submit(device, pool, &targets, clear, draw);
        // SAFETY: the ash seam. The submit (if any) was waited; the pool is freed once.
        unsafe { raw.destroy_command_pool(pool, None) };
        recorded?;

        // Take ownership of the read target's handles as a sampled GpuTexture (no material
        // set; the editor reads thumbnails over the control plane). The multisampled color
        // image (and the depth image) free normally on scope exit.
        let result = targets.resolve.as_mut().unwrap_or(&mut targets.color);
        let texture = GpuTexture::from_parts(
            &self.resources,
            GpuTextureParts {
                image: result.image,
                view: result.view,
                allocation: result
                    .allocation
                    .take()
                    .expect("result image owns its allocation"),
                bindless_index: u32::MAX,
                extent: result.extent,
                format: result.format,
            },
            // A thumbnail texture is never bindless; pair it with its own free-list so its
            // drop has somewhere to push the sentinel slot without disturbing the renderer's.
            &thumbnail_free_list(),
        );
        // The handles are owned by the GpuTexture now; null them so the ManagedImage drop
        // is a no-op for the moved-out image (the allocation is already taken).
        result.image = vk::Image::null();
        result.view = vk::ImageView::null();
        Ok(Arc::new(texture))
    }

    /// Allocates the color (+ optional resolve) + depth targets for a `size`×`size` render.
    fn allocate_targets(
        &self,
        device: &Device,
        size: u32,
        samples: vk::SampleCountFlags,
    ) -> Result<ThumbnailTargets> {
        let msaa = samples != vk::SampleCountFlags::TYPE_1;
        let color = self.new_color_image(device, size, samples)?;
        let resolve = if msaa {
            Some(self.new_color_image(device, size, vk::SampleCountFlags::TYPE_1)?)
        } else {
            None
        };
        let depth = self.new_depth_image(device, size, samples)?;
        Ok(ThumbnailTargets {
            color,
            resolve,
            depth,
            samples,
        })
    }

    fn new_color_image(
        &self,
        device: &Device,
        size: u32,
        samples: vk::SampleCountFlags,
    ) -> Result<ManagedImage> {
        self.new_managed_image(
            device,
            size,
            self.color_format,
            samples,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::ImageAspectFlags::COLOR,
        )
    }

    fn new_depth_image(
        &self,
        device: &Device,
        size: u32,
        samples: vk::SampleCountFlags,
    ) -> Result<ManagedImage> {
        self.new_managed_image(
            device,
            size,
            crate::DEPTH_FORMAT,
            samples,
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            vk::ImageAspectFlags::DEPTH,
        )
    }

    /// Creates a dedicated-memory VMA image + a full-subresource view.
    fn new_managed_image(
        &self,
        device: &Device,
        size: u32,
        format: vk::Format,
        samples: vk::SampleCountFlags,
        usage: vk::ImageUsageFlags,
        aspect: vk::ImageAspectFlags,
    ) -> Result<ManagedImage> {
        let allocator = device.allocator();
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: size,
                height: size,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned and freed
        // by the returned `ManagedImage` (or moved into a `GpuTexture`).
        let (image, allocation) = checked(
            unsafe { allocator.create_image(&image_info, &alloc_info) },
            "thumbnail: vmaCreateImage",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspect,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the image just created; freed on the
        // error path before the early return.
        let view = match unsafe { device.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe { allocator.destroy_image(image, &mut allocation) };
                return Err(Error::Vk {
                    context: "thumbnail: create_image_view",
                    result,
                });
            }
        };
        Ok(ManagedImage {
            resources: Arc::clone(device.resources()),
            image,
            view,
            allocation: Some(allocation),
            extent: vk::Extent2D {
                width: size,
                height: size,
            },
            format,
        })
    }

    /// Builds the minimal mesh-thumbnail PSO from `thumbnail.spv` (vertex input + a
    /// 2×mat4 vertex push, no descriptor sets).
    fn build_thumbnail_pipeline(
        &self,
        device: &Device,
        samples: vk::SampleCountFlags,
    ) -> Result<Pipeline> {
        let module = load_thumbnail_shader(device, "shaders/thumbnail.spv")?;
        let raw = device.raw();
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(size_of::<ThumbnailPush>() as u32)];
        let layout_info = vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push);
        // SAFETY: the ash seam. The push range outlives the call; the layout is owned by
        // the returned `Pipeline` (freed on the build-error path).
        let layout = match checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "thumbnail: create_pipeline_layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The module was created above; freed once here.
                unsafe { raw.destroy_shader_module(module, None) };
                return Err(err);
            }
        };
        let result = self.build_graphics_pipeline(device, module, layout, samples, false);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freed after.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    /// Builds the studio material-preview PSO from `spv_path` (binds the bindless set 0,
    /// a 112-byte vertex+fragment `PreviewPush`).
    fn build_preview_pipeline(
        &self,
        device: &Device,
        descriptors: &Descriptors,
        spv_path: &str,
        samples: vk::SampleCountFlags,
    ) -> Result<Pipeline> {
        let module = load_thumbnail_shader(device, spv_path)?;
        let raw = device.raw();
        let set_layouts = [descriptors.bindless_set_layout()];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(size_of::<PreviewPush>() as u32)];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: the ash seam. The set layout + push range outlive the call.
        let layout = match checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "preview: create_pipeline_layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The module was created above; freed once here.
                unsafe { raw.destroy_shader_module(module, None) };
                return Err(err);
            }
        };
        let result = self.build_graphics_pipeline(device, module, layout, samples, true);
        // SAFETY: the ash seam. The module is consumed by pipeline creation; freed after.
        unsafe { raw.destroy_shader_module(module, None) };
        result
    }

    /// The shared graphics-pipeline body for the thumbnail + preview PSOs: a triangle
    /// list of the three-attribute [`Vertex`] stream, fill rasterizer (no cull),
    /// depth-test+write (LESS), the color + depth formats, dynamic viewport/scissor.
    /// `layout` is already created (and freed here on a build error).
    fn build_graphics_pipeline(
        &self,
        device: &Device,
        module: vk::ShaderModule,
        layout: vk::PipelineLayout,
        samples: vk::SampleCountFlags,
        is_preview: bool,
    ) -> Result<Pipeline> {
        let raw = device.raw();
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
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(std::mem::offset_of!(Vertex, position) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(std::mem::offset_of!(Vertex, normal) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(std::mem::offset_of!(Vertex, uv0) as u32),
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
        let multisample =
            vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(samples);
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

        let color_formats = [self.color_format];
        let mut rendering_info = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(crate::DEPTH_FORMAT);

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
        match created {
            Ok(pipelines) => Ok(Pipeline::from_parts(&self.resources, pipelines[0], layout)),
            Err((_, result)) => {
                // SAFETY: the ash seam. The layout was created by the caller; freed once.
                unsafe { raw.destroy_pipeline_layout(layout, None) };
                Err(Error::Vk {
                    context: if is_preview {
                        "create_graphics_pipelines (preview)"
                    } else {
                        "create_graphics_pipelines (thumbnail)"
                    },
                    result,
                })
            }
        }
    }

    /// Renders `texture` (downscaled to fit `size`×`size`) and reads the result back as a
    /// PNG. A texture larger than `size` is reduced by a chained 2× linear-blit pyramid
    /// (the undersampling fix); a texture at or below `size`, or a format without
    /// linear-blit support, reads back at native extent.
    ///
    /// # Errors
    ///
    /// Propagates any allocation / blit / read-back / encode failure.
    pub fn encode_texture_thumbnail_png(
        &self,
        device: &Device,
        texture: &Arc<GpuTexture>,
        size: u32,
        transfer: PngTransfer,
    ) -> Result<ThumbnailPng> {
        let src_w = texture.extent.width;
        let src_h = texture.extent.height;
        let max_dim = src_w.max(src_h);
        let format = texture.format;

        let downscale = max_dim > size && size > 0 && format_supports_linear_blit(device, format);
        let (dst_w, dst_h) = if downscale {
            (
                ((src_w * size + max_dim / 2) / max_dim).max(1),
                ((src_h * size + max_dim / 2) / max_dim).max(1),
            )
        } else {
            (src_w, src_h)
        };

        let mut steps: Vec<(vk::Extent2D, ManagedImage)> = Vec::new();
        if downscale {
            let mut extents = Vec::new();
            let mut cur = texture.extent;
            while cur.width > dst_w * 2 || cur.height > dst_h * 2 {
                cur.width = dst_w.max(cur.width / 2);
                cur.height = dst_h.max(cur.height / 2);
                extents.push(cur);
            }
            if extents
                .last()
                .is_none_or(|e| e.width != dst_w || e.height != dst_h)
            {
                extents.push(vk::Extent2D {
                    width: dst_w,
                    height: dst_h,
                });
            }
            for extent in extents {
                steps.push((extent, self.new_blit_image(device, extent, format)?));
            }
        }

        let bytes = (dst_w as vk::DeviceSize)
            * (dst_h as vk::DeviceSize)
            * format_pixel_bytes(format) as vk::DeviceSize;
        let readback = crate::Buffer::new(
            device.resources(),
            bytes,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        let raw = device.raw();
        // SAFETY: the ash seam. A transient one-off pool freed at the end of this call.
        let pool = checked(
            unsafe {
                raw.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                        .queue_family_index(device.graphics_queue_family),
                    None,
                )
            },
            "encode_texture_thumbnail: create_command_pool",
        )?;
        let recorded = record_downscale_and_read(
            device,
            pool,
            texture,
            &steps,
            vk::Extent2D {
                width: dst_w,
                height: dst_h,
            },
            downscale,
            readback.handle(),
        );
        // SAFETY: the ash seam. The submit (if any) was waited; the pool is freed once.
        unsafe { raw.destroy_command_pool(pool, None) };
        recorded?;

        // SAFETY: the buffer is HOST_VISIBLE + MAPPED for `bytes`; the copy completed.
        let pixels = unsafe { std::slice::from_raw_parts(readback.mapped_ptr(), bytes as usize) };
        let png = crate::thumbnail::encode_to_png(pixels, dst_w, dst_h, format, transfer)
            .map_err(|err| Error::ShaderLoad(format!("thumbnail PNG encode: {err}")))?;
        Ok(ThumbnailPng {
            bytes: png,
            width: dst_w,
            height: dst_h,
        })
    }

    /// A transient 1× image for the thumbnail downscale chain (`TRANSFER_DST |
    /// TRANSFER_SRC` only). The view is null (blit/copy take images).
    fn new_blit_image(
        &self,
        device: &Device,
        extent: vk::Extent2D,
        format: vk::Format,
    ) -> Result<ManagedImage> {
        let allocator = device.allocator();
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned + freed by
        // the returned `ManagedImage`.
        let (image, allocation) = checked(
            unsafe { allocator.create_image(&image_info, &alloc_info) },
            "encode_texture_thumbnail: vmaCreateImage (blit target)",
        )?;
        Ok(ManagedImage {
            resources: Arc::clone(device.resources()),
            image,
            view: vk::ImageView::null(),
            allocation: Some(allocation),
            extent,
            format,
        })
    }
}

/// The bindless free-list a thumbnail [`GpuTexture`] is paired with so its `Drop` has a
/// list to push its (sentinel) slot back to. A thumbnail texture is never bindless, so
/// this is a throwaway list dropped with the texture — it never touches the renderer's.
fn thumbnail_free_list() -> crate::BindlessFreeList {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

/// Binds `mesh`'s vertex + index buffers and draws every submesh.
///
/// # Safety
///
/// `cmd` recording; `mesh` outlives the submit.
unsafe fn draw_submeshes(raw: &ash::Device, cmd: vk::CommandBuffer, mesh: &GpuMesh) {
    // SAFETY: forwarded from the caller's recording contract.
    unsafe {
        raw.cmd_bind_vertex_buffers(cmd, 0, &[mesh.vertex_buffer()], &[0]);
        raw.cmd_bind_index_buffer(cmd, mesh.index_buffer(), 0, vk::IndexType::UINT32);
        for submesh in &mesh.submeshes {
            raw.cmd_draw_indexed(
                cmd,
                submesh.index_count,
                1,
                submesh.first_index,
                submesh.vertex_offset,
                0,
            );
        }
    }
}

/// Records the clear→render→read sequence into a one-off buffer from `pool`, submits on
/// the graphics queue, and waits the fence.
fn record_and_submit<F>(
    device: &Device,
    pool: vk::CommandPool,
    targets: &ThumbnailTargets,
    clear: [f32; 4],
    draw: F,
) -> Result<()>
where
    F: FnOnce(&ash::Device, vk::CommandBuffer),
{
    let raw = device.raw();
    let msaa = targets.samples != vk::SampleCountFlags::TYPE_1;
    let size = targets.color.extent;

    // SAFETY: the ash seam. One primary buffer from the transient pool.
    let cmd = checked(
        unsafe {
            raw.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        },
        "thumbnail: allocate_command_buffers",
    )?[0];
    // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "thumbnail: create_fence",
    )?;

    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let result = (|| -> Result<()> {
        // SAFETY: the ash seam. Begin/record/end; the targets outlive the submit.
        unsafe {
            checked(
                raw.begin_command_buffer(cmd, &begin),
                "thumbnail: begin_command_buffer",
            )?;

            color_to_attachment(raw, cmd, targets.color.image);
            if let Some(resolve) = &targets.resolve {
                color_to_attachment(raw, cmd, resolve.image);
            }
            depth_to_attachment(raw, cmd, targets.depth.image);

            let mut color_attach = vk::RenderingAttachmentInfo::default()
                .image_view(targets.color.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(if msaa {
                    vk::AttachmentStoreOp::DONT_CARE
                } else {
                    vk::AttachmentStoreOp::STORE
                })
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: clear },
                });
            if let Some(resolve) = &targets.resolve {
                color_attach = color_attach
                    .resolve_mode(vk::ResolveModeFlags::AVERAGE)
                    .resolve_image_view(resolve.view)
                    .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            }
            let depth_attach = vk::RenderingAttachmentInfo::default()
                .image_view(targets.depth.view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: 1.0,
                        stencil: 0,
                    },
                });
            let color_attachments = [color_attach];
            let rendering = vk::RenderingInfo::default()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: size,
                })
                .layer_count(1)
                .color_attachments(&color_attachments)
                .depth_attachment(&depth_attach);
            raw.cmd_begin_rendering(cmd, &rendering);

            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: size.width as f32,
                height: size.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            raw.cmd_set_viewport(cmd, 0, &[viewport]);
            raw.cmd_set_scissor(
                cmd,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: size,
                }],
            );
            draw(raw, cmd);
            raw.cmd_end_rendering(cmd);

            let result_image = targets
                .resolve
                .as_ref()
                .map_or(targets.color.image, |r| r.image);
            color_to_shader_read(raw, cmd, result_image);
            checked(raw.end_command_buffer(cmd), "thumbnail: end_command_buffer")?;
        }

        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The control-drain thread owns the queue here; the fence
        // belongs to this device.
        unsafe {
            checked(
                raw.queue_submit2(device.graphics_queue, &submits, fence),
                "thumbnail: queue_submit2",
            )?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "thumbnail: wait_for_fences",
            )?;
        }
        Ok(())
    })();

    // SAFETY: the ash seam. The fence was waited (or the submit failed before signaling),
    // so it is idle and destroyed exactly once.
    unsafe { raw.destroy_fence(fence, None) };
    result
}

/// Records the downscale-blit chain (if any) + the read-back copy, submits, and waits.
fn record_downscale_and_read(
    device: &Device,
    pool: vk::CommandPool,
    texture: &Arc<GpuTexture>,
    steps: &[(vk::Extent2D, ManagedImage)],
    dst: vk::Extent2D,
    downscale: bool,
    readback: vk::Buffer,
) -> Result<()> {
    let raw = device.raw();
    // SAFETY: the ash seam. One primary buffer from the transient pool.
    let cmd = checked(
        unsafe {
            raw.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        },
        "encode_texture_thumbnail: allocate_command_buffers",
    )?[0];
    // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "encode_texture_thumbnail: create_fence",
    )?;

    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let result = (|| -> Result<()> {
        // SAFETY: the ash seam. Begin/record/end; the texture + transients outlive the
        // submit; the readback buffer is the copy destination.
        unsafe {
            checked(
                raw.begin_command_buffer(cmd, &begin),
                "encode_texture_thumbnail: begin_command_buffer",
            )?;
            if downscale {
                record_blit_chain(raw, cmd, texture.handle(), texture.extent, steps);
                let src = steps.last().map_or(texture.handle(), |(_, img)| img.image);
                capture_image_to_buffer(
                    raw,
                    cmd,
                    src,
                    dst,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::BLIT,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::BLIT,
                    vk::AccessFlags2::TRANSFER_READ,
                    readback,
                );
            } else {
                capture_image_to_buffer(
                    raw,
                    cmd,
                    texture.handle(),
                    texture.extent,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                    readback,
                );
            }
            checked(
                raw.end_command_buffer(cmd),
                "encode_texture_thumbnail: end_command_buffer",
            )?;
        }
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The control-drain thread owns the queue here.
        unsafe {
            checked(
                raw.queue_submit2(device.graphics_queue, &submits, fence),
                "encode_texture_thumbnail: queue_submit2",
            )?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "encode_texture_thumbnail: wait_for_fences",
            )?;
        }
        Ok(())
    })();
    // SAFETY: the ash seam. The fence was waited; destroyed exactly once.
    unsafe { raw.destroy_fence(fence, None) };
    result
}

/// Whether `format` supports linear-filtered blits (src + dst) in optimal tiling.
fn format_supports_linear_blit(device: &Device, format: vk::Format) -> bool {
    // SAFETY: the ash seam. The format-property query is read-only.
    let props = unsafe {
        device
            .instance()
            .get_physical_device_format_properties(device.physical_device(), format)
    };
    let needed = vk::FormatFeatureFlags::BLIT_SRC
        | vk::FormatFeatureFlags::BLIT_DST
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
    props.optimal_tiling_features.contains(needed)
}

/// Builds + uploads a unit UV sphere (origin-centered, radius 1; normals == positions)
/// for material previews.
fn make_preview_sphere(device: &Device, descriptors: &Descriptors) -> Result<Arc<GpuMesh>> {
    const RINGS: u32 = 32;
    const SECTORS: u32 = 48;
    let mut mesh = Mesh::default();
    for r in 0..=RINGS {
        let phi = std::f32::consts::PI * (r as f32) / (RINGS as f32);
        for s in 0..=SECTORS {
            let theta = 2.0 * std::f32::consts::PI * (s as f32) / (SECTORS as f32);
            let position = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.vertices.push(Vertex {
                position,
                normal: position,
                uv0: Vec2::new((s as f32) / (SECTORS as f32), (r as f32) / (RINGS as f32)),
            });
        }
    }
    for r in 0..RINGS {
        for s in 0..SECTORS {
            let a = r * (SECTORS + 1) + s;
            let b = a + SECTORS + 1;
            mesh.indices
                .extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    mesh.submeshes.push(Submesh {
        first_index: 0,
        index_count: mesh.indices.len() as u32,
        vertex_offset: 0,
        material_slot: 0,
    });
    let uploader = Uploader::new(device, &GpuQueue::new(device.graphics_queue))?;
    uploader.upload_mesh(descriptors, &mesh, &[], None, None)
}

/// Loads a thumbnail/preview SPIR-V module, resolving `shaders/<x>.spv` against the
/// runtime shader dir (or taking an absolute codegen path as-is).
fn load_thumbnail_shader(device: &Device, shader: &str) -> Result<vk::ShaderModule> {
    let path = if std::path::Path::new(shader).is_absolute() {
        std::path::PathBuf::from(shader)
    } else {
        crate::pipelines::resolve_shader_dir()
            .join(shader.strip_prefix("shaders/").unwrap_or(shader))
    };
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
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: the ash seam. The code slice outlives the call; the module is freed by the
    // caller after pipeline creation.
    checked(
        unsafe { device.raw().create_shader_module(&info, None) },
        "thumbnail: create_shader_module",
    )
}

/// The `PreviewPush` for one material: the resolved bindless indices (or the default
/// white slot when a texture is absent) + the PBR factors.
fn preview_push(material: &SubmeshMaterial, view_proj: Mat4) -> PreviewPush {
    let idx = |tex: &Option<Arc<GpuTexture>>| -> u32 {
        tex.as_ref()
            .map_or(DEFAULT_WHITE_SLOT, |t| t.bindless_index())
    };
    let mut features = 0;
    if material.normal_texture.is_some() {
        features |= FEATURE_NORMAL;
    }
    PreviewPush {
        view_proj,
        base_color: material.base_color,
        tex: UVec4::new(
            idx(&material.albedo_texture),
            idx(&material.metallic_roughness_texture),
            idx(&material.normal_texture),
            features,
        ),
        pbr: Vec4::new(
            material.metallic,
            material.roughness,
            material.normal_strength,
            0.0,
        ),
    }
}

/// The mesh-bounds center + radius the framing uses (radius floored so a degenerate AABB
/// still frames).
fn mesh_bounds(mesh: &GpuMesh) -> (Vec3, f32) {
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    let mut radius = (mesh.bounds_max - mesh.bounds_min).length() * 0.5;
    if radius <= 0.0001 {
        radius = 1.0;
    }
    (center, radius)
}

/// A 3/4-view `proj * view` framing a sphere of `radius` at `center`, viewed from `dir`
/// (normalized) at a distance that fits the bounds. The Vulkan-clip Y flip matches the
/// viewport so the thumbnail is upright.
fn framed_view_proj(center: Vec3, radius: f32, dir: Vec3) -> Mat4 {
    let fovy = 45.0_f32.to_radians();
    let distance = radius / (fovy * 0.5).tan() * 1.3;
    let eye = center + dir.normalize() * distance;
    let view = Mat4::look_at_rh(eye, center, Vec3::Y);
    let mut proj = Mat4::perspective_rh(
        fovy,
        1.0,
        0.01_f32.max(distance - radius * 2.0),
        distance + radius * 2.0,
    );
    // glam's `perspective_rh` targets Vulkan [0,1] depth; flip the projected Y so the
    // framing is upright in the Vulkan-clip viewport.
    proj.y_axis.y *= -1.0;
    proj * view
}

impl Drop for ManagedImage {
    fn drop(&mut self) {
        // A no-op when ownership was handed to a GpuTexture (allocation taken). Otherwise
        // free the view (if any) then the image through the shared bundle.
        let Some(mut allocation) = self.allocation.take() else {
            return;
        };
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; the view
        // (when non-null) then the image are each freed exactly once.
        unsafe {
            if self.view != vk::ImageView::null() {
                self.resources.device().destroy_image_view(self.view, None);
            }
            self.resources
                .allocator()
                .destroy_image(self.image, &mut allocation);
        }
    }
}

/// Transitions a color image `UNDEFINED → COLOR_ATTACHMENT_OPTIMAL`.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
unsafe fn color_to_attachment(raw: &ash::Device, cmd: vk::CommandBuffer, image: vk::Image) {
    // SAFETY: forwarded from the caller's recording contract.
    unsafe {
        transition(
            raw,
            cmd,
            image,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::TOP_OF_PIPE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );
    }
}

/// Transitions a depth image `UNDEFINED → DEPTH_ATTACHMENT_OPTIMAL`.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
unsafe fn depth_to_attachment(raw: &ash::Device, cmd: vk::CommandBuffer, image: vk::Image) {
    // SAFETY: forwarded from the caller's recording contract.
    unsafe {
        transition(
            raw,
            cmd,
            image,
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::TOP_OF_PIPE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
        );
    }
}

/// Transitions the rendered color result `COLOR_ATTACHMENT_OPTIMAL → SHADER_READ_ONLY`.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
unsafe fn color_to_shader_read(raw: &ash::Device, cmd: vk::CommandBuffer, image: vk::Image) {
    // SAFETY: forwarded from the caller's recording contract.
    unsafe {
        transition(
            raw,
            cmd,
            image,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        );
    }
}

/// Records the chained 2× linear-blit downscale: the source is blitted into each
/// transient in turn (each flipped to `TRANSFER_SRC` to feed the next), restoring the
/// source's `SHADER_READ_ONLY` layout afterward. On exit the last transient is
/// `TRANSFER_SRC_OPTIMAL`.
///
/// # Safety
///
/// `cmd` recording; `source` + every transient image outlive the submit.
unsafe fn record_blit_chain(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    source: vk::Image,
    source_extent: vk::Extent2D,
    steps: &[(vk::Extent2D, ManagedImage)],
) {
    // SAFETY: forwarded from the caller's contract for every transition/blit below.
    unsafe {
        transition(
            raw,
            cmd,
            source,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            vk::PipelineStageFlags2::BLIT,
            vk::AccessFlags2::TRANSFER_READ,
        );
        let mut src_image = source;
        let mut src_extent = source_extent;
        for (extent, dst) in steps {
            transition(
                raw,
                cmd,
                dst.image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            let blit = vk::ImageBlit::default()
                .src_subresource(color_layers())
                .src_offsets([
                    vk::Offset3D { x: 0, y: 0, z: 0 },
                    vk::Offset3D {
                        x: src_extent.width as i32,
                        y: src_extent.height as i32,
                        z: 1,
                    },
                ])
                .dst_subresource(color_layers())
                .dst_offsets([
                    vk::Offset3D { x: 0, y: 0, z: 0 },
                    vk::Offset3D {
                        x: extent.width as i32,
                        y: extent.height as i32,
                        z: 1,
                    },
                ]);
            raw.cmd_blit_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
            transition(
                raw,
                cmd,
                dst.image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            );
            src_image = dst.image;
            src_extent = *extent;
        }
        // Restore the source's shader-read layout so the bindless array stays valid.
        transition(
            raw,
            cmd,
            source,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::PipelineStageFlags2::BLIT,
            vk::AccessFlags2::TRANSFER_READ,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        );
    }
}

/// Copies `image` (in `src_layout`) into `buffer`, transitioning in/out around the copy.
///
/// # Safety
///
/// `cmd` recording; `image` + `buffer` outlive the submit.
#[allow(clippy::too_many_arguments)]
unsafe fn capture_image_to_buffer(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    extent: vk::Extent2D,
    src_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_layout: vk::ImageLayout,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
    buffer: vk::Buffer,
) {
    // SAFETY: forwarded from the caller's contract.
    unsafe {
        transition(
            raw,
            cmd,
            image,
            vk::ImageAspectFlags::COLOR,
            src_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            src_stage,
            src_access,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            });
        raw.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer,
            &[region],
        );
        transition(
            raw,
            cmd,
            image,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst_layout,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
            dst_stage,
            dst_access,
        );
    }
}

/// The mip-0 single-layer color subresource the blits address.
fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// One sync2 image-memory barrier on the full single-mip subresource of `aspect`.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
#[allow(clippy::too_many_arguments)]
unsafe fn transition(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: forwarded from the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::Descriptors;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{Mesh, Submesh, Vertex};
    use std::sync::Mutex;

    /// The render fixture: a bare headless device + descriptors + the thumbnail sub-state,
    /// plus the production default-white texture seeded into the bindless table. A material
    /// with no textures samples [`DEFAULT_WHITE_SLOT`], so it must hold a valid view or
    /// lavapipe faults sampling an unwritten descriptor.
    /// The fields' drop order tears the GPU resources down before the device.
    struct Fixture {
        thumb: ThumbnailRenderer,
        descriptors: Descriptors,
        _white: Arc<GpuTexture>,
        uploader: Uploader,
        device: Device,
    }

    /// Builds the fixture, or skips (no Vulkan ICD in this toolbox). A full `Renderer::new`
    /// crashes the headless swapchain WSI on lavapipe, so the thumbnail render is exercised
    /// against the device directly — but the default white goes through the same production
    /// [`Uploader::upload_default_white`] path the renderer uses, no fixture workaround.
    fn fixture_or_skip() -> Option<Fixture> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = GpuQueue::new(device.graphics_queue);
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        // The production default-white path: uploads the 1×1 white into slot 0 and seeds
        // every other bindless slot, exactly as the renderer does at init.
        let white = uploader
            .upload_default_white(&descriptors)
            .expect("default white texture");
        // Lavapipe supports RGBA8 unorm as a color attachment + linear blit, the format the
        // editor consumes for thumbnails (the swapchain format on a real surface).
        let thumb = ThumbnailRenderer::new(device.resources(), vk::Format::R8G8B8A8_UNORM);
        Some(Fixture {
            thumb,
            descriptors,
            _white: white,
            uploader,
            device,
        })
    }

    /// A unit cube — non-degenerate bounds so the thumbnail framing is well-defined.
    fn cube() -> Mesh {
        let corners = [
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let vertices: Vec<Vertex> = corners
            .iter()
            .map(|&position| Vertex {
                position,
                normal: position.normalize(),
                uv0: Vec2::new(0.5, 0.5),
            })
            .collect();
        let indices = vec![
            0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7,
            4, 1, 5, 6, 1, 6, 2,
        ];
        Mesh {
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: indices.len() as u32,
                vertex_offset: 0,
                material_slot: 0,
            }],
            vertices,
            indices,
        }
    }

    /// The fraction of RGB bytes that differ from the clear color by more than a small
    /// tolerance — a render that drew geometry has many; a bare clear has ~none.
    fn non_clear_fraction(png: &[u8], clear: [u8; 3]) -> f32 {
        let decoded = image::load_from_memory(png).expect("decode png").to_rgb8();
        let (w, h) = decoded.dimensions();
        let differing = decoded
            .pixels()
            .filter(|p| {
                (p.0[0] as i32 - clear[0] as i32).abs() > 6
                    || (p.0[1] as i32 - clear[1] as i32).abs() > 6
                    || (p.0[2] as i32 - clear[2] as i32).abs() > 6
            })
            .count() as f32;
        differing / (w * h) as f32
    }

    /// A mesh thumbnail renders the framed cube (not just the clear), encodes to a 64×64
    /// PNG, and the whole render+readback is validation-clean on lavapipe.
    #[test]
    fn mesh_thumbnail_renders_nontrivial_pixels() {
        let Some(mut fx) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let mesh = fx
            .uploader
            .upload_mesh(&fx.descriptors, &cube(), &[], None, None)
            .expect("upload cube");

        let png = fx
            .thumb
            .encode_asset_thumbnail_png(&fx.device, &fx.descriptors, &mesh, 64)
            .expect("mesh thumbnail");
        assert_eq!((png.width, png.height), (64, 64));
        assert!(!png.bytes.is_empty());

        // The mesh-thumbnail clear is (0.12, 0.12, 0.14) → ~(31, 31, 36).
        let fraction = non_clear_fraction(&png.bytes, [31, 31, 36]);
        assert!(
            fraction > 0.05,
            "the framed cube covers a meaningful fraction (saw {fraction})"
        );

        drop(mesh);
        fx.device.wait_idle().expect("idle before teardown");
        drop(fx);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the mesh thumbnail must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The studio material preview renders a lit sphere (not just the clear), encodes to a
    /// 64×64 PNG, and is validation-clean. Exercises the bindless bind + the 112-byte
    /// PreviewPush (the no-texture material samples the default-white slot 0).
    #[test]
    fn material_preview_renders_nontrivial_pixels() {
        let Some(mut fx) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let material = SubmeshMaterial::defaults();
        let tex = fx
            .thumb
            .render_material_preview(&fx.device, &fx.descriptors, &material, 64, None)
            .expect("material preview");
        let png = fx
            .thumb
            .encode_texture_thumbnail_png(&fx.device, &tex, 64, PngTransfer::Clamp)
            .expect("encode preview");
        assert_eq!((png.width, png.height), (64, 64));

        // The preview clear is (0.10, 0.10, 0.12) → ~(26, 26, 31).
        let fraction = non_clear_fraction(&png.bytes, [26, 26, 31]);
        assert!(
            fraction > 0.05,
            "the lit sphere covers a meaningful fraction (saw {fraction})"
        );

        drop(tex);
        fx.device.wait_idle().expect("idle before teardown");
        drop(fx);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the material preview must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Regression guard for the unwritten default-white descriptor (lavapipe SIGSEGV /
    /// real-hardware UB): an untextured material indexes [`DEFAULT_WHITE_SLOT`] (slot 0),
    /// which the production [`Uploader::upload_default_white`] path (run in the fixture, as
    /// the renderer runs it at init) seeds with a valid 1×1 white view. The preview must
    /// render without faulting and be validation-clean, and the white must pass through —
    /// the lit sphere reads bright, not the near-black an unwritten descriptor (zeroed
    /// sample) would leave. Before the fix, no view was ever written into slot 0 and the
    /// bindless sample faulted.
    #[test]
    fn untextured_material_samples_seeded_default_white_slot() {
        let Some(mut fx) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();

        // The default material names no albedo/ORM/normal texture, so every texture index
        // in its `PreviewPush` resolves to the default-white slot 0.
        let material = SubmeshMaterial::defaults();
        assert!(
            material.albedo_texture.is_none() && material.metallic_roughness_texture.is_none(),
            "the regression scenario needs an untextured material (samples slot 0)"
        );

        let tex = fx
            .thumb
            .render_material_preview(&fx.device, &fx.descriptors, &material, 64, None)
            .expect("untextured material preview must not fault on slot 0");
        let png = fx
            .thumb
            .encode_texture_thumbnail_png(&fx.device, &tex, 64, PngTransfer::Clamp)
            .expect("encode preview");

        // The white albedo passes through the lighting (white × factor = factor), so the
        // sphere reads bright. An unwritten slot 0 would sample zero → a near-black sphere,
        // so a meaningfully bright fraction proves the seeded white reached the shader.
        let decoded = image::load_from_memory(&png.bytes)
            .expect("decode png")
            .to_rgb8();
        let bright = decoded
            .pixels()
            .filter(|p| p.0[0] as u32 + p.0[1] as u32 + p.0[2] as u32 > 180)
            .count() as f32;
        let fraction = bright / (decoded.width() * decoded.height()) as f32;
        assert!(
            fraction > 0.05,
            "the white-lit sphere must read bright (the seeded default white passed \
             through); saw {fraction} — a near-black result means slot 0 sampled an \
             unwritten descriptor"
        );

        drop(tex);
        fx.device.wait_idle().expect("idle before teardown");
        drop(fx);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "sampling the seeded default-white slot must be validation-clean (saw {} new \
             issue(s))",
            after.saturating_sub(before)
        );
    }

    /// `prewarm` builds the lazy PSOs + the preview sphere, and a subsequent render reuses
    /// them (validation-clean).
    #[test]
    fn prewarm_builds_then_renders_reuse_the_caches() {
        let Some(mut fx) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        fx.thumb
            .prewarm(&fx.device, &fx.descriptors)
            .expect("prewarm");
        assert!(fx.thumb.thumbnail_pipeline_built());
        assert!(fx.thumb.preview_pipeline_built());
        assert!(fx.thumb.preview_sphere_built());

        let material = SubmeshMaterial::defaults();
        let tex = fx
            .thumb
            .render_material_preview(&fx.device, &fx.descriptors, &material, 32, None)
            .expect("preview after prewarm");
        drop(tex);
        fx.device.wait_idle().expect("idle before teardown");
        drop(fx);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "prewarm + reuse must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// A texture larger than the requested size downscales (via the linear-blit pyramid)
    /// to fit, preserving aspect; both are validation-clean.
    #[test]
    fn texture_thumbnail_downscales_to_fit() {
        let Some(fx) = fixture_or_skip() else {
            return;
        };
        let before = validation_issue_count();

        let (w, h) = (256u32, 128u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = (x * 255 / w) as u8;
                rgba[i + 1] = (y * 255 / h) as u8;
                rgba[i + 2] = 128;
                rgba[i + 3] = 255;
            }
        }
        let tex = fx
            .uploader
            .upload_texture(&fx.descriptors, &rgba, w, h, true)
            .expect("upload texture");

        let png = fx
            .thumb
            .encode_texture_thumbnail_png(&fx.device, &tex, 64, PngTransfer::Clamp)
            .expect("encode texture thumbnail");
        assert_eq!(png.width, 64, "max dimension fits the requested size");
        assert_eq!(png.height, 32, "aspect ratio is preserved");
        let decoded = image::load_from_memory(&png.bytes)
            .expect("decode")
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 32));

        drop(tex);
        fx.device.wait_idle().expect("idle before teardown");
        drop(fx);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the texture thumbnail must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
