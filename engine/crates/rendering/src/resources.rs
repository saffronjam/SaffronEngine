//! The move-only RAII GPU resource wrappers — `Buffer`, `Image`, `Image3D`,
//! `GpuTexture`, `GpuMesh`, `Pipeline`, `AccelerationStructure` — each an
//! `impl Drop` type that frees its handles, plus the [`DeviceResources`] bundle the
//! device shares so a resource can free itself without a live `&Device`.
//!
//! The move is the language's job and freeing the handle is the `Drop` body. A
//! borrowed-handle trick would be unsafe (nothing makes the device outlive the
//! resource), so instead of borrowing raw handles, every wrapper holds an
//! [`Arc`]`<`[`DeviceResources`]`>`: the ash
//! device + the VMA allocator behind one `Arc`. The device/allocator are destroyed
//! only when the last clone drops (README §4: "the device must outlive every
//! resource" — here it is *structural*, not field-order-hopeful), and the `Arc`
//! makes the wrappers `Send`, which the off-thread `GpuTexture` drop needs (§5).

use std::sync::{Arc, Mutex};

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{Submesh, Vertex, VertexSkin};
use vk_mem::{Alloc, Allocator};

/// The shared bindless texture free-list: returned slot indices a later upload
/// reuses. README §5's second `Arc<Mutex>` site — a [`GpuTexture`]'s `Drop` locks
/// it and pushes its slot back, even off the main thread. Every texture holds a
/// clone of this shared free-list.
pub type BindlessFreeList = Arc<Mutex<Vec<u32>>>;

/// The device + allocator handles a GPU resource needs to free itself, shared
/// behind one `Arc` so the resource can `Drop` without a live `&Device`.
///
/// Rust cannot encode "borrowed but the owner outlives me" for a `Drop` type, so the
/// two handles live behind this `Arc`: a resource clones the `Arc` at construction, and
/// the allocator/device are destroyed only when the last holder (the [`super::Device`]
/// itself, normally the final one after the run loop's `wait_idle` and resource
/// teardown) drops. The [`Drop`] frees the allocator before the device
/// (`vmaDestroyAllocator` before `vkDestroyDevice`).
pub struct DeviceResources {
    /// The VMA allocator. `Option` so [`Drop`] can free it before the device.
    allocator: Option<Allocator>,
    /// The ash logical device (a cheap handle + `Arc`'d fn table — its own clone is
    /// not used; this single owned copy is destroyed in [`Drop`]).
    device: ash::Device,
}

impl DeviceResources {
    /// Bundles the allocator + device. Called once by [`super::Device::new`]; the
    /// returned `Arc` is the canonical holder cloned into every resource.
    pub(crate) fn new(device: ash::Device, allocator: Allocator) -> Arc<Self> {
        Arc::new(Self {
            allocator: Some(allocator),
            device,
        })
    }

    /// The ash logical device (resource creation / view + handle teardown).
    pub(crate) fn device(&self) -> &ash::Device {
        &self.device
    }

    /// The VMA allocator (image/buffer create + destroy). Present for the whole
    /// bundle lifetime; only [`Drop`] takes it (to free it before the device).
    pub(crate) fn allocator(&self) -> &Allocator {
        self.allocator
            .as_ref()
            .expect("allocator lives until DeviceResources::drop")
    }

    /// The device address of `buffer` (core 1.2 `vkGetBufferDeviceAddress`), for feeding
    /// AS-build vertex / index / scratch input. The buffer must carry
    /// `SHADER_DEVICE_ADDRESS` usage. Lives here so the upload path (which holds only the
    /// bundle, not a `&Device`) can address its mesh buffers for the BLAS build.
    pub(crate) fn buffer_device_address(&self, buffer: vk::Buffer) -> vk::DeviceAddress {
        let info = vk::BufferDeviceAddressInfo::default().buffer(buffer);
        // SAFETY: the ash seam. The buffer was created with `SHADER_DEVICE_ADDRESS` usage;
        // the returned address is valid for the device's lifetime.
        unsafe { self.device.get_buffer_device_address(&info) }
    }
}

impl Drop for DeviceResources {
    fn drop(&mut self) {
        // The VMA allocator frees its own `VkDeviceMemory` through the live device,
        // so it must go before `vkDestroyDevice`.
        // Field order alone cannot guarantee it (the allocator is an `Option` so its
        // `Drop` runs here, ahead of the `device` field's drop).
        drop(self.allocator.take());
        // SAFETY: the ash seam. The run loop idled the device before any teardown
        // (README §4 / PP-10), and this is the last `Arc<DeviceResources>` holder,
        // so no handle this device created is still live. Destroyed exactly once.
        unsafe { self.device.destroy_device(None) };
    }
}

/// Maps an ash `VkResult<T>` from a VMA create call into this crate's [`super::Error`].
fn checked_vma<T>(
    result: std::result::Result<T, vk::Result>,
    context: &'static str,
) -> crate::Result<T> {
    crate::checked(result, context)
}

/// A move-only VMA buffer. When [`Buffer::mapped`] is non-null the allocation is
/// persistently mapped for per-frame host writes; [`Drop`] frees it before the
/// allocator is destroyed (the bundle's `Arc` keeps the allocator alive until then).
pub struct Buffer {
    resources: Arc<DeviceResources>,
    buffer: vk::Buffer,
    allocation: vk_mem::Allocation,
    mapped: *mut u8,
    size: vk::DeviceSize,
}

// SAFETY: the raw `mapped` pointer is into VMA-owned, allocation-lifetime memory;
// the allocation/buffer handles are `Send` (vk-mem marks `Allocation` Send/Sync).
// The buffer carries no thread-affine state, so moving it across threads is sound.
unsafe impl Send for Buffer {}

impl Buffer {
    /// Creates a buffer of `size` with `usage`, allocated per `alloc_info`.
    ///
    /// When `alloc_info` requests `MAPPED`, [`Buffer::mapped`] returns the persistent
    /// host pointer; otherwise it is null.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if `vmaCreateBuffer` fails.
    pub fn new(
        resources: &Arc<DeviceResources>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        alloc_info: &vk_mem::AllocationCreateInfo,
    ) -> crate::Result<Self> {
        let buffer_info = vk::BufferCreateInfo::default().size(size).usage(usage);
        // SAFETY: the VMA seam. The create-infos are valid for the call; the
        // returned buffer + allocation are owned and freed in `Drop`.
        let (buffer, allocation) = checked_vma(
            unsafe {
                resources
                    .allocator()
                    .create_buffer(&buffer_info, alloc_info)
            },
            "vmaCreateBuffer",
        )?;
        let mapped = resources
            .allocator()
            .get_allocation_info(&allocation)
            .mapped_data
            .cast::<u8>();
        Ok(Self {
            resources: Arc::clone(resources),
            buffer,
            allocation,
            mapped,
            size,
        })
    }

    /// The buffer handle.
    pub fn handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// The buffer size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// The persistent host-mapped pointer, or null when the buffer was not created
    /// `MAPPED`.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped
    }

    /// The mapped allocation as a writable byte slice, or `None` when unmapped.
    ///
    /// Callers writing GPU-visible structs through this must respect the std430
    /// layout contract (README §3); the slice spans the full [`Buffer::size`].
    pub fn mapped_bytes(&mut self) -> Option<&mut [u8]> {
        if self.mapped.is_null() {
            return None;
        }
        // SAFETY: the allocation is HOST_VISIBLE + persistently MAPPED for `size`
        // bytes; the `&mut self` borrow makes the slice exclusive.
        Some(unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size as usize) })
    }

    /// Flushes the complete mapped allocation before GPU reads.
    pub(crate) fn flush_mapped(&self) -> crate::Result<()> {
        checked_vma(
            self.resources
                .allocator()
                .flush_allocation(&self.allocation, 0, self.size),
            "vmaFlushAllocation",
        )
    }

    /// Invalidates the complete mapped allocation before host readback.
    pub(crate) fn invalidate_mapped(&self) -> crate::Result<()> {
        checked_vma(
            self.resources
                .allocator()
                .invalidate_allocation(&self.allocation, 0, self.size),
            "vmaInvalidateAllocation",
        )
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The `Arc<DeviceResources>` keeps the allocator alive
        // for this call; the buffer/allocation are destroyed exactly once.
        unsafe {
            self.resources
                .allocator()
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}

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
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
    /// The image extent.
    pub extent: vk::Extent2D,
    /// The image format.
    pub format: vk::Format,
    /// The current image layout, tracked across frames by the render graph.
    pub layout: vk::ImageLayout,
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
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
    /// The 3D image extent.
    pub extent: vk::Extent3D,
    /// The image format.
    pub format: vk::Format,
    /// The current image layout, tracked across frames by the render graph.
    pub layout: vk::ImageLayout,
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
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
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

/// A device-local sampled texture (image + view) that also owns a bindless slot.
///
/// [`Drop`] returns the bindless slot to the shared free-list under the mutex (so a
/// worker-uploaded texture
/// destroyed off the main thread is safe — README §5), then frees the view and
/// image. The sampler is shared (the renderer's linear sampler), so it is not owned
/// here. A displacement height map additionally owns its [`MinMaxPyramid`] (freed here).
pub struct GpuTexture {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
    bindless_index: u32,
    free_list: Option<BindlessFreeList>,
    /// The per-height min/max pyramid, present only for a displacement height map.
    min_max: Option<MinMaxPyramid>,
    /// The texture extent.
    pub extent: vk::Extent2D,
    /// The texture format.
    pub format: vk::Format,
}

// SAFETY: the free-list is `Arc<Mutex<_>>` (Send+Sync); the image/view/allocation
// carry no thread-affine state. A `GpuTexture` is moved to a worker thread and
// dropped there — the bindless-slot-return path is exactly why this must be `Send`.
unsafe impl Send for GpuTexture {}
// SAFETY: every field is shared read-only after construction (the raw image/view +
// `vk_mem::Allocation` carry no interior mutability and are mutated only through
// `&mut self`); the free-list is `Arc<Mutex<_>>`. The thumbnail worker's handback
// hands an `Arc<GpuTexture>` back to the main thread through an `Arc<Mutex<_>>`
// (README §5 / the assets Ref-policy ledger), which requires `GpuTexture: Sync`.
unsafe impl Sync for GpuTexture {}

/// The pieces an upload assembles a [`GpuTexture`] from: the created image + view +
/// allocation, the claimed bindless slot, and the extent/format. A parameter struct
/// so [`GpuTexture::from_parts`] reads as named fields.
pub struct GpuTextureParts {
    /// The device-local image handle.
    pub image: vk::Image,
    /// The sampled image view.
    pub view: vk::ImageView,
    /// The image's VMA allocation.
    pub allocation: vk_mem::Allocation,
    /// The claimed slot in the bindless array (set 0).
    pub bindless_index: u32,
    /// The image extent.
    pub extent: vk::Extent2D,
    /// The image format.
    pub format: vk::Format,
    /// The per-height min/max pyramid, for a displacement height map only (else `None`).
    pub min_max: Option<MinMaxPyramid>,
}

impl GpuTexture {
    /// Wraps an already-created image + view as a bindless texture occupying
    /// `parts.bindless_index`, returning that slot to `free_list` on [`Drop`].
    ///
    /// The upload path (a later phase) creates the device-local image, records the
    /// staging copy, claims a `bindless_index` under the bindless mutex, then hands
    /// the pieces here. This wrapper owns the teardown.
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        parts: GpuTextureParts,
        free_list: &BindlessFreeList,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            image: parts.image,
            view: parts.view,
            allocation: parts.allocation,
            bindless_index: parts.bindless_index,
            free_list: Some(Arc::clone(free_list)),
            min_max: parts.min_max,
            extent: parts.extent,
            format: parts.format,
        }
    }

    /// The image handle.
    pub fn handle(&self) -> vk::Image {
        self.image
    }

    /// The sampled image view.
    pub fn view(&self) -> vk::ImageView {
        self.view
    }

    /// The per-height min/max pyramid view, if this texture is a displacement height map.
    pub fn min_max_view(&self) -> Option<vk::ImageView> {
        self.min_max.as_ref().map(|p| p.view)
    }

    /// This texture's slot in the bindless array (set 0).
    pub fn bindless_index(&self) -> u32 {
        self.bindless_index
    }
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        // Reclaim the bindless slot for reuse, under the shared mutex — a
        // worker-uploaded texture may be dropped off the main thread. The
        // descriptor still points at the destroyed view, but no live material
        // references the slot; the next upload overwrites it.
        if let Some(free_list) = self.free_list.take()
            && let Ok(mut slots) = free_list.lock()
        {
            slots.push(self.bindless_index);
        }
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; view
        // then image, each freed exactly once. The min/max pyramid (if any) frees the same way; the
        // heightMinMax descriptor still points at the destroyed view, but no live displacement material
        // references the slot (its `Arc<GpuTexture>` is gone), and the next upload overwrites it.
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
            if let Some(mut pyramid) = self.min_max.take() {
                self.resources
                    .device()
                    .destroy_image_view(pyramid.view, None);
                self.resources
                    .allocator()
                    .destroy_image(pyramid.image, &mut pyramid.allocation);
            }
        }
    }
}

/// A device-local creative look-up table: one `R16G16B16A16_SFLOAT` `N×N×N` 3D image sampled by the
/// tonemap pass's tetrahedral LUT stage (binding 2). Unlike [`GpuTexture`] it occupies no bindless
/// slot — it binds directly to the per-view tonemap set — so its [`Drop`] just frees the view + image.
/// Held as an `Arc<GpuLut>` on the renderer (the assigned creative look) or on the asset catalog's LUT
/// cache; the identity default LUT the renderer keeps is one of these too.
pub struct GpuLut {
    resources: Arc<DeviceResources>,
    image: vk::Image,
    view: vk::ImageView,
    allocation: vk_mem::Allocation,
    /// The table resolution per axis (`2` identity, `17`/`33`/`65` imported, `33` baked).
    size: u32,
}

// SAFETY: as [`GpuTexture`] — the image/view/allocation carry no thread-affine state; a `GpuLut` is
// held behind an `Arc` shared read-only after construction.
unsafe impl Send for GpuLut {}
unsafe impl Sync for GpuLut {}

impl GpuLut {
    /// Wraps an already-created `TYPE_3D` image + view as a creative LUT of `size` per axis. The
    /// upload path creates the device-local image, records the staging copy, then hands the pieces
    /// here; this wrapper owns the teardown.
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        image: vk::Image,
        view: vk::ImageView,
        allocation: vk_mem::Allocation,
        size: u32,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            image,
            view,
            allocation,
            size,
        }
    }

    /// The `TYPE_3D` sampled image view (bound at binding 2 of the tonemap set).
    pub fn view(&self) -> vk::ImageView {
        self.view
    }

    /// The table resolution per axis.
    pub fn size(&self) -> u32 {
        self.size
    }
}

impl Drop for GpuLut {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive for this call; view then
        // image, each freed exactly once. Idled before teardown (README §4).
        unsafe {
            self.resources.device().destroy_image_view(self.view, None);
            self.resources
                .allocator()
                .destroy_image(self.image, &mut self.allocation);
        }
    }
}

/// A device-local per-mesh signed distance field (sparse SDST v2): two 3D images — the
/// `R16_SNORM` brick *atlas* (occupied 8³ bricks) and the `R32_UINT` brick *indirection*
/// volume — sharing one bindless slot (the cone-trace indexes both at the same index), plus
/// the local-space grid metadata the shader's brick tap needs (padded bounds, encode clamp,
/// fine voxel dims, indirection dims, atlas tiling).
///
/// Owns the same teardown discipline as [`GpuTexture`]: [`Drop`] returns the bindless slot
/// to the shared SDF free-list under the mutex (a worker-uploaded mesh's field may be
/// dropped off the main thread), then frees both views + images. Held as an `Arc<GpuSdf>`
/// on the [`GpuMesh`] it was baked for, so it lives exactly as long as the mesh.
pub struct GpuSdf {
    resources: Arc<DeviceResources>,
    atlas_image: vk::Image,
    atlas_view: vk::ImageView,
    atlas_alloc: vk_mem::Allocation,
    indirection_image: vk::Image,
    indirection_view: vk::ImageView,
    indirection_alloc: vk_mem::Allocation,
    coverage_image: vk::Image,
    coverage_view: vk::ImageView,
    coverage_alloc: vk_mem::Allocation,
    bindless_index: u32,
    free_list: Option<BindlessFreeList>,
    /// The padded grid lower corner, local (rest) space.
    pub bounds_min: Vec3,
    /// The padded grid upper corner, local (rest) space.
    pub bounds_max: Vec3,
    /// The `R16_SNORM` distance normalization clamp: a sampled `+1.0` denormalizes to
    /// `+max_dist` local units.
    pub max_dist: f32,
    /// The fine voxel count per axis.
    pub voxel_dims: [u32; 3],
    /// The brick indirection-volume dims (bricks per axis).
    pub indirection_dims: [u32; 3],
    /// The atlas tiling (occupied bricks per axis in the atlas image).
    pub atlas_bricks: [u32; 3],
    /// The prefiltered atlas mip levels (the brick atlas image carries this many mips).
    pub mip_count: u32,
}

// SAFETY: as [`GpuTexture`] — the free-list is `Arc<Mutex<_>>` (Send+Sync); the
// image/view/allocation carry no thread-affine state. A `GpuSdf` rides inside an
// `Arc<GpuMesh>` the worker may build + drop off the main thread.
unsafe impl Send for GpuSdf {}
// SAFETY: every field is shared read-only after construction; the free-list is
// `Arc<Mutex<_>>`. The thumbnail worker hands an `Arc<GpuMesh>` (holding the `GpuSdf`)
// back to the main thread through an `Arc<Mutex<_>>` (README §5), which needs `Sync`.
unsafe impl Sync for GpuSdf {}

/// The pieces an upload assembles a [`GpuSdf`] from: the two created images + views +
/// allocations, the claimed (shared) SDF bindless slot, and the v2 brick metadata. A
/// parameter struct so [`GpuSdf::from_parts`] reads as named fields.
pub struct GpuSdfParts {
    /// The device-local `R16_SNORM` brick-atlas 3D image handle.
    pub atlas_image: vk::Image,
    /// The atlas `TYPE_3D` sampled image view.
    pub atlas_view: vk::ImageView,
    /// The atlas image's VMA allocation.
    pub atlas_alloc: vk_mem::Allocation,
    /// The device-local `R32_UINT` indirection-volume 3D image handle.
    pub indirection_image: vk::Image,
    /// The indirection `TYPE_3D` sampled image view.
    pub indirection_view: vk::ImageView,
    /// The indirection image's VMA allocation.
    pub indirection_alloc: vk_mem::Allocation,
    /// The device-local `R16_SNORM` coarse coverage 3D image handle (one texel per brick).
    pub coverage_image: vk::Image,
    /// The coverage `TYPE_3D` sampled image view.
    pub coverage_view: vk::ImageView,
    /// The coverage image's VMA allocation.
    pub coverage_alloc: vk_mem::Allocation,
    /// The claimed slot in the bindless SDF arrays (set 0, bindings 1 + 2 + 3).
    pub bindless_index: u32,
    /// The padded grid lower corner, local space.
    pub bounds_min: Vec3,
    /// The padded grid upper corner, local space.
    pub bounds_max: Vec3,
    /// The `R16_SNORM` distance normalization clamp.
    pub max_dist: f32,
    /// The fine voxel count per axis.
    pub voxel_dims: [u32; 3],
    /// The brick indirection-volume dims.
    pub indirection_dims: [u32; 3],
    /// The atlas tiling (bricks per axis).
    pub atlas_bricks: [u32; 3],
    /// The prefiltered atlas mip levels.
    pub mip_count: u32,
}

impl GpuSdf {
    /// Wraps the already-created atlas + indirection `Texture3D`s as a bindless field
    /// occupying `parts.bindless_index`, returning that slot to `free_list` on [`Drop`].
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        parts: GpuSdfParts,
        free_list: &BindlessFreeList,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            atlas_image: parts.atlas_image,
            atlas_view: parts.atlas_view,
            atlas_alloc: parts.atlas_alloc,
            indirection_image: parts.indirection_image,
            indirection_view: parts.indirection_view,
            indirection_alloc: parts.indirection_alloc,
            coverage_image: parts.coverage_image,
            coverage_view: parts.coverage_view,
            coverage_alloc: parts.coverage_alloc,
            bindless_index: parts.bindless_index,
            free_list: Some(Arc::clone(free_list)),
            bounds_min: parts.bounds_min,
            bounds_max: parts.bounds_max,
            max_dist: parts.max_dist,
            voxel_dims: parts.voxel_dims,
            indirection_dims: parts.indirection_dims,
            atlas_bricks: parts.atlas_bricks,
            mip_count: parts.mip_count,
        }
    }

    /// The brick-atlas image handle.
    pub fn atlas_handle(&self) -> vk::Image {
        self.atlas_image
    }

    /// The brick-atlas sampled `TYPE_3D` image view (bindless binding 1).
    pub fn atlas_view(&self) -> vk::ImageView {
        self.atlas_view
    }

    /// The indirection-volume sampled `TYPE_3D` image view (bindless binding 2).
    pub fn indirection_view(&self) -> vk::ImageView {
        self.indirection_view
    }

    /// The coarse coverage-volume sampled `TYPE_3D` image view (bindless binding 3).
    pub fn coverage_view(&self) -> vk::ImageView {
        self.coverage_view
    }

    /// This field's slot in the bindless SDF arrays (set 0, bindings 1 + 2).
    pub fn bindless_index(&self) -> u32 {
        self.bindless_index
    }
}

impl Drop for GpuSdf {
    fn drop(&mut self) {
        // Reclaim the SDF bindless slot for reuse, under the shared mutex — a
        // worker-built mesh's field may be dropped off the main thread.
        if let Some(free_list) = self.free_list.take()
            && let Ok(mut slots) = free_list.lock()
        {
            slots.push(self.bindless_index);
        }
        // SAFETY: the ash/VMA seam. The bundle keeps device + allocator alive; each view
        // then its image, freed exactly once.
        unsafe {
            let device = self.resources.device();
            let allocator = self.resources.allocator();
            device.destroy_image_view(self.atlas_view, None);
            allocator.destroy_image(self.atlas_image, &mut self.atlas_alloc);
            device.destroy_image_view(self.indirection_view, None);
            allocator.destroy_image(self.indirection_image, &mut self.indirection_alloc);
            device.destroy_image_view(self.coverage_view, None);
            allocator.destroy_image(self.coverage_image, &mut self.coverage_alloc);
        }
    }
}

/// A device-local mesh: vertex + index (+ optional skin) buffers, the submesh
/// ranges, the local-space AABB, the CPU-side copies retained for triangle-precise
/// picking, and the optional ray-tracing BLAS.
///
/// The three VMA buffers are freed in [`Drop`]; the [`AccelerationStructure`] is an
/// `Arc` (shared, read-only after build) and drops itself.
pub struct GpuMesh {
    resources: Arc<DeviceResources>,
    vertex_buffer: vk::Buffer,
    vertex_alloc: vk_mem::Allocation,
    index_buffer: vk::Buffer,
    index_alloc: vk_mem::Allocation,
    /// The skin stream buffer + allocation (`None` for unskinned meshes).
    skin: Option<(vk::Buffer, vk_mem::Allocation)>,
    /// The morph (blend-shape) buffers (`None` for a mesh without morph targets).
    morph: Option<MorphBuffers>,
    /// The meshlet buffers (`None` unless built with mesh-shader support).
    meshlets: Option<MeshletBuffers>,
    /// The watertight-conditioning buffers (`None` for an empty mesh).
    conditioning: Option<ConditioningBuffers>,
    /// Number of indices across every submesh.
    pub index_count: u32,
    /// Number of vertices.
    pub vertex_count: u32,
    /// The draw ranges over the shared vertex/index buffers.
    pub submeshes: Vec<Submesh>,
    /// Local-space AABB minimum (for ray picking).
    pub bounds_min: Vec3,
    /// Local-space AABB maximum (for ray picking).
    pub bounds_max: Vec3,
    /// CPU copy of the complete local/rest vertex stream for surface queries.
    pub cpu_vertices: Arc<[Vertex]>,
    /// CPU copy of the flat index buffer spanning every submesh.
    pub cpu_indices: Arc<[u32]>,
    /// CPU copy of the skin stream parallel to [`GpuMesh::cpu_vertices`] (empty
    /// when unskinned).
    pub cpu_skin: Vec<VertexSkin>,
    /// The ray-tracing BLAS (`None` when RT is unsupported or not yet built).
    pub blas: Option<Arc<AccelerationStructure>>,
    /// The per-mesh signed distance fields — one tight field per primitive (and per spatial
    /// chunk of an oversized primitive), empty when the mesh baked none (a degenerate mesh, or
    /// a build without SDF support). Held here so the fields live exactly as long as the mesh
    /// that owns them; the lighting cone-trace indexes each by [`GpuSdf::bindless_index`].
    pub sdfs: Vec<Arc<GpuSdf>>,
}

impl GpuMesh {
    /// Returns the host bytes retained for exact mesh-surface and deformation queries.
    pub fn retained_query_cpu_bytes(&self) -> u64 {
        fn bytes_for<T>(len: usize) -> u64 {
            u64::try_from(len)
                .unwrap_or(u64::MAX)
                .saturating_mul(size_of::<T>() as u64)
        }

        bytes_for::<Vertex>(self.cpu_vertices.len())
            .saturating_add(bytes_for::<u32>(self.cpu_indices.len()))
            .saturating_add(bytes_for::<VertexSkin>(self.cpu_skin.len()))
            .saturating_add(bytes_for::<Submesh>(self.submeshes.len()))
    }
}

/// The device-local meshlet buffers a [`GpuMesh`] carries when built with mesh-shader support: the
/// [`saffron_geometry::Meshlet`] descriptor array, the flat meshlet-vertex indices, and the packed
/// meshlet-triangle local indices (padded to `u32` for storage-buffer reads). The mesh shader binds
/// all three; the draw records one `cmd_draw_mesh_tasks` per submesh over [`MeshletBuffers::submesh_ranges`].
pub struct MeshletBuffers {
    /// The `Meshlet` descriptor array buffer + allocation (32 B stride).
    pub descriptors: (vk::Buffer, vk_mem::Allocation),
    /// The flat global vertex-index buffer + allocation (`u32` per entry).
    pub vertices: (vk::Buffer, vk_mem::Allocation),
    /// The packed local triangle-index buffer + allocation (`u32` per byte-triple group; see upload).
    pub triangles: (vk::Buffer, vk_mem::Allocation),
    /// `(first_meshlet, meshlet_count)` per submesh, parallel to [`GpuMesh::submeshes`].
    pub submesh_ranges: Vec<(u32, u32)>,
    /// Total meshlet count across all submeshes.
    pub meshlet_count: u32,
}

/// The device-local morph buffers a [`GpuMesh`] carries when it has blend shapes: the flat
/// `MorphDelta` array (28 B stride) and the per-target `{first_delta, delta_count}` ranges,
/// plus the counts the deform pass dispatches over.
pub struct MorphBuffers {
    /// The flat `MorphDelta` array buffer + allocation.
    pub deltas: (vk::Buffer, vk_mem::Allocation),
    /// The per-target range array buffer + allocation (`uint2` per target).
    pub ranges: (vk::Buffer, vk_mem::Allocation),
    /// CPU copy of the per-target `[first_delta, delta_count]` ranges, parallel to the GPU
    /// `ranges` buffer — the instancing pass reads these to compute each active target's
    /// scatter base + the total scatter dispatch size.
    pub cpu_ranges: Vec<[u32; 2]>,
    /// Number of morph targets.
    pub target_count: u32,
    /// Total `MorphDelta` records across all targets.
    pub delta_count: u32,
}

/// The device-local watertight-conditioning buffers a [`GpuMesh`] carries: the unique-edge list, the
/// per-triangle edge indices, the per-welded-vertex direction/tangent basis, and the base→welded map
/// ([`saffron_geometry::MeshConditioning`]). All four are plain `STORAGE_BUFFER`s (compute-read by the
/// Phase-3 factor pass + Phase-4 dicer); nothing binds them yet in Phase 2.
pub struct ConditioningBuffers {
    /// The `Edge` array buffer + allocation (16 B stride).
    pub edges: (vk::Buffer, vk_mem::Allocation),
    /// The `TriEdges` array buffer + allocation (16 B stride, one per triangle).
    pub tri_edges: (vk::Buffer, vk_mem::Allocation),
    /// The `WeldedVertex` array buffer + allocation (48 B stride).
    pub welded: (vk::Buffer, vk_mem::Allocation),
    /// The `weld_id` array buffer + allocation (`u32` per base vertex).
    pub weld_id: (vk::Buffer, vk_mem::Allocation),
    /// Number of unique edges.
    pub edge_count: u32,
    /// Number of welded vertices.
    pub welded_count: u32,
}

// SAFETY: the buffers/allocations carry no thread-affine state; the CPU-side
// vectors and `Arc<AccelerationStructure>` are `Send`. Meshes are shared as
// `Arc<GpuMesh>` and may be dropped from the worker thread.
unsafe impl Send for GpuMesh {}
// SAFETY: every field is shared read-only after construction (the raw buffers +
// `vk_mem::Allocation` carry no interior mutability and are mutated only through
// `&mut self`); the CPU vectors + `Arc<AccelerationStructure>` are `Sync`. The
// thumbnail worker hands an `Arc<GpuMesh>` back to the main thread through an
// `Arc<Mutex<_>>` (README §5 / the assets Ref-policy ledger), which needs `Sync`.
unsafe impl Sync for GpuMesh {}

/// The buffers and metadata a [`GpuMesh`] is assembled from (the upload path fills
/// this, then [`GpuMesh::from_parts`] takes ownership).
pub struct GpuMeshParts {
    /// The device-local vertex buffer + allocation.
    pub vertex: (vk::Buffer, vk_mem::Allocation),
    /// The device-local index buffer + allocation.
    pub index: (vk::Buffer, vk_mem::Allocation),
    /// The optional device-local skin stream buffer + allocation.
    pub skin: Option<(vk::Buffer, vk_mem::Allocation)>,
    /// The optional device-local morph buffers.
    pub morph: Option<MorphBuffers>,
    /// The optional device-local meshlet buffers (mesh-shader raster path).
    pub meshlets: Option<MeshletBuffers>,
    /// The optional device-local watertight-conditioning buffers.
    pub conditioning: Option<ConditioningBuffers>,
    /// Number of indices across every submesh.
    pub index_count: u32,
    /// Number of vertices.
    pub vertex_count: u32,
    /// The draw ranges.
    pub submeshes: Vec<Submesh>,
    /// Local-space AABB minimum.
    pub bounds_min: Vec3,
    /// Local-space AABB maximum.
    pub bounds_max: Vec3,
    /// Complete CPU vertex stream for surface queries.
    pub cpu_vertices: Vec<Vertex>,
    /// CPU indices for picking.
    pub cpu_indices: Vec<u32>,
    /// CPU skin stream for picking (empty when unskinned).
    pub cpu_skin: Vec<VertexSkin>,
    /// The built ray-tracing BLAS (`None` when RT is unsupported).
    pub blas: Option<Arc<AccelerationStructure>>,
    /// The uploaded per-mesh signed distance fields (one per primitive / chunk; empty when
    /// none was baked).
    pub sdfs: Vec<Arc<GpuSdf>>,
}

impl GpuMesh {
    /// Takes ownership of the uploaded buffers + metadata.
    pub fn from_parts(resources: &Arc<DeviceResources>, parts: GpuMeshParts) -> Self {
        Self {
            resources: Arc::clone(resources),
            vertex_buffer: parts.vertex.0,
            vertex_alloc: parts.vertex.1,
            index_buffer: parts.index.0,
            index_alloc: parts.index.1,
            skin: parts.skin,
            morph: parts.morph,
            meshlets: parts.meshlets,
            conditioning: parts.conditioning,
            index_count: parts.index_count,
            vertex_count: parts.vertex_count,
            submeshes: parts.submeshes,
            bounds_min: parts.bounds_min,
            bounds_max: parts.bounds_max,
            cpu_vertices: parts.cpu_vertices.into(),
            cpu_indices: parts.cpu_indices.into(),
            cpu_skin: parts.cpu_skin,
            blas: parts.blas,
            sdfs: parts.sdfs,
        }
    }

    /// The per-mesh signed distance fields — one tight field per primitive (and per spatial
    /// chunk of an oversized primitive); empty when the mesh baked none.
    pub fn sdfs(&self) -> &[Arc<GpuSdf>] {
        &self.sdfs
    }

    /// The vertex buffer handle.
    pub fn vertex_buffer(&self) -> vk::Buffer {
        self.vertex_buffer
    }

    /// The index buffer handle.
    pub fn index_buffer(&self) -> vk::Buffer {
        self.index_buffer
    }

    /// The skin stream buffer handle, or `None` for an unskinned mesh.
    pub fn skin_buffer(&self) -> Option<vk::Buffer> {
        self.skin.as_ref().map(|(buffer, _)| *buffer)
    }

    /// The morph buffers, or `None` for a mesh without blend shapes.
    pub fn morph(&self) -> Option<&MorphBuffers> {
        self.morph.as_ref()
    }

    /// The meshlet buffers, or `None` when the mesh was built without mesh-shader support.
    pub fn meshlets(&self) -> Option<&MeshletBuffers> {
        self.meshlets.as_ref()
    }

    /// The watertight-conditioning buffers, or `None` for an empty mesh.
    pub fn conditioning(&self) -> Option<&ConditioningBuffers> {
        self.conditioning.as_ref()
    }
}

impl Drop for GpuMesh {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The bundle keeps the allocator alive; each buffer is
        // destroyed exactly once. The `blas` Arc drops after this body.
        unsafe {
            let allocator = self.resources.allocator();
            allocator.destroy_buffer(self.vertex_buffer, &mut self.vertex_alloc);
            allocator.destroy_buffer(self.index_buffer, &mut self.index_alloc);
            if let Some((buffer, allocation)) = self.skin.as_mut() {
                allocator.destroy_buffer(*buffer, allocation);
            }
            if let Some(morph) = self.morph.as_mut() {
                allocator.destroy_buffer(morph.deltas.0, &mut morph.deltas.1);
                allocator.destroy_buffer(morph.ranges.0, &mut morph.ranges.1);
            }
            if let Some(meshlets) = self.meshlets.as_mut() {
                allocator.destroy_buffer(meshlets.descriptors.0, &mut meshlets.descriptors.1);
                allocator.destroy_buffer(meshlets.vertices.0, &mut meshlets.vertices.1);
                allocator.destroy_buffer(meshlets.triangles.0, &mut meshlets.triangles.1);
            }
            if let Some(c) = self.conditioning.as_mut() {
                allocator.destroy_buffer(c.edges.0, &mut c.edges.1);
                allocator.destroy_buffer(c.tri_edges.0, &mut c.tri_edges.1);
                allocator.destroy_buffer(c.welded.0, &mut c.welded.1);
                allocator.destroy_buffer(c.weld_id.0, &mut c.weld_id.1);
            }
        }
    }
}

/// A graphics or compute pipeline owning its `vk::Pipeline` + `vk::PipelineLayout`.
///
/// Owned by the renderer (never crosses to client code); [`Drop`] frees the pipeline
/// then the layout through the device.
pub struct Pipeline {
    resources: Arc<DeviceResources>,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
}

// SAFETY: the pipeline/layout handles carry no thread-affine state; cached PSOs are
// shared as `Arc<Pipeline>` across the upload + render threads.
unsafe impl Send for Pipeline {}

impl Pipeline {
    /// Wraps an already-created pipeline + its layout. The PSO-cache phase creates
    /// the pipeline (graphics or compute) and its layout, then hands them here.
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            pipeline,
            layout,
        }
    }

    /// The pipeline handle.
    pub fn handle(&self) -> vk::Pipeline {
        self.pipeline
    }

    /// The pipeline layout.
    pub fn layout(&self) -> vk::PipelineLayout {
        self.layout
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The bundle keeps the device alive; pipeline then
        // layout, in that order, each freed exactly once.
        unsafe {
            self.resources
                .device()
                .destroy_pipeline(self.pipeline, None);
            self.resources
                .device()
                .destroy_pipeline_layout(self.layout, None);
        }
    }
}

/// A ray-tracing acceleration structure (BLAS or TLAS): the `vk` handle, its device
/// address, and its backing device buffer.
///
/// It clones the ash `acceleration_structure::Device` (a cheap handle + fn-pointer
/// table) at construction so [`Drop`] is self-contained — no live dispatch needed.
/// [`Drop`] destroys the handle (through the cloned dispatch) then the backing buffer
/// (through the allocator).
pub struct AccelerationStructure {
    resources: Arc<DeviceResources>,
    dispatch: ash::khr::acceleration_structure::Device,
    handle: vk::AccelerationStructureKHR,
    buffer: vk::Buffer,
    allocation: vk_mem::Allocation,
    /// The device address (for TLAS instance references / shader binding).
    pub address: vk::DeviceAddress,
}

// SAFETY: the dispatch is a handle + fn-pointer table (Clone, no thread affinity);
// the buffer/allocation carry no thread-affine state. A BLAS is shared as
// `Arc<AccelerationStructure>` from `GpuMesh`, which may drop off the worker thread.
unsafe impl Send for AccelerationStructure {}
// SAFETY: every field is shared read-only after construction. A BLAS rides inside an
// `Arc<GpuMesh>` that the thumbnail worker hands back to the main thread through an
// `Arc<Mutex<_>>`, so `GpuMesh: Sync` requires `AccelerationStructure: Sync`.
unsafe impl Sync for AccelerationStructure {}

impl AccelerationStructure {
    /// Allocates an AS-storage backing buffer of `size`, creates the acceleration
    /// structure of `kind` over it, and queries its device address. The backing buffer
    /// carries `ACCELERATION_STRUCTURE_STORAGE | SHADER_DEVICE_ADDRESS` usage; the build is
    /// recorded separately by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if the storage buffer or the AS cannot be created.
    pub fn create(
        resources: &Arc<DeviceResources>,
        dispatch: &ash::khr::acceleration_structure::Device,
        size: vk::DeviceSize,
        kind: vk::AccelerationStructureTypeKHR,
    ) -> crate::Result<Self> {
        let buffer_info = vk::BufferCreateInfo::default().size(size).usage(
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        );
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-info is valid; the buffer + allocation are
        // owned and freed in `Drop` (or below on a create-AS failure).
        let (buffer, allocation) = checked_vma(
            unsafe {
                resources
                    .allocator()
                    .create_buffer(&buffer_info, &alloc_info)
            },
            "vmaCreateBuffer (accel storage)",
        )?;

        let create_info = vk::AccelerationStructureCreateInfoKHR::default()
            .buffer(buffer)
            .size(size)
            .ty(kind);
        // SAFETY: the ash seam. The backing buffer covers `size` with AS-storage usage;
        // the returned handle is destroyed in `Drop` through the cloned dispatch.
        let handle = match unsafe { dispatch.create_acceleration_structure(&create_info, None) } {
            Ok(handle) => handle,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the storage buffer before the early return.
                unsafe {
                    resources
                        .allocator()
                        .destroy_buffer(buffer, &mut allocation)
                };
                return Err(crate::Error::Vk {
                    context: "create_acceleration_structure",
                    result,
                });
            }
        };

        let address_info =
            vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(handle);
        // SAFETY: the ash seam. The handle was just created on this dispatch's device.
        let address = unsafe { dispatch.get_acceleration_structure_device_address(&address_info) };

        Ok(Self {
            resources: Arc::clone(resources),
            dispatch: dispatch.clone(),
            handle,
            buffer,
            allocation,
            address,
        })
    }

    /// The acceleration-structure handle.
    pub fn handle(&self) -> vk::AccelerationStructureKHR {
        self.handle
    }
}

impl Drop for AccelerationStructure {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps the device + allocator alive;
        // the cloned dispatch resolves `vkDestroyAccelerationStructureKHR`. The
        // handle is destroyed then the backing buffer, each exactly once.
        unsafe {
            self.dispatch
                .destroy_acceleration_structure(self.handle, None);
            self.resources
                .allocator()
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Device, SurfaceSource};
    use crate::validation_issue_count;

    /// Reads the VMA live allocation count — the precise leak probe. Unlike heap
    /// budgets (which need `VK_EXT_memory_budget`), `vmaCalculateStatistics` works on
    /// every device including llvmpipe, so the before/after assertion is reliable in
    /// the toolbox.
    fn live_allocations(device: &Device) -> u32 {
        device
            .allocator()
            .calculate_statistics()
            .expect("vmaCalculateStatistics")
            .total
            .statistics
            .allocationCount
    }

    /// Builds a headless device or skips the test (no Vulkan ICD in this toolbox).
    fn device_or_skip() -> Option<Device> {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Some(device),
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                None
            }
        }
    }

    /// Creates a 1×1 `R8G8B8A8_UNORM` `GpuTexture` occupying `slot`, returning its
    /// bindless slot to `free_list` on drop — the GpuTexture upload path's teardown,
    /// exercised without the full upload (image + view here, no staging copy).
    fn make_texture(device: &Device, free_list: &BindlessFreeList, slot: u32) -> GpuTexture {
        let resources = device.resources();
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the VMA seam. Freed when the returned GpuTexture drops.
        let (image, allocation) =
            unsafe { resources.allocator().create_image(&image_info, &alloc_info) }
                .expect("create_image");
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. Freed when the returned GpuTexture drops.
        let view = unsafe { resources.device().create_image_view(&view_info, None) }
            .expect("create_image_view");
        GpuTexture::from_parts(
            resources,
            GpuTextureParts {
                image,
                view,
                allocation,
                bindless_index: slot,
                extent: vk::Extent2D {
                    width: 1,
                    height: 1,
                },
                format: vk::Format::R8G8B8A8_UNORM,
                min_max: None,
            },
            free_list,
        )
    }

    /// Allocating then dropping each VMA-backed wrapper (`Buffer`, `Image`,
    /// `Image3D`, `GpuMesh`, `GpuTexture`) reclaims its allocation fully — the live
    /// VMA allocation count returns to the baseline after the drop, proving the
    /// `Drop` bodies free every handle (no leak). The phase's named no-leak gate.
    #[test]
    fn wrappers_drop_reclaims_every_allocation() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let resources = device.resources();
        let baseline = live_allocations(&device);

        // Buffer: one allocation, mapped for host writes.
        {
            let alloc_info = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferHost,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            };
            let mut buffer = Buffer::new(
                resources,
                256,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &alloc_info,
            )
            .expect("Buffer::new");
            assert_eq!(buffer.size(), 256);
            assert!(buffer.mapped_bytes().is_some(), "MAPPED buffer is mapped");
            assert!(
                live_allocations(&device) > baseline,
                "the buffer raised the live allocation count"
            );
        }
        assert_eq!(
            live_allocations(&device),
            baseline,
            "dropping the Buffer reclaimed its allocation"
        );

        // Image (2D color) + Image3D (GDF cascade volume): each owns image + view.
        {
            let _image = Image::new(
                resources,
                &ImageDesc::color_2d(
                    vk::Extent2D {
                        width: 8,
                        height: 8,
                    },
                    vk::Format::R8G8B8A8_UNORM,
                    vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                ),
            )
            .expect("Image::new");
            let _image3d = Image3D::new(
                resources,
                vk::Extent3D {
                    width: 4,
                    height: 4,
                    depth: 4,
                },
                vk::Format::R16G16B16A16_SFLOAT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            )
            .expect("Image3D::new");
            assert!(live_allocations(&device) >= baseline + 2);
        }
        assert_eq!(
            live_allocations(&device),
            baseline,
            "dropping Image + Image3D reclaimed both allocations"
        );

        // GpuMesh: two VMA buffers (vertex + index), no skin stream.
        {
            let make_buffer = |size: vk::DeviceSize, usage: vk::BufferUsageFlags| {
                let alloc_info = vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferDevice,
                    ..Default::default()
                };
                let info = vk::BufferCreateInfo::default().size(size).usage(usage);
                // SAFETY: the VMA seam. Ownership passes into the GpuMesh below.
                unsafe { resources.allocator().create_buffer(&info, &alloc_info) }
                    .expect("create_buffer")
            };
            let parts = GpuMeshParts {
                vertex: make_buffer(96, vk::BufferUsageFlags::VERTEX_BUFFER),
                index: make_buffer(48, vk::BufferUsageFlags::INDEX_BUFFER),
                skin: None,
                morph: None,
                meshlets: None,
                conditioning: None,
                index_count: 12,
                vertex_count: 3,
                submeshes: Vec::new(),
                bounds_min: Vec3::ZERO,
                bounds_max: Vec3::ONE,
                cpu_vertices: Vec::new(),
                cpu_indices: Vec::new(),
                cpu_skin: Vec::new(),
                blas: None,
                sdfs: Vec::new(),
            };
            let mesh = GpuMesh::from_parts(resources, parts);
            assert_eq!(mesh.index_count, 12);
            assert!(mesh.skin_buffer().is_none());
            assert!(live_allocations(&device) >= baseline + 2);
        }
        assert_eq!(
            live_allocations(&device),
            baseline,
            "dropping the GpuMesh reclaimed both buffers"
        );

        // GpuTexture: one image allocation; its slot returns to the free-list.
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let texture = make_texture(&device, &free_list, 7);
            assert_eq!(texture.bindless_index(), 7);
            assert!(live_allocations(&device) > baseline);
        }
        assert_eq!(
            live_allocations(&device),
            baseline,
            "dropping the GpuTexture reclaimed its image allocation"
        );

        device.wait_idle().expect("idle after the run");
    }

    /// A `GpuTexture` moved to a spawned thread and dropped there returns its
    /// bindless slot to the shared free-list under the mutex — the slot reappears in
    /// the list. Proves the `Arc<Mutex>` Drop path is `Send`-safe (README §5: a
    /// worker-uploaded texture may be destroyed off the main thread). The phase's
    /// named off-thread-reclaim gate.
    #[test]
    fn gpu_texture_dropped_off_thread_returns_its_slot() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let texture = make_texture(&device, &free_list, 42);

        // Move the texture into a worker thread and drop it there. `GpuTexture: Send`
        // is required for this to compile — the spawn closure takes ownership.
        let probe = Arc::clone(&free_list);
        std::thread::spawn(move || {
            drop(texture);
        })
        .join()
        .expect("worker thread joins");

        let slots = probe.lock().expect("free-list lock");
        assert_eq!(
            slots.as_slice(),
            &[42],
            "the off-thread drop returned slot 42 to the shared free-list"
        );
        drop(slots);
        device.wait_idle().expect("idle after the run");
    }

    /// Constructing the full resource set against a device and dropping it (then the
    /// device) is validation-clean — the teardown-order gate. The `Arc<DeviceResources>`
    /// keeps the allocator + device alive until the last resource drops, then the
    /// bundle frees the allocator before the device, the device before the instance.
    /// A wrong order would surface as a validation message (a handle freed under a
    /// live parent); the count must not move across the construct + drop.
    #[test]
    fn full_resource_set_teardown_is_validation_clean() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let resources = device.resources();

        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let buffer = Buffer::new(
            resources,
            512,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &alloc_info,
        )
        .expect("Buffer::new");
        let image = Image::new(
            resources,
            &ImageDesc::color_2d(
                vk::Extent2D {
                    width: 16,
                    height: 16,
                },
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE,
            ),
        )
        .expect("Image::new");
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let texture = make_texture(&device, &free_list, 0);

        // Drop every resource explicitly, then idle + drop the device. The bundle's
        // Arc held by `buffer`/`image`/`texture` releases here; the device + allocator
        // survive until `device` itself drops at the end of the function.
        drop(buffer);
        drop(image);
        drop(texture);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the full resource set's construct + teardown must be validation-clean \
             (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
