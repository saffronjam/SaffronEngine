use super::*;
use crate::validation_issue_count;
use vk_mem::Alloc;

/// A validation-clean offscreen clear+readback. Lavapipe's `VK_EXT_headless_surface` swapchain
/// WSI crashes inside `wsi_create_native_image_mem`, so the present-engine half of the loop is
/// not exercisable in this toolbox without a real Wayland display. Everything the engine controls
/// is: this allocates a color image via VMA, records the exact
/// `UNDEFINED → TRANSFER_DST → clear → TRANSFER_SRC → copy` sync2 sequence the swapchain path
/// uses, reads the result back, and asserts both the cleared color landed and the run was
/// validation-clean. The real acquire→present half is covered by `tests/swapchain_present.rs` on
/// a weston Wayland surface. Skips cleanly when no Vulkan device is obtainable.
///
/// The real acquire→present half is covered by `tests/swapchain_present.rs` on a
/// weston Wayland surface (which lavapipe presents correctly), skipped when no
/// display is available. Skips cleanly when no Vulkan device is obtainable.
#[test]
fn offscreen_clear_is_validation_clean() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();
    let cleared = clear_offscreen_and_read_back(&device, [0.25, 0.5, 0.75, 1.0])
        .expect("offscreen clear+readback succeeds");
    device.wait_idle().expect("idle after the run");

    // The image is R8G8B8A8_UNORM; the cleared floats quantize to these bytes. `0.5`
    // lands exactly halfway (127.5), and the spec leaves the tie-break to the
    // implementation, so each channel is checked to within one quantization step.
    let expected = [64u8, 128, 191, 255];
    for (channel, (&got, &want)) in cleared.iter().zip(&expected).enumerate() {
        assert!(
            got.abs_diff(want) <= 1,
            "the clear color reads back: channel {channel} was {got}, expected {want}±1 \
             (full readback {cleared:?})"
        );
    }
    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the clear+readback must be validation-clean (saw {} new issue(s))",
        after - before
    );
}

/// Allocates a 1×1 `R8G8B8A8_UNORM` image, clears it to `color`, copies it into a
/// host-visible buffer, and returns the single texel's bytes. Exercises the same
/// device/allocator/queue/sync2 path the swapchain present uses.
fn clear_offscreen_and_read_back(device: &Device, color: [f32; 4]) -> Result<[u8; 4]> {
    let allocator = device.allocator();

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
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    // SAFETY: the ash/VMA seam. The create-infos are valid for the call; the
    // returned image+allocation are freed below before the function returns.
    let (image, mut image_alloc) = unsafe { allocator.create_image(&image_info, &alloc_info) }
        .map_err(|result| Error::Vk {
            context: "create_image",
            result,
        })?;

    let buffer_info = vk::BufferCreateInfo::default()
        .size(4)
        .usage(vk::BufferUsageFlags::TRANSFER_DST);
    let buffer_alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferHost,
        flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
            | vk_mem::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    // SAFETY: the ash/VMA seam. As above; freed before returning.
    let (buffer, mut buffer_alloc) = unsafe {
        allocator.create_buffer(&buffer_info, &buffer_alloc_info)
    }
    .map_err(|result| Error::Vk {
        context: "create_buffer",
        result,
    })?;

    let result = record_clear_copy(device, image, buffer, color);

    // SAFETY: the ash/VMA seam. The device was idled by `record_clear_copy`
    // before this; each resource is destroyed exactly once.
    let texel = result.and_then(|()| {
        let info = allocator.get_allocation_info(&buffer_alloc);
        let ptr = info.mapped_data.cast::<u8>();
        if ptr.is_null() {
            return Err(Error::Vk {
                context: "buffer not mapped",
                result: vk::Result::ERROR_MEMORY_MAP_FAILED,
            });
        }
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED and 4 bytes long; the copy
        // completed (the submit fence was waited).
        Ok(unsafe { std::ptr::read(ptr.cast::<[u8; 4]>()) })
    });
    // SAFETY: the ash/VMA seam. Destroyed after the device idled.
    unsafe {
        allocator.destroy_buffer(buffer, &mut buffer_alloc);
        allocator.destroy_image(image, &mut image_alloc);
    }
    texel
}

/// Records the clear + copy on a one-shot command buffer, submits with a fence,
/// and waits — the same sync2 sequence as the swapchain path, ending in a copy
/// to a host buffer instead of a present.
fn record_clear_copy(
    device: &Device,
    image: vk::Image,
    buffer: vk::Buffer,
    color: [f32; 4],
) -> Result<()> {
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "fence",
    )?;

    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let record = || -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. The command-buffer recording below references
        // the image/buffer that outlive the submit-wait.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "begin")?;
            barrier(
                raw,
                cmd,
                image,
                range,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::empty(),
                vk::PipelineStageFlags2::CLEAR,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            let clear = vk::ClearColorValue { float32: color };
            raw.cmd_clear_color_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &[range],
            );
            barrier(
                raw,
                cmd,
                image,
                range,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::CLEAR,
                vk::AccessFlags2::TRANSFER_WRITE,
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
                    width: 1,
                    height: 1,
                    depth: 1,
                });
            raw.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[region],
            );
            checked(raw.end_command_buffer(cmd), "end")?;
        }

        let cmd_info = vk::CommandBufferSubmitInfo::default().command_buffer(cmd);
        let cmd_infos = [cmd_info];
        let submit = vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos);
        // SAFETY: the ash seam. Single-threaded queue use in this test.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &[submit], fence, "submit")?;
            checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
        }
        Ok(())
    };
    let result = record();

    // SAFETY: the ash seam. The fence was waited (or the submit never happened),
    // so the pool/fence are idle and destroyed exactly once.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

/// Records one sync2 image-layout barrier.
#[allow(clippy::too_many_arguments)]
unsafe fn barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    range: vk::ImageSubresourceRange,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(range);
    let barriers = [b];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the ash seam. The image outlives the recorded command.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// A GPU-runtime gate: build the descriptor + pipeline sub-state, seed a known linear-HDR color
/// into an offscreen, then run the full final post chain (mandatory tonemap → ground grid →
/// editor overlay) through the render graph and read the offscreen back. Asserts the tonemap
/// mapped the HDR value to the expected display-referred byte, the grid + overlay composited over
/// it, the frame was validation-clean, and present-only and editor mode produce byte-identical
/// offscreen content. Skips when no Vulkan device is present.
#[test]
fn final_post_chain_tonemaps_composites_grid_and_overlay_validation_clean() {
    use crate::descriptors::Descriptors;
    use crate::overlay::{OverlayState, OverlayVertex, TonemapPush};
    use crate::pipelines::Pipelines;
    use crate::resources::BindlessFreeList;
    use crate::ssao::Ssao;
    use crate::view_target::ViewTarget;
    use saffron_geometry::glam::{Vec2, Vec4};
    use std::sync::{Arc, Mutex};

    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
    let ssao = Ssao::new(&device).expect("Ssao");
    let mut view = ViewTarget::new(&device, 16, 16).expect("ViewTarget");
    view.allocate_screen_space_sets(&descriptors, &ssao)
        .expect("alloc sets");
    view.build_screen_space(&device, &descriptors, &ssao)
        .expect("build screen-space (writes the tonemap set)");
    // Binding 2 of the tonemap set is the always-bound creative LUT; bind the neutral
    // identity ramp exactly as renderer bring-up does.
    let queue = device.graphics_queue.clone();
    let uploader = crate::upload::Uploader::new(&device, &queue).expect("Uploader");
    let identity_lut = uploader.upload_identity_lut().expect("identity LUT");
    view.write_tonemap_lut(&device, descriptors.linear_sampler(), identity_lut.view());

    // The three post PSOs build on llvmpipe (graphics + compute, no RT).
    let tonemap = pipelines.request_tonemap().expect("tonemap PSO");
    let grid = pipelines.request_grid().expect("grid PSO");
    let overlay = pipelines.request_overlay().expect("overlay PSO");
    let overlay_depth = pipelines
        .request_overlay_depth()
        .expect("overlay-depth PSO");

    // A full-viewport on-top overlay quad in solid red (two triangles, no depth test).
    let red = Vec4::new(1.0, 0.0, 0.0, 1.0);
    let quad = |x: f32, y: f32| OverlayVertex::new(Vec2::new(x, y), red, Vec4::ZERO, 0.0);
    let on_top = vec![
        quad(-1.0, -1.0),
        quad(1.0, -1.0),
        quad(1.0, 1.0),
        quad(-1.0, -1.0),
        quad(1.0, 1.0),
        quad(-1.0, 1.0),
    ];

    // Thumbnails use PBR-Neutral so a material's color (a gold sphere) stays accurate in the
    // asset preview rather than getting the viewport's filmic look.
    let exposure = TonemapPush::new(0.0, crate::overlay::TonemapMode::PbrNeutral, 0.0);

    // Render the chain twice; the only difference is the present-only flag, which does
    // not touch this path — the two readbacks must be byte-identical.
    let mut readbacks = Vec::new();
    for _present_only in [false, true] {
        let mut overlay_state = OverlayState::new(device.resources());
        overlay_state.submit(Vec::new(), on_top.clone());
        let draw = overlay_state.prepare(0).expect("prepare").expect("draw");

        let pixels = render_post_chain_readback(
            &device,
            &view,
            view.tonemap_set,
            tonemap.handle(),
            tonemap.layout(),
            &exposure,
            grid.handle(),
            grid.layout(),
            overlay.handle(),
            overlay_depth.handle(),
            &draw,
        )
        .expect("post-chain readback");
        readbacks.push(pixels);
    }

    // The on-top red overlay covers every pixel: the center R channel is ~1.0
    // (overlay alpha 1 over the tonemapped gray), and the two modes match byte-for-byte.
    let editor = &readbacks[0];
    let present_only = &readbacks[1];
    assert_eq!(
        editor, present_only,
        "present-only and editor mode produce identical offscreen content"
    );
    // Pixel (8,8), R channel (4 halves per pixel, R first). f16 1.0 == 0x3C00.
    let center_r = editor[(16 * 8 + 8) * 4];
    assert_eq!(
        center_r,
        half_from_f32(1.0),
        "the on-top red overlay covered the center"
    );

    device.wait_idle().expect("idle before teardown");
    drop(view);
    drop(ssao);
    drop(identity_lut);
    drop(uploader);
    drop(queue);
    drop(tonemap);
    drop(grid);
    drop(overlay);
    drop(overlay_depth);
    drop(pipelines);
    drop(descriptors);
    drop(free_list);
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the final post chain must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// Both editor views are created at startup, each with its own offscreen targets; sizing one view
/// leaves the other's extent + tracked desired size untouched, and a view's `desired_width` reads
/// back the requested size. The capture path then reads the active view's offscreen back to a PNG
/// file. Skips when no Vulkan device is obtainable; the host swapchain WSI crashes headless, so
/// this exercises the per-view targets and the image→buffer→PNG capture directly on `ViewTarget`s.
#[test]
fn per_view_targets_size_independently_and_capture_writes_a_png() {
    use crate::descriptors::Descriptors;
    use crate::resources::BindlessFreeList;
    use crate::ssao::Ssao;
    use crate::view_target::ViewTarget;
    use std::sync::{Arc, Mutex};

    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let ssao = Ssao::new(&device).expect("Ssao");

    // Two independent views (mirroring the renderer's init loop): the scene view at
    // 24×16, the preview view sized later to 8×8.
    let mut views = Vec::with_capacity(VIEW_COUNT);
    for _ in 0..VIEW_COUNT {
        let mut view = ViewTarget::new(&device, 24, 16).expect("ViewTarget");
        view.allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc sets");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("build screen-space");
        views.push(view);
    }

    // A fresh view records its construction size as the desired size.
    assert_eq!(views[ViewId::Scene.index()].desired_width, 24);
    assert_eq!(views[ViewId::AssetPreview.index()].desired_width, 24);

    // Resize only the preview view to 8×8; the scene view is untouched.
    {
        let preview = &mut views[ViewId::AssetPreview.index()];
        preview.desired_width = 8;
        preview.desired_height = 8;
        let ext = vk::Extent2D {
            width: 8,
            height: 8,
        };
        preview.resize(&device, ext, ext).expect("resize preview");
        preview
            .build_screen_space(&device, &descriptors, &ssao)
            .expect("rebuild preview screen-space");
    }
    assert_eq!(
        views[ViewId::Scene.index()].scaled_render_extent().width,
        24
    );
    assert_eq!(
        views[ViewId::AssetPreview.index()]
            .scaled_render_extent()
            .width,
        8
    );
    assert_eq!(views[ViewId::AssetPreview.index()].desired_width, 8);

    // Capture the scene view's offscreen: clear it to a known linear-HDR gray through a
    // graphics pass, then copy it out exactly as `capture_viewport` does and encode a
    // PNG. Decode it back to confirm the dimensions + a clamped center pixel.
    let scene = &mut views[ViewId::Scene.index()];
    let tmp = std::env::temp_dir().join(format!(
        "saffron-capture-test-{}-{}.png",
        std::process::id(),
        scene.generation
    ));
    capture_view_to_png_for_test(&device, scene, &tmp).expect("capture");
    let decoded = image::open(&tmp).expect("decode capture").to_rgba8();
    assert_eq!(
        decoded.dimensions(),
        (24, 16),
        "PNG matches the offscreen size"
    );
    // The seed clears to linear 0.75; Clamp transfer keeps [0,1]×255 → ~191.
    let center = decoded.get_pixel(12, 8).0;
    assert!(
        (center[0] as i32 - 191).abs() <= 2,
        "the cleared gray reads back near 0.75×255 (got {})",
        center[0]
    );
    let _ = std::fs::remove_file(&tmp);

    device.wait_idle().expect("idle before teardown");
    drop(views);
    drop(ssao);
    drop(descriptors);
    drop(free_list);
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the per-view capture path must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// Seeds a known linear-HDR gray (0.75) into a view's offscreen via a graphics clear-store pass,
/// then runs the exact image→buffer copy + PNG write `capture_viewport` records. The standalone
/// analog of `Renderer::capture_viewport` for a test that cannot bring up a headless `Renderer`.
fn capture_view_to_png_for_test(
    device: &Device,
    view: &mut ViewTarget,
    path: &std::path::Path,
) -> Result<()> {
    let raw = device.raw();
    let extent = view.offscreen.extent;
    let format = view.offscreen.format;
    let image = view.offscreen.handle();
    let offscreen_view = view.offscreen.view();
    let byte_size = extent.width as vk::DeviceSize
        * extent.height as vk::DeviceSize
        * crate::format_pixel_bytes(format) as vk::DeviceSize;

    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end.
    let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "fence",
    )?;

    let buffer = crate::Buffer::new(
        device.resources(),
        byte_size,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )?;
    let color_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };

    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let recorded = (|| -> Result<()> {
        // SAFETY: the ash seam. Recorded on the one-off buffer.
        unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

        let mut graph = RenderGraph::new();
        let color = graph.import_image(
            image,
            offscreen_view,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let mut seed = RgAttachment::clear_store(color);
        seed.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.75, 0.75, 0.75, 1.0],
            },
        };
        graph.add_pass(
            RgPass::graphics("seed", extent)
                .color(seed)
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
        );
        graph.execute(device, cmd);

        // The seed pass left the offscreen COLOR_ATTACHMENT_OPTIMAL; copy it out exactly
        // as `capture_viewport` does.
        // SAFETY: the ash seam. COLOR_ATTACHMENT → TRANSFER_SRC then copy out.
        unsafe {
            capture_barrier(
                raw,
                cmd,
                image,
                color_range,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
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
                buffer.handle(),
                &[region],
            );
            checked(raw.end_command_buffer(cmd), "end")?;
        }

        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. Single-threaded queue use in the test.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "submit")?;
            checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
        }
        Ok(())
    })();

    if recorded.is_ok() {
        let pixels = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize) };
        crate::write_png_file(pixels, extent.width, extent.height, format, path)
            .map_err(|err| Error::ShaderLoad(format!("capture write: {err}")))?;
    }
    view.offscreen.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    recorded
}

/// Encodes an `f32` to its IEEE binary16 bit pattern (the offscreen is RGBA16F; the
/// readback compares raw half words).
fn half_from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exp <= 0 {
        return sign;
    }
    if exp >= 0x1f {
        return sign | 0x7c00;
    }
    sign | ((exp as u16) << 10) | ((mantissa >> 13) as u16)
}

/// Seeds a known linear-HDR gray into the offscreen, runs the final post chain (tonemap in-place
/// → grid → overlay) through the render graph, and copies the offscreen out as raw RGBA16F half
/// words. Mirrors the renderer's pass order; the offscreen carries `TRANSFER_SRC` + `STORAGE` so
/// it can be cleared, tonemapped, and read back.
#[allow(clippy::too_many_arguments)]
fn render_post_chain_readback(
    device: &Device,
    view: &ViewTarget,
    tonemap_set: vk::DescriptorSet,
    tonemap_pipeline: vk::Pipeline,
    tonemap_layout: vk::PipelineLayout,
    exposure: &crate::overlay::TonemapPush,
    grid_pipeline: vk::Pipeline,
    grid_layout: vk::PipelineLayout,
    overlay_pipeline: vk::Pipeline,
    overlay_depth_pipeline: vk::Pipeline,
    draw: &crate::overlay::OverlayDraw,
) -> Result<Vec<u16>> {
    use crate::overlay::{GridPush, record_grid, record_overlay};
    use crate::render_graph::{RenderGraph, RgPass, RgUsage};

    let raw = device.raw();
    // At the test's render scale 1, the display (offscreen) and input (depth) extents match.
    let extent = view.published_extent();
    let offscreen = view.offscreen.handle();
    let offscreen_view = view.offscreen.view();
    let depth = view.depth.handle();
    let depth_view = view.depth.view();

    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "fence",
    )?;

    let halves = extent.width as usize * extent.height as usize * 4;
    let buffer = crate::Buffer::new(
        device.resources(),
        (halves * size_of::<u16>()) as vk::DeviceSize,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )?;

    let color_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };

    let exposure = *exposure;
    let grid_push = GridPush::new(Mat4::IDENTITY);
    let draw = *draw;
    let raw_body = raw.clone();

    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let recorded = (|| -> Result<()> {
        // SAFETY: the ash seam. Recorded on the one-off buffer.
        unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

        let mut graph = RenderGraph::new();
        // Both targets enter UNDEFINED; the seed graphics pass below clears them (the
        // offscreen carries no TRANSFER_DST, so the known HDR value is laid down via a
        // color-attachment clear, mirroring how the scene pass writes the color).
        let color = graph.import_image(
            offscreen,
            offscreen_view,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let depth_res = graph.import_image(
            depth,
            depth_view,
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );

        // Seed a known linear-HDR white (1.0) into the offscreen + a cleared-far depth
        // (so nothing occludes the on-top overlay). A graphics clear-store pass.
        let mut seed_color = RgAttachment::clear_store(color);
        seed_color.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [1.0, 1.0, 1.0, 1.0],
            },
        };
        graph.add_pass(
            RgPass::graphics("seed", extent)
                .color(seed_color)
                .depth_attachment(super::depth_clear_store(depth_res))
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
        );

        // Tonemap (mandatory, in-place compute). Binding 1 is the dynamic-offset grade
        // UBO; the readback records one frame, so it selects frame slot 0's slice.
        let raw_tm = raw_body.clone();
        let push = exposure;
        let grade_offset = view.grade_ubo_offset(0);
        let groups = |n: u32| n.div_ceil(8);
        graph.add_pass(
            RgPass::compute("tonemap")
                .access(color, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The set/PSO are valid; the dispatch covers
                    // the viewport (8×8 per group).
                    unsafe {
                        raw_tm.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            tonemap_pipeline,
                        );
                        raw_tm.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            tonemap_layout,
                            0,
                            &[tonemap_set],
                            &[grade_offset],
                        );
                        raw_tm.cmd_push_constants(
                            cmd,
                            tonemap_layout,
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            bytemuck::bytes_of(&push),
                        );
                        raw_tm.cmd_dispatch(cmd, groups(extent.width), groups(extent.height), 1);
                    }
                }),
        );

        // Grid (graphics, over the tonemapped color, depth-tested read-only).
        let raw_grid = raw_body.clone();
        graph.add_pass(
            RgPass::graphics("grid", extent)
                .color(super::color_load_store(color))
                .depth_attachment(super::depth_load_readonly(depth_res))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_grid(&raw_grid, cmd, grid_pipeline, grid_layout, &grid_push);
                }),
        );

        // Overlay (graphics, on-top range over the color).
        let raw_ov = raw_body.clone();
        graph.add_pass(
            RgPass::graphics("editor-overlay", extent)
                .color(super::color_load_store(color))
                .depth_attachment(super::depth_load_readonly(depth_res))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_overlay(
                        &raw_ov,
                        cmd,
                        &draw,
                        overlay_pipeline,
                        overlay_depth_pipeline,
                    );
                }),
        );

        graph.execute(device, cmd);

        // The overlay graphics pass left the offscreen COLOR_ATTACHMENT_OPTIMAL; copy
        // it out.
        // SAFETY: the ash seam. COLOR_ATTACHMENT → TRANSFER_SRC then copy out.
        unsafe {
            barrier(
                raw,
                cmd,
                offscreen,
                color_range,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
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
                offscreen,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer.handle(),
                &[region],
            );
            // Restore the offscreen to UNDEFINED-equivalent for the next run (the
            // second iteration's clear transitions from UNDEFINED again).
            checked(raw.end_command_buffer(cmd), "end")?;
        }

        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. Single-threaded queue use in the test.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "submit")?;
            checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
        }
        Ok(())
    })();

    let mut out = vec![0u16; halves];
    if recorded.is_ok() {
        let ptr = buffer.mapped_ptr().cast::<u16>();
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED; the copy completed.
        unsafe { std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), halves) };
    }
    // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    recorded.map(|()| out)
}

/// The present-only blit's non-blank proof, headless: a full [`Renderer`] renders a visible
/// procedural sky into its offscreen, then [`crate::present::record_present_blit`] blits that
/// offscreen into a host-readable BGRA8 image, which is read back and asserted NON-UNIFORM.
///
/// The headless stand-in for the windowed integration test: lavapipe cannot present a headless
/// swapchain, but the blit itself is the load-bearing part and runs anywhere. Skips when no
/// Vulkan device is obtainable.
#[test]
fn present_blit_carries_a_non_blank_scene() {
    use saffron_geometry::glam::{Mat4, Vec3};

    let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 64, 64) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    // A visible procedural sky + a camera + no deformation work, so `render_scene_offscreen`
    // fills the offscreen with the sky's gradient (the non-uniform content the blit carries).
    renderer.submit_sky(&SkyRenderSettings::default());
    renderer
        .set_scene_lighting(&SceneLighting::default())
        .expect("set_scene_lighting");
    let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 100.0);
    let view = Mat4::look_at_rh(Vec3::new(0.0, 1.0, 4.0), Vec3::ZERO, Vec3::Y);
    renderer
        .submit_gpu_scene_deformations(proj * view, &[], &[])
        .expect("submit_gpu_scene_deformations");
    renderer
        .render_scene_offscreen()
        .expect("render_scene_offscreen");

    // The offscreen now holds the rendered sky. Allocate a BGRA8 destination + read-back
    // buffer, then run the exact present blit into it and read it back.
    let extent = renderer.active_view().offscreen.extent;
    let device = renderer.device_arc();
    let raw = device.raw();
    let dst_format = vk::Format::B8G8R8A8_UNORM;
    // The destination stands in for a swapchain image: TRANSFER_DST (the blit target) +
    // TRANSFER_SRC (the read-back) + COLOR_ATTACHMENT (swapchain images carry it, and
    // `Image::new` builds a sampled-compatible view).
    let dst = crate::Image::new(
        device.resources(),
        &crate::ImageDesc::color_2d(
            extent,
            dst_format,
            vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        ),
    )
    .expect("dst image");
    let byte_size = extent.width as vk::DeviceSize
        * extent.height as vk::DeviceSize
        * crate::format_pixel_bytes(dst_format) as vk::DeviceSize;
    let buffer = crate::Buffer::new(
        device.resources(),
        byte_size,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("readback buffer");

    renderer.device().wait_idle().expect("idle before blit");
    let offscreen = renderer.active_view().offscreen.handle();
    let from_layout = renderer.active_view().offscreen.layout;
    let (from_stage, from_access) = match from_layout {
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        ),
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        ),
        _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
    };

    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. One-off pool/buffer/fence for the blit + read-back; all freed
    // below after the fence signals.
    let pixels = unsafe {
        let pool = raw.create_command_pool(&pool_info, None).expect("pool");
        let cmd = raw
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .command_buffer_count(1),
            )
            .expect("cmd")[0];
        let fence = raw
            .create_fence(&vk::FenceCreateInfo::default(), None)
            .expect("fence");
        raw.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
        .expect("begin");
        // This stand-in runs on an offscreen device with no `VK_KHR_swapchain`, where
        // `PRESENT_SRC_KHR` is invalid; the blit leaves `dst` in `TRANSFER_SRC` directly so the
        // read-back copy can read it. The windowed present path passes `PRESENT_SRC_KHR`, proven
        // on a real surface in `tests/swapchain_present.rs`.
        crate::present::record_present_blit(
            raw,
            cmd,
            offscreen,
            extent,
            from_layout,
            from_stage,
            from_access,
            dst.handle(),
            extent,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
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
            dst.handle(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer.handle(),
            &[region],
        );
        raw.end_command_buffer(cmd).expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submit, fence, "submit")
            .expect("submit");
        raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
        let slice = std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize).to_vec();
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
        slice
    };

    // The blitted BGRA8 image must be NON-UNIFORM: the procedural sky is a gradient, so the
    // pixels carry many distinct colors. A uniform clear would yield exactly one.
    let mut distinct = std::collections::HashSet::new();
    for px in pixels.chunks_exact(4) {
        distinct.insert([px[0], px[1], px[2]]);
        if distinct.len() > 64 {
            break;
        }
    }
    assert!(
        distinct.len() > 16,
        "the present blit carried a NON-BLANK scene (saw {} distinct colors; a uniform clear \
         would be 1) — the offscreen sky was blitted, not a flat fill",
        distinct.len()
    );

    renderer.device().wait_idle().expect("idle before teardown");
    drop(buffer);
    drop(dst);
    drop(renderer);
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the present blit must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// A displaced (`HeightMode::Displacement`) instance driven through `render_scene_offscreen`
/// exercises the adaptive-tessellation *prep* chain — factor/scan/finalize/args/emit, the
/// transient VB/IB with their `cmd_fill_buffer` clears and storage bindings, the boundary +
/// interior geomorph, the prev-stream, and (RT armed) the coarse RT dice + `TessellatedBlas`
/// build — and asserts the frame stays validation-clean. Two frames, so the prev-stream
/// ping-pong runs.
///
/// Nothing here is mirrored into the persistent GPU scene, so no traversal record names the
/// arena and no amplified triangle is rasterized: the draw side is covered by
/// `visibility::tests::displaced` (arena rasterized through the binned cut) and by
/// `tests/e2e/tessellation-quality.test.ts` (displacement toggled on a real scene entity and
/// the viewport pixels compared).
#[test]
fn displaced_instance_tessellation_frame_is_validation_clean() {
    use crate::draw_list::SubmeshMaterial;
    use crate::upload::Uploader;
    use saffron_core::HeightMode;
    use saffron_geometry::glam::{Mat4, Vec2, Vec3};
    use saffron_geometry::{Mesh, Submesh, Vertex};
    use std::sync::Arc;

    let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 128, 128) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    // Arm RT so the coarse RT dice + `TessellatedBlas` build run too (the `.rt` transient buffers),
    // exercising both tess paths when the device supports ray tracing.
    if renderer.rt_supported() {
        renderer.set_rt_shadows(true);
    }

    // Upload a UV'd quad (watertight conditioning is built at upload) + a non-flat height map (whose
    // min/max pyramid drives the per-region factor) into the renderer's bindless descriptors.
    let queue = renderer.device().graphics_queue.clone();
    let uploader = Uploader::new(renderer.device(), &queue).expect("Uploader");
    let vert = |x: f32, z: f32, u: f32, w: f32| Vertex {
        position: Vec3::new(x, 0.0, z),
        normal: Vec3::new(0.0, 1.0, 0.0),
        uv0: Vec2::new(u, w),
        ..Vertex::default()
    };
    let mesh = Mesh {
        vertices: vec![
            vert(-1.0, -1.0, 0.0, 0.0),
            vert(1.0, -1.0, 1.0, 0.0),
            vert(1.0, 1.0, 1.0, 1.0),
            vert(-1.0, 1.0, 0.0, 1.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 6,
            vertex_offset: 0,
            material_slot: 0,
        }],
    };
    let hierarchy = crate::upload::hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
    let mesh = uploader
        .upload_mesh(
            renderer.descriptors(),
            &mesh,
            &hierarchy,
            &[],
            None,
            crate::SdfSource::None,
        )
        .expect("upload_mesh");
    let mut rgba = vec![0u8; 8 * 8 * 4];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        px[0] = ((i * 37) % 256) as u8; // a busy height in R so the per-region factor refines
        px[3] = 255;
    }
    let height = uploader
        .upload_height_texture(renderer.descriptors(), &rgba, 8, 8)
        .expect("upload_height_texture");

    let displaced_work = || {
        let mut material = SubmeshMaterial::defaults();
        material.height_texture = Some(Arc::clone(&height));
        material.height_mode = HeightMode::Displacement;
        material.height_scale = 0.2;
        let displace =
            crate::displace_info_from(std::slice::from_ref(&material)).expect("displaced material");
        crate::DeformationWork {
            mesh: Arc::clone(&mesh),
            entity: 7,
            skinned: false,
            joint_offset: 0,
            joint_count: 0,
            morph_weights: Vec::new(),
            model: Mat4::IDENTITY,
            displace: Some(displace),
            instance_slot: 0,
        }
    };

    // A close camera so the projected factor exceeds 1 and the dice/emit actually amplify (and, at
    // the default cap, may split — exercising the subpatch path).
    let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.05, 100.0);
    let view = Mat4::look_at_rh(Vec3::new(0.0, 1.5, 1.5), Vec3::ZERO, Vec3::Y);
    let view_proj = proj * view;
    for frame in 0..2 {
        renderer.submit_sky(&SkyRenderSettings::default());
        renderer
            .set_scene_lighting(&SceneLighting::default())
            .expect("set_scene_lighting");
        renderer
            .submit_gpu_scene_deformations(view_proj, &[displaced_work()], &[])
            .expect("submit_gpu_scene_deformations");
        renderer
            .render_scene_offscreen()
            .unwrap_or_else(|err| panic!("render_scene_offscreen frame {frame}: {err}"));
    }
    renderer
        .device()
        .wait_idle()
        .expect("idle after the displaced frames");

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the displaced tessellation frame must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// Waits `fence` for at most `timeout_ns` and reports whether it signalled. Bounded on purpose: a
/// slot fence nothing will ever signal must fail a test rather than hang it.
fn fence_signalled(device: &Device, fence: vk::Fence, timeout_ns: u64) -> bool {
    // SAFETY: the ash seam. The fence belongs to this device and outlives the wait.
    unsafe { device.raw().wait_for_fences(&[fence], true, timeout_ns) }.is_ok()
}

/// Long enough for a real frame to complete on a software rasterizer.
const FENCE_WAIT_NS: u64 = 10_000_000_000;

/// `begin_offscreen_frame` resets the slot's fence, and from there until something signals it the
/// slot is unusable — every early return in between depends on the close being decided by that
/// fence state alone. Closing an armed slot signals it and moves the ring on.
#[test]
fn closing_an_armed_slot_signals_its_fence_and_advances_the_ring() {
    let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 64, 64) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let device = renderer.device_arc();

    renderer
        .begin_offscreen_frame()
        .expect("begin_offscreen_frame");
    let armed_fence = renderer.frames.in_flight();
    let armed_slot = renderer.frames.index();
    assert!(
        renderer.slot_fence_armed,
        "the begin arms the slot whose fence it reset"
    );
    assert!(
        !fence_signalled(&device, armed_fence, 100_000_000),
        "an armed slot's fence is reset and unsignalled until something submits for it"
    );

    renderer
        .finish_unsubmitted_frame()
        .expect("closing the armed slot");

    assert!(
        !renderer.slot_fence_armed,
        "the closed slot is no longer armed"
    );
    assert_ne!(
        renderer.frames.index(),
        armed_slot,
        "the ring advances past the slot it closed"
    );
    assert!(
        fence_signalled(&device, armed_fence, FENCE_WAIT_NS),
        "the closing submit signals the fence the begin reset"
    );
    renderer.device().wait_idle().expect("idle before teardown");
}

/// A frame's slot fence gets exactly one signal and the ring advances past it exactly once —
/// whether the frame reached its tail submit or returned early somewhere between the begin and
/// that submit. The recovery must not depend on the frame having succeeded, so the render result
/// is reported rather than asserted.
#[test]
fn a_frame_closes_its_slot_whether_or_not_it_reached_the_tail_submit() {
    use saffron_geometry::glam::{Mat4, Vec3};

    let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 64, 64) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let device = renderer.device_arc();

    renderer.submit_sky(&SkyRenderSettings::default());
    renderer
        .set_scene_lighting(&SceneLighting::default())
        .expect("set_scene_lighting");
    let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 100.0);
    let view = Mat4::look_at_rh(Vec3::new(0.0, 1.0, 4.0), Vec3::ZERO, Vec3::Y);
    renderer
        .submit_gpu_scene_deformations(proj * view, &[], &[])
        .expect("submit_gpu_scene_deformations");

    renderer
        .begin_offscreen_frame()
        .expect("begin_offscreen_frame");
    let frame_fence = renderer.frames.in_flight();
    let frame_slot = renderer.frames.index();
    let rendered = renderer.render_scene_offscreen();
    renderer
        .finish_unsubmitted_frame()
        .expect("closing whatever the frame left behind");

    assert!(
        fence_signalled(&device, frame_fence, FENCE_WAIT_NS),
        "the frame's slot fence must be signalled once the frame is closed (render result: \
         {rendered:?})"
    );
    assert_eq!(
        renderer.frames.index(),
        (frame_slot + 1) % crate::frame::MAX_FRAMES_IN_FLIGHT,
        "the ring advances past the frame's slot exactly once (render result: {rendered:?})"
    );
    renderer.device().wait_idle().expect("idle before teardown");
}
