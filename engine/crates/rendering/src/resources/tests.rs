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
            mip_count: 1,
            min_max: None,
        },
        free_list,
    )
}

/// Allocating then dropping each VMA-backed wrapper reclaims its allocation fully: the live
/// VMA allocation count returns to the baseline after the drop.
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
            submesh_opaque: vec![true],
            micromaps: Vec::new(),
            vertex: make_buffer(96, vk::BufferUsageFlags::VERTEX_BUFFER),
            index: make_buffer(48, vk::BufferUsageFlags::INDEX_BUFFER),
            skin: None,
            morph: None,
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
            assembly_blas: Vec::new(),
            aggregate_blas: None,
            sdfs: Vec::new(),
            hierarchy_pages: Vec::new(),
            assembly: None,
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

/// A `GpuTexture` moved to a spawned thread and dropped there returns its bindless slot to the
/// shared free-list under the mutex, so a worker-uploaded texture can be destroyed off the main
/// thread.
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

/// A heap the reported budgets are folded over.
fn heap(size: vk::DeviceSize, device_local: bool) -> vk::MemoryHeap {
    vk::MemoryHeap {
        size,
        flags: if device_local {
            vk::MemoryHeapFlags::DEVICE_LOCAL
        } else {
            vk::MemoryHeapFlags::empty()
        },
    }
}

#[test]
fn vram_folds_only_the_device_local_heaps() {
    // A discrete adapter as the driver describes it: video memory, a host-visible upload window
    // carved out of it, and system memory the driver can also allocate from.
    let heaps = [
        heap(8 * 1024 * 1024 * 1024, true),
        heap(256 * 1024 * 1024, true),
        heap(16 * 1024 * 1024 * 1024, false),
    ];
    let budgets = [
        (1_500_000_000, 7_600_000_000),
        (40_000_000, 250_000_000),
        (900_000_000, 15_000_000_000),
    ];
    let vram = device_local_vram(&heaps, budgets);
    assert_eq!(vram.usage_bytes, 1_540_000_000);
    assert_eq!(vram.budget_bytes, 7_850_000_000);
}

#[test]
fn vram_uses_heap_size_when_reported_budget_is_implausible() {
    let heaps = [
        heap(8 * 1024 * 1024 * 1024, true),
        heap(16 * 1024 * 1024 * 1024, false),
    ];
    let vram = device_local_vram(&heaps, [(107 * 1024 * 1024, 108 * 1024 * 1024), (0, 0)]);
    assert_eq!(vram.usage_bytes, 107 * 1024 * 1024);
    assert_eq!(vram.budget_bytes, 8 * 1024 * 1024 * 1024);
}

#[test]
fn vram_is_zero_when_no_heap_is_device_local() {
    let heaps = [heap(4 * 1024 * 1024 * 1024, false)];
    let vram = device_local_vram(&heaps, [(123_456, 789_012)]);
    assert_eq!(vram, VramUsage::default());
}
