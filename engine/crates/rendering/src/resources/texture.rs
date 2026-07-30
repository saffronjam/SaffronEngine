//! The sampled-resource wrappers: bindless albedo textures, creative-look LUTs, and the
//! per-mesh signed-distance-field bricks. Each returns its bindless slot to the shared free-list
//! on drop, which the thumbnail worker relies on when it drops a texture off the main thread.

use super::*;

/// A device-local sampled texture (image + view) that also owns a bindless slot.
///
/// [`Drop`] returns the bindless slot to the shared free-list under the mutex (so a
/// worker-uploaded texture
/// destroyed off the main thread is safe), then frees the view and
/// image. The sampler is shared (the renderer's linear sampler), so it is not owned
/// here. A displacement height map additionally owns its [`MinMaxPyramid`] (freed here).
pub struct GpuTexture {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
    pub(super) bindless_index: u32,
    pub(super) free_list: Option<BindlessFreeList>,
    /// The per-height min/max pyramid, present only for a displacement height map.
    pub(super) min_max: Option<MinMaxPyramid>,
    /// The texture extent.
    pub extent: vk::Extent2D,
    /// The texture format.
    pub format: vk::Format,
    /// The uploaded mip-level count.
    pub mip_count: u32,
}

// SAFETY: the free-list is `Arc<Mutex<_>>` (Send+Sync); the image/view/allocation
// carry no thread-affine state. A `GpuTexture` is moved to a worker thread and
// dropped there — the bindless-slot-return path is exactly why this must be `Send`.
unsafe impl Send for GpuTexture {}

// SAFETY: every field is shared read-only after construction (the raw image/view +
// `vk_mem::Allocation` carry no interior mutability and are mutated only through
// `&mut self`); the free-list is `Arc<Mutex<_>>`. The thumbnail worker's handback
// hands an `Arc<GpuTexture>` back to the main thread through an `Arc<Mutex<_>>`
//, which requires `GpuTexture: Sync`.
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
    /// The uploaded mip-level count.
    pub mip_count: u32,
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
            mip_count: parts.mip_count,
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
    pub(super) resources: Arc<DeviceResources>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) allocation: vk_mem::Allocation,
    /// The table resolution per axis (`2` identity, `17`/`33`/`65` imported, `33` baked).
    pub(super) size: u32,
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
        // image, each freed exactly once. Idled before teardown.
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
    pub(super) resources: Arc<DeviceResources>,
    pub(super) atlas_image: vk::Image,
    pub(super) atlas_view: vk::ImageView,
    pub(super) atlas_alloc: vk_mem::Allocation,
    pub(super) indirection_image: vk::Image,
    pub(super) indirection_view: vk::ImageView,
    pub(super) indirection_alloc: vk_mem::Allocation,
    pub(super) coverage_image: vk::Image,
    pub(super) coverage_view: vk::ImageView,
    pub(super) coverage_alloc: vk_mem::Allocation,
    pub(super) bindless_index: u32,
    pub(super) free_list: Option<BindlessFreeList>,
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
    /// The field's own aggregate occupancy in unorm16 (`0` = resolve from the drawn
    /// material), from the cooked header.
    pub occupancy_unorm: u32,
    /// The field's own proxy albedo, rgb 8:8:8 unorm packed (`0` = resolve from the
    /// drawn material), from the cooked header.
    pub proxy_albedo: u32,
}

// SAFETY: as [`GpuTexture`] — the free-list is `Arc<Mutex<_>>` (Send+Sync); the
// image/view/allocation carry no thread-affine state. A `GpuSdf` rides inside an
// `Arc<GpuMesh>` the worker may build + drop off the main thread.
unsafe impl Send for GpuSdf {}

// SAFETY: every field is shared read-only after construction; the free-list is
// `Arc<Mutex<_>>`. The thumbnail worker hands an `Arc<GpuMesh>` (holding the `GpuSdf`)
// back to the main thread through an `Arc<Mutex<_>>`, which needs `Sync`.
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
    /// The field's cooked aggregate occupancy in unorm16 (`0` = resolve from material).
    pub occupancy_unorm: u32,
    /// The field's cooked proxy albedo, packed 8:8:8 (`0` = resolve from material).
    pub proxy_albedo: u32,
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
            occupancy_unorm: parts.occupancy_unorm,
            proxy_albedo: parts.proxy_albedo,
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
