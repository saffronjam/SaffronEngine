//! The RAII 2D / 3D image wrappers and the min/max pyramid the displacement path samples.

use super::*;

/// How to create an [`Image`]: extent + format + usage + the view's aspect/type
/// and mip/layer counts. A parameter struct so [`Image::new`] reads as named fields
/// rather than a positional argument list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDesc {
    /// The 2D image extent.
    pub extent: vk::Extent2D,
    /// The image + view format.
    pub format: vk::Format,
    /// Image usage flags.
    pub usage: vk::ImageUsageFlags,
    /// The view aspect (`COLOR` / `DEPTH`).
    pub aspect: vk::ImageAspectFlags,
    /// The view type (`TYPE_2D` / `CUBE` / `TYPE_2D_ARRAY` …).
    pub view_type: vk::ImageViewType,
    /// Mip levels of the image and the view range.
    pub mip_levels: u32,
    /// Array layers of the image and the view range.
    pub array_layers: u32,
    /// MSAA sample count (`TYPE_1` for a normal single-sampled image; > 1 for a
    /// multisampled scene target resolved into a 1× image).
    pub samples: vk::SampleCountFlags,
}

impl ImageDesc {
    /// A single-mip, single-layer 2D color image with a `COLOR`-aspect `TYPE_2D`
    /// view — the common offscreen-target case (single-sampled).
    pub fn color_2d(extent: vk::Extent2D, format: vk::Format, usage: vk::ImageUsageFlags) -> Self {
        Self {
            extent,
            format,
            usage,
            aspect: vk::ImageAspectFlags::COLOR,
            view_type: vk::ImageViewType::TYPE_2D,
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        }
    }
}

/// A VMA-allocated 2D image owning its handle, view, and allocation.
///
/// `layout` tracks the image's current layout across frames (the render graph seeds
/// and updates it). [`Drop`] frees the view (through the device) then the image
/// (through the allocator).
pub struct Image {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
    /// The image extent.
    pub extent: vk::Extent2D,
    /// The image format.
    pub format: vk::Format,
    /// The current image layout, tracked across frames by the render graph.
    pub layout: vk::ImageLayout,
    pub(super) graph_state: crate::RgExternalState,
}

// SAFETY: the image/view/allocation handles carry no thread-affine state and
// vk-mem marks its `Allocation` Send/Sync; moving an `Image` across threads is sound.
unsafe impl Send for Image {}

impl Image {
    /// Creates a 2D image + a full-subresource view per `desc`, allocated
    /// device-local.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if image or view creation fails (the image is
    /// freed before returning on a view failure).
    pub fn new(resources: &Arc<DeviceResources>, desc: &ImageDesc) -> crate::Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .flags(if desc.view_type == vk::ImageViewType::CUBE {
                vk::ImageCreateFlags::CUBE_COMPATIBLE
            } else {
                vk::ImageCreateFlags::empty()
            })
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format)
            .extent(vk::Extent3D {
                width: desc.extent.width,
                height: desc.extent.height,
                depth: 1,
            })
            .mip_levels(desc.mip_levels)
            .array_layers(desc.array_layers)
            .samples(desc.samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(desc.usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image + allocation
        // are owned and freed in `Drop` (or below on a view-creation failure).
        let (image, allocation) = checked_vma(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(desc.view_type)
            .format(desc.format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: desc.aspect,
                base_mip_level: 0,
                level_count: desc.mip_levels,
                base_array_layer: 0,
                layer_count: desc.array_layers,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image we just created before the
                // early return; the allocator is live (the bundle outlives us).
                unsafe { resources.allocator().destroy_image(image, &mut allocation) };
                return Err(crate::Error::Vk {
                    context: "create_image_view",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
            extent: desc.extent,
            format: desc.format,
            layout: vk::ImageLayout::UNDEFINED,
            graph_state: crate::RgExternalState::new(vk::ImageLayout::UNDEFINED),
        })
    }

    /// Creates a 2D image with **no** view — for a transfer-only target (the shm-capture
    /// BGRA8 blit destination) whose usage (`TRANSFER_*` only) cannot back an image view.
    /// [`Image::view`] returns a null handle; Drop's `destroy_image_view(null)` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if image creation fails.
    pub fn new_no_view(resources: &Arc<DeviceResources>, desc: &ImageDesc) -> crate::Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .flags(if desc.view_type == vk::ImageViewType::CUBE {
                vk::ImageCreateFlags::CUBE_COMPATIBLE
            } else {
                vk::ImageCreateFlags::empty()
            })
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format)
            .extent(vk::Extent3D {
                width: desc.extent.width,
                height: desc.extent.height,
                depth: 1,
            })
            .mip_levels(desc.mip_levels)
            .array_layers(desc.array_layers)
            .samples(desc.samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(desc.usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-info is valid; the image + allocation are
        // owned and freed in `Drop`.
        let (image, allocation) = checked_vma(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage (no view)",
        )?;
        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view: vk::ImageView::null(),
            allocation,
            extent: desc.extent,
            format: desc.format,
            layout: vk::ImageLayout::UNDEFINED,
            graph_state: crate::RgExternalState::new(vk::ImageLayout::UNDEFINED),
        })
    }

    /// The image handle.
    pub fn handle(&self) -> vk::Image {
        self.image
    }

    /// The full-subresource image view.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }

    /// Complete cross-frame render-graph state for this image.
    pub fn graph_state(&self) -> crate::RgExternalState {
        self.graph_state.with_layout(self.layout)
    }

    /// Stores the image state resolved by the render graph.
    pub fn set_graph_state(&mut self, state: crate::RgExternalState) {
        self.layout = state.layout;
        self.graph_state = state;
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; the
        // view is destroyed through the device, then the image through the
        // allocator, in that order. Each handle is freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// A VMA-allocated 3D image (the GDF cascade clipmap volumes + the lite albedo cache), owning
/// handle + view + allocation.
pub struct Image3D {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
    /// The 3D image extent.
    pub extent: vk::Extent3D,
    /// The image format.
    pub format: vk::Format,
    /// The current image layout, tracked across frames by the render graph.
    pub layout: vk::ImageLayout,
    pub(super) graph_state: crate::RgExternalState,
}

// SAFETY: as [`Image`] — no thread-affine state; vk-mem `Allocation` is Send.
unsafe impl Send for Image3D {}

impl Image3D {
    /// Creates a 3D image (with `mip_levels` mip levels) + a `TYPE_3D` view spanning every
    /// level, allocated device-local.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if image or view creation fails (the image is
    /// freed before returning on a view failure).
    pub fn new(
        resources: &Arc<DeviceResources>,
        extent: vk::Extent3D,
        format: vk::Format,
        mip_levels: u32,
        usage: vk::ImageUsageFlags,
    ) -> crate::Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_3D)
            .format(format)
            .extent(extent)
            .mip_levels(mip_levels.max(1))
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the VMA seam. As [`Image::new`]; the image is freed in `Drop` or
        // below on a view-creation failure.
        let (image, allocation) = checked_vma(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage3D",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_3D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels.max(1),
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe { resources.allocator().destroy_image(image, &mut allocation) };
                return Err(crate::Error::Vk {
                    context: "create_image_view_3d",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
            extent,
            format,
            layout: vk::ImageLayout::UNDEFINED,
            graph_state: crate::RgExternalState::new(vk::ImageLayout::UNDEFINED),
        })
    }

    /// The image handle.
    pub fn handle(&self) -> vk::Image {
        self.image
    }

    /// The `TYPE_3D` image view.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }

    /// Complete cross-frame render-graph state for this image.
    pub fn graph_state(&self) -> crate::RgExternalState {
        self.graph_state.with_layout(self.layout)
    }

    /// Stores the image state resolved by the render graph.
    pub fn set_graph_state(&mut self, state: crate::RgExternalState) {
        self.layout = state.layout;
        self.graph_state = state;
    }
}

impl Drop for Image3D {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. View through the device, then image through the
        // allocator, in that order. Each handle freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// A per-height min/max pyramid image (`R32G32_SFLOAT`, min in R / max in G, one mip per pyramid
/// level), owned by the [`GpuTexture`] it was built for and freed with it.
///
/// Written into the parallel `heightMinMaxTextures` bindless array (binding 4) at the owning
/// texture's own slot, so the tessellation factor kernel's `heightIndex` addresses both the texture
/// and its pyramid. Only a displacement height map carries one; every other texture leaves it `None`.
pub struct MinMaxPyramid {
    /// The pyramid image.
    pub image: vk::Image,
    /// The view over every pyramid mip.
    pub view: vk::ImageView,
    /// The pyramid image's VMA allocation.
    pub allocation: vk_mem::Allocation,
}

/// The default 1×1 min/max pyramid (`(0, 0)` → zero local range) seeded into every unbound
/// `heightMinMaxTextures` slot at init, held by the renderer for its lifetime.
///
/// Its own RAII teardown (freed on [`Drop`]), so a non-displacement texture's slot keeps a valid view
/// pointing here (partially-bound arrays fault on an unbound slot on some drivers). A displacement
/// height map overwrites its slot with its real [`MinMaxPyramid`].
pub struct DefaultHeightMinMax {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
}

impl DefaultHeightMinMax {
    /// Wraps the created default image/view/allocation, owning their teardown.
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        image: vk::Image,
        view: vk::ImageView,
        allocation: vk_mem::Allocation,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
        }
    }

    /// The default pyramid view seeded into every `heightMinMaxTextures` slot.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }
}

impl Drop for DefaultHeightMinMax {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; view then image, each
        // freed exactly once. Every displacement slot that overwrote its descriptor has already freed
        // its own pyramid with its `GpuTexture`, so this frees only the default.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}
