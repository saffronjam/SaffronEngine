//! The surface swapchain + its per-image sync, recreated as a unit on resize.

use ash::vk;

use crate::{Device, Result, checked};

/// The swapchain and its per-image presentation sync.
///
/// Owns its images' views and one render-finished semaphore per image. Not a
/// `Drop` type — the handles borrow the device, so the owning [`crate::Renderer`]
/// calls [`Swapchain::destroy`] after `wait_idle`, before the device is torn down.
pub struct Swapchain {
    handle: vk::SwapchainKHR,
    /// The chosen swapchain image format.
    pub format: vk::Format,
    /// The swapchain extent in pixels.
    pub extent: vk::Extent2D,
    /// Whether the surface allowed `TRANSFER_SRC` (window-screenshot capture).
    pub capture_supported: bool,
    images: Vec<vk::Image>,
    image_views: Vec<vk::ImageView>,
    render_finished: Vec<vk::Semaphore>,
    images_in_flight: Vec<vk::Fence>,
}

impl Swapchain {
    /// Builds the swapchain for `(width, height)` against the device's surface.
    ///
    /// Clamps the requested extent to the surface capabilities, picks a FIFO
    /// present mode (always supported), and requests `min_image_count + 1` images
    /// (clamped to `max_image_count`). Resize is handled by the renderer
    /// destroying the old swapchain after the device is idle, then building a
    /// fresh one.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call.
    pub fn new(device: &Device, width: u32, height: u32) -> Result<Self> {
        // The swapchain is built only for the windowed host, which always has a
        // surface (`SurfaceSource::Window`); the offscreen host never reaches here.
        let surface_loader = device
            .surface_loader()
            .expect("swapchain requires a surface (windowed host only)");
        let surface = device
            .surface()
            .expect("swapchain requires a surface (windowed host only)");
        let physical_device = device.physical_device();

        // SAFETY: the ash seam. Surface-capabilities query on the chosen device.
        let caps = checked(
            unsafe {
                surface_loader.get_physical_device_surface_capabilities(physical_device, surface)
            },
            "get_physical_device_surface_capabilities",
        )?;

        let extent = choose_extent(&caps, width, height);
        let image_count = choose_image_count(&caps);

        let format = device.surface_format;
        let mut usage = vk::ImageUsageFlags::TRANSFER_DST;
        let capture_supported = caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC);
        if capture_supported {
            usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
        usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;

        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true);

        let swapchain_loader = device.swapchain_loader();
        // SAFETY: the ash seam. The create-info is valid for the call; the returned
        // swapchain is owned and destroyed in `destroy`.
        let handle = checked(
            unsafe { swapchain_loader.create_swapchain(&create_info, None) },
            "create_swapchain",
        )?;

        // SAFETY: the ash seam. Retrieves the images of the swapchain just created.
        let images = checked(
            unsafe { swapchain_loader.get_swapchain_images(handle) },
            "get_swapchain_images",
        )?;

        let mut swapchain = Self {
            handle,
            format: format.format,
            extent,
            capture_supported,
            images,
            image_views: Vec::new(),
            render_finished: Vec::new(),
            images_in_flight: Vec::new(),
        };

        if let Err(err) = swapchain.create_per_image(device) {
            swapchain.destroy(device);
            return Err(err);
        }
        Ok(swapchain)
    }

    fn create_per_image(&mut self, device: &Device) -> Result<()> {
        let raw = device.raw();
        for &image in &self.images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(self.format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            // SAFETY: the ash seam. The image belongs to the swapchain just built;
            // the view is owned and destroyed in `destroy`.
            let view = checked(
                unsafe { raw.create_image_view(&view_info, None) },
                "create_image_view",
            )?;
            self.image_views.push(view);

            // SAFETY: the ash seam. Default-info semaphore creation.
            let semaphore = checked(
                unsafe { raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) },
                "create_semaphore",
            )?;
            self.render_finished.push(semaphore);
        }
        self.images_in_flight = vec![vk::Fence::null(); self.images.len()];
        Ok(())
    }

    /// Destroys the swapchain, its views, and its per-image semaphores. Must be
    /// called after `wait_idle`, before the device is torn down.
    pub fn destroy(&mut self, device: &Device) {
        let raw = device.raw();
        // SAFETY: the ash seam. `wait_idle` ran first; each handle is destroyed
        // exactly once and belongs to this device.
        unsafe {
            for &view in &self.image_views {
                raw.destroy_image_view(view, None);
            }
            for &semaphore in &self.render_finished {
                raw.destroy_semaphore(semaphore, None);
            }
            if self.handle != vk::SwapchainKHR::null() {
                device
                    .swapchain_loader()
                    .destroy_swapchain(self.handle, None);
            }
        }
        self.image_views.clear();
        self.render_finished.clear();
        self.images_in_flight.clear();
        self.handle = vk::SwapchainKHR::null();
    }

    /// The raw swapchain handle (for acquire / present).
    pub fn handle(&self) -> vk::SwapchainKHR {
        self.handle
    }

    /// The number of swapchain images.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// The image at `index` (returned by acquire).
    pub fn image(&self, index: usize) -> vk::Image {
        self.images[index]
    }

    /// The render-finished semaphore for image `index`.
    pub fn render_finished(&self, index: usize) -> vk::Semaphore {
        self.render_finished[index]
    }

    /// The fence currently tracking image `index` (null if none).
    pub fn image_in_flight(&self, index: usize) -> vk::Fence {
        self.images_in_flight[index]
    }

    /// Records that `fence` now tracks image `index`.
    pub fn set_image_in_flight(&mut self, index: usize, fence: vk::Fence) {
        self.images_in_flight[index] = fence;
    }
}

/// Clamps the requested extent to the surface's allowed range, honoring the
/// `current_extent == u32::MAX` "surface defers to the app" convention.
fn choose_extent(caps: &vk::SurfaceCapabilitiesKHR, width: u32, height: u32) -> vk::Extent2D {
    if caps.current_extent.width != u32::MAX {
        return caps.current_extent;
    }
    vk::Extent2D {
        width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
        height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
    }
}

/// Requests one more than the minimum image count, clamped to the maximum (0 means
/// no maximum).
fn choose_image_count(caps: &vk::SurfaceCapabilitiesKHR) -> u32 {
    let desired = caps.min_image_count + 1;
    if caps.max_image_count > 0 {
        desired.min(caps.max_image_count)
    } else {
        desired
    }
}
