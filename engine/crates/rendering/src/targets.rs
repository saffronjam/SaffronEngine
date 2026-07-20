//! The scene-global shadow render targets: the directional + spot depth maps and the
//! omnidirectional point-shadow distance cube. The scene has one light rig, so these
//! live once (not per editor pane).
//!
//! The directional and spot maps are plain depth [`Image`]s (sampled with the compare
//! sampler). The point cube ([`PointShadowCube`]) is a `CUBE_COMPATIBLE` 6-layer color
//! image (`R32_SFLOAT` distance) with a cube sampling view plus 6 per-face render views,
//! and a shared single-layer depth scratch — its 6 layers exceed the render graph's
//! single-layer barrier, so it manages its own layout in the point-shadow pass body.

use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use crate::lighting::{POINT_SHADOW_COLOR_FORMAT, POINT_SHADOW_SIZE, SHADOW_MAP_SIZE};
use crate::pipelines::DEPTH_FORMAT;
use crate::resources::{DeviceResources, Image, ImageDesc};
use crate::{Device, Result, checked};

/// The scene-global shadow maps.
///
/// Built once in [`Targets::new`]; the maps are sampled by every light set and written
/// by the shadow passes. Each [`Image`] / [`PointShadowCube`] is a Drop type holding the
/// shared allocator `Arc`, so they free without a live `&Device`.
pub struct Targets {
    /// Directional-light depth map (sampled with the compare sampler).
    pub directional_shadow: Image,
    /// First shadow-casting spot light's depth map (same compare sampler).
    pub spot_shadow: Image,
    /// First shadow-casting point light's omnidirectional distance cube for **static**
    /// (non-deformed) casters — re-rendered only when the static caster set or the light moves.
    pub point_shadow: PointShadowCube,
    /// The same light's distance cube for **dynamic** (deformed: skinned / morph) casters —
    /// re-rendered each active frame. The scene samples `min(static, dynamic)`, so a moving
    /// character re-renders only itself against the cached static environment cube.
    pub point_shadow_dynamic: PointShadowCube,
}

impl Targets {
    /// Creates the directional + spot depth maps (`SHADOW_MAP_SIZE`²) and the point
    /// distance cube (`POINT_SHADOW_SIZE`² per face). All are seeded `UNDEFINED`; the
    /// shadow passes transition them, and the light set binds them at
    /// `SHADER_READ_ONLY_OPTIMAL` (the graph guarantees the layout when the scene samples).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if any image/view cannot be created (already-created handles
    /// free via their Drop).
    pub fn new(device: &Device) -> Result<Self> {
        let resources = device.resources();
        let mut directional_shadow = shadow_depth_map(resources)?;
        let mut spot_shadow = shadow_depth_map(resources)?;
        let mut point_shadow = PointShadowCube::new(resources)?;
        let mut point_shadow_dynamic = PointShadowCube::new(resources)?;

        // Transition all maps once to ShaderReadOnly so their descriptors are valid on frames
        // where no shadow pass runs (the shader gates the sample), and so each point cube's first
        // per-frame barrier (ShaderReadOnly → ColorAttachment) has a matching old layout. A
        // one-time init transition.
        initialize_shadow_layouts(device, &directional_shadow, &spot_shadow, &point_shadow)?;
        initialize_shadow_layouts(
            device,
            &directional_shadow,
            &spot_shadow,
            &point_shadow_dynamic,
        )?;
        directional_shadow.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        spot_shadow.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        point_shadow.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        point_shadow_dynamic.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        Ok(Self {
            directional_shadow,
            spot_shadow,
            point_shadow,
            point_shadow_dynamic,
        })
    }

    /// The directional shadow map's sampling view (compare sampler).
    pub fn directional_shadow_view(&self) -> vk::ImageView {
        self.directional_shadow.view()
    }

    /// The spot shadow map's sampling view (compare sampler).
    pub fn spot_shadow_view(&self) -> vk::ImageView {
        self.spot_shadow.view()
    }

    /// The static point distance cube's sampling view (linear sampler, cube view-type).
    pub fn point_shadow_view(&self) -> vk::ImageView {
        self.point_shadow.cube_view()
    }

    /// The dynamic point distance cube's sampling view (linear sampler, cube view-type).
    pub fn point_shadow_dynamic_view(&self) -> vk::ImageView {
        self.point_shadow_dynamic.cube_view()
    }
}

/// Runs a one-off command buffer transitioning the directional + spot depth maps and the
/// point cube (all 6 layers) `UNDEFINED → SHADER_READ_ONLY_OPTIMAL`, submitting on the
/// graphics queue and waiting.
fn initialize_shadow_layouts(
    device: &Device,
    directional: &Image,
    spot: &Image,
    point: &PointShadowCube,
) -> Result<()> {
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "init cmd")?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "init fence",
    )?;

    let result = (|| -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. The barriers reference images this device created.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "init begin")?;
            let depth = init_barrier(directional.handle(), vk::ImageAspectFlags::DEPTH, 1);
            let spot_depth = init_barrier(spot.handle(), vk::ImageAspectFlags::DEPTH, 1);
            let cube = init_barrier(point.image(), vk::ImageAspectFlags::COLOR, 6);
            let barriers = [depth, spot_depth, cube];
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "init end")?;
        }
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. The queue is touched single-threaded at init.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "init submit")?;
            checked(raw.wait_for_fences(&[fence], true, u64::MAX), "init wait")?;
        }
        Ok(())
    })();

    // SAFETY: the ash seam. The fence was waited (or the submit never happened), so the
    // pool/fence are idle and destroyed exactly once.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

/// One `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` init barrier over `layer_count` layers of
/// `aspect` for the shadow-map init transition.
fn init_barrier(
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    layer_count: u32,
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count,
        })
}

/// A single-mip `SHADOW_MAP_SIZE`² depth image, usable as a depth attachment and sampled
/// with the compare sampler.
fn shadow_depth_map(resources: &Arc<DeviceResources>) -> Result<Image> {
    Image::new(
        resources,
        &ImageDesc {
            extent: vk::Extent2D {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
            },
            format: DEPTH_FORMAT,
            usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            aspect: vk::ImageAspectFlags::DEPTH,
            view_type: vk::ImageViewType::TYPE_2D,
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        },
    )
}

/// The omnidirectional point-shadow distance cube: a `CUBE_COMPATIBLE` 6-layer
/// `R32_SFLOAT` color image with a cube sampling view + 6 per-face render views, plus a
/// shared single-layer depth scratch the per-face passes clear and reuse.
///
/// A move-only Drop type owning its VMA allocation + 7 image views + the depth scratch.
pub struct PointShadowCube {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    allocation: vk_mem::Allocation,
    cube_view: vk::ImageView,
    face_views: [vk::ImageView; 6],
    depth: Image,
    /// The per-face extent (square).
    pub extent: vk::Extent2D,
    /// The cube's current layout, tracked by the point-shadow pass body (it ends
    /// `SHADER_READ_ONLY_OPTIMAL` so the scene samples it).
    pub layout: vk::ImageLayout,
}

// SAFETY: the handles carry no thread-affine state and vk-mem marks its `Allocation`
// Send/Sync; moving the cube across threads is sound (matches the other resource wrappers).
unsafe impl Send for PointShadowCube {}

impl PointShadowCube {
    /// Allocates the cube image, its cube + 6 per-face views, and the depth scratch.
    fn new(resources: &Arc<DeviceResources>) -> Result<Self> {
        let raw = resources.device();
        let size = POINT_SHADOW_SIZE;
        let extent = vk::Extent2D {
            width: size,
            height: size,
        };

        let image_info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::CUBE_COMPATIBLE)
            .image_type(vk::ImageType::TYPE_2D)
            .format(POINT_SHADOW_COLOR_FORMAT)
            .extent(vk::Extent3D {
                width: size,
                height: size,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(6)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-info is valid; the image + allocation are
        // owned and freed in `Drop` (or below on a later failure).
        let (image, mut allocation) = checked(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "create_image (point shadow cube)",
        )?;

        let cube_view = match create_cube_view(raw, image) {
            Ok(view) => view,
            Err(err) => {
                // SAFETY: free the image just created before returning the error.
                unsafe { resources.allocator().destroy_image(image, &mut allocation) };
                return Err(err);
            }
        };

        let mut face_views = [vk::ImageView::null(); 6];
        for (face, slot) in face_views.iter_mut().enumerate() {
            match create_face_view(raw, image, face as u32) {
                Ok(view) => *slot = view,
                Err(err) => {
                    // SAFETY: free the cube view + every face view created so far + the
                    // image, then return — the partial-failure cleanup.
                    unsafe {
                        raw.destroy_image_view(cube_view, None);
                        for created in &face_views[..face] {
                            raw.destroy_image_view(*created, None);
                        }
                        resources.allocator().destroy_image(image, &mut allocation);
                    }
                    return Err(err);
                }
            }
        }

        let depth = match Image::new(
            resources,
            &ImageDesc {
                extent,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        ) {
            Ok(depth) => depth,
            Err(err) => {
                // SAFETY: free every view + the image on the depth-scratch failure.
                unsafe {
                    raw.destroy_image_view(cube_view, None);
                    for view in &face_views {
                        raw.destroy_image_view(*view, None);
                    }
                    resources.allocator().destroy_image(image, &mut allocation);
                }
                return Err(err);
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            allocation,
            cube_view,
            face_views,
            depth,
            extent,
            layout: vk::ImageLayout::UNDEFINED,
        })
    }

    /// The cube sampling view (view-type CUBE), bound into the light set.
    pub fn cube_view(&self) -> vk::ImageView {
        self.cube_view
    }

    /// The cube color image handle (the point-shadow pass body barriers it directly).
    pub fn image(&self) -> vk::Image {
        self.image
    }

    /// The per-face 2D render view for cube layer `face` (0..6).
    pub fn face_view(&self, face: usize) -> vk::ImageView {
        self.face_views[face]
    }

    /// The shared depth-scratch image handle (reused across the 6 faces).
    pub fn depth_image(&self) -> vk::Image {
        self.depth.handle()
    }

    /// The shared depth-scratch view.
    pub fn depth_view(&self) -> vk::ImageView {
        self.depth.view()
    }
}

impl Drop for PointShadowCube {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps the device + allocator alive; the
        // run loop idled the device before teardown (README §4). The depth `Image` Drops
        // itself (its own field); here the cube + face views go through the device, then
        // the cube image through the allocator — each freed exactly once.
        let raw = self.resources.device();
        unsafe {
            raw.destroy_image_view(self.cube_view, None);
            for view in &self.face_views {
                raw.destroy_image_view(*view, None);
            }
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// Creates the cube (6-layer) sampling view over `image`.
fn create_cube_view(raw: &ash::Device, image: vk::Image) -> Result<vk::ImageView> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::CUBE)
        .format(POINT_SHADOW_COLOR_FORMAT)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 6,
        });
    // SAFETY: the ash seam. The view references the image just created; freed in Drop.
    checked(
        unsafe { raw.create_image_view(&info, None) },
        "create_image_view (cube)",
    )
}

/// Creates the single-layer 2D render view for cube layer `face`.
fn create_face_view(raw: &ash::Device, image: vk::Image, face: u32) -> Result<vk::ImageView> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(POINT_SHADOW_COLOR_FORMAT)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: face,
            layer_count: 1,
        });
    // SAFETY: the ash seam. As above; freed in Drop / the partial-failure path.
    checked(
        unsafe { raw.create_image_view(&info, None) },
        "create_image_view (cube face)",
    )
}
