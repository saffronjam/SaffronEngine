//! The RAII cube / 2D image wrappers the bake chain fills: an environment cube with its
//! per-face and per-mip views, and a single-layer LUT image.

use super::*;

/// A `CUBE_COMPATIBLE` 6-layer color cube (sampled + storage) owning its handle, a
/// `CUBE` sampling view, and the VMA allocation. The convolution passes write it through
/// transient per-mip `TYPE_2D_ARRAY` storage views the bake creates and frees itself.
///
/// A move-only Drop type — its view goes through the device, its image through the
/// allocator (the `Image::reset()` order). `layout` tracks the cross-bake layout (the
/// bake's `UNDEFINED → GENERAL → SHADER_READ_ONLY` cycle).
pub(super) struct IblCube {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
}

pub(super) struct IblCubeSet {
    pub(super) env: IblCube,
    pub(super) initialized: bool,
}

impl IblCubeSet {
    pub(super) fn new(resources: &Arc<DeviceResources>) -> Result<Self> {
        Ok(Self {
            env: IblCube::new(resources, IBL_ENV_SIZE, IBL_ENV_SIZE.ilog2() + 1)?,
            initialized: false,
        })
    }
}

// SAFETY: as `Image` — no thread-affine state; vk-mem `Allocation` is Send.
unsafe impl Send for IblCube {}

impl IblCube {
    /// Creates a `size`²×6 cube of `mip_levels` mips, `IBL_COLOR_FORMAT`, sampled +
    /// storage, dedicated-allocated, with a `CUBE` sampling view spanning all mips/layers.
    pub(super) fn new(
        resources: &Arc<DeviceResources>,
        size: u32,
        mip_levels: u32,
    ) -> Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .flags(vk::ImageCreateFlags::CUBE_COMPATIBLE)
            .image_type(vk::ImageType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .extent(vk::Extent3D {
                width: size,
                height: size,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(6)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            // TRANSFER_SRC|DST so the env cube can blit-generate its mip chain (filtered
            // importance sampling reads coarser source mips); harmless on the other cubes.
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image + allocation are
        // owned and freed in `Drop` (or below on a view-creation failure).
        let (image, allocation) = checked(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage (ibl cube)",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::CUBE)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 6,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe {
                    resources.allocator().destroy_image(image, &mut allocation);
                }
                return Err(Error::Vk {
                    context: "create_image_view (ibl cube)",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
        })
    }

    /// A transient `TYPE_2D_ARRAY` storage view over one mip (all 6 layers) — the
    /// convolution passes bind these as the storage-image output; the caller frees them.
    pub(super) fn storage_view(&self, mip: u32) -> Result<vk::ImageView> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(self.image)
            .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: mip,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 6,
            });
        // SAFETY: the ash seam. The transient view is freed by the bake before returning.
        checked(
            unsafe { self.resources.device().create_image_view(&view_info, None) },
            "create_image_view (ibl storage)",
        )
    }
}

impl Drop for IblCube {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; the view
        // is destroyed through the device, then the image through the allocator. Each
        // handle is freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// A 2D color image (sampled + storage), the IBL BRDF LUT and the three atmosphere LUTs.
/// A move-only Drop type.
pub(super) struct IblImage {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
}

// SAFETY: as `Image` — no thread-affine state; vk-mem `Allocation` is Send.
unsafe impl Send for IblImage {}

impl IblImage {
    /// Creates a `width`×`height` `IBL_COLOR_FORMAT` image, sampled + storage, with a
    /// `TYPE_2D` view that doubles as both the sampled and storage view.
    pub(super) fn new(resources: &Arc<DeviceResources>, width: u32, height: u32) -> Result<Self> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            ..Default::default()
        };
        // SAFETY: the VMA seam. Owned + freed in `Drop` (or below on a view failure).
        let (image, allocation) = checked(
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) },
            "vmaCreateImage (ibl lut)",
        )?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(IBL_COLOR_FORMAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the image just created.
        let view = match unsafe { resources.device().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the image before the early return.
                unsafe {
                    resources.allocator().destroy_image(image, &mut allocation);
                }
                return Err(Error::Vk {
                    context: "create_image_view (ibl lut)",
                    result,
                });
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
        })
    }
}

impl Drop for IblImage {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. View through the device, then image through the
        // allocator. Each handle freed exactly once.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}
