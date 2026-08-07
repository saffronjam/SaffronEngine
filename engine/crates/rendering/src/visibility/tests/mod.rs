mod cull;
mod displaced;
mod executor;
mod transparent;
mod traversal;

use super::*;
use crate::descriptors::Descriptors;
use crate::global_gpu_data::{GlobalGpuData, GpuHandle};
use crate::gpu_scene_upload::GpuSceneUploader;
use crate::nested_scopes::NestedScopeRecorder;
use crate::persistent_gpu_scene::GpuSceneWorldId;
use crate::persistent_gpu_scene::{
    GpuSceneDynamicTransform, GpuSceneMaterialRecord, GpuScenePageRecord, GpuSceneSharedDelta,
    GpuSceneSharedDeltaResult, GpuSceneTransform, GpuSceneUploadLimits, GpuSceneWorldDelta,
    GpuSceneWorldDeltaResult, PersistentGpuScene,
};
use crate::render_graph::{RenderGraph, RgPass, RgUsage};
use crate::resources::Buffer;
use crate::resources::{BindlessFreeList, Image, ImageDesc};
use crate::{
    Device, GpuSceneInstanceRecord, GpuScenePrototypeRecord, Pipelines, SurfaceSource,
    validation_issue_count,
};
use saffron_geometry::glam::{Mat4, Vec3};
use std::sync::Mutex;

const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

/// The offscreen device every visibility test rasterizes through. An unobtainable device
/// fails the test rather than skipping it: `tools/gpu-driver.sh` points the loader at a
/// driver on every supported host, and a skip would let a file full of GPU assertions pass
/// without issuing one draw.
///
/// `create_device` is the one step that fails from pressure rather than absence — every GPU
/// test in this crate holds a live `VkDevice` for its whole body and a driver refuses new
/// ones past its concurrent limit — so that failure alone is retried while the suite drains.
fn offscreen_device() -> Device {
    const ATTEMPTS: u32 = 60;
    let mut last = None;
    for attempt in 1..=ATTEMPTS {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => return device,
            Err(err) => {
                let contended = matches!(
                    err,
                    crate::Error::Vk {
                        context: "create_device",
                        ..
                    }
                );
                last = Some(err);
                if !contended {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    100 * u64::from(attempt).min(10),
                ));
            }
        }
    }
    panic!(
        "offscreen Vulkan device: {}",
        last.expect("one attempt at least")
    )
}

/// A prototype whose only meaningful field is its unit conservative bounds sphere — enough for
/// the cull, which reads bounds and nothing else. Its geometry, material, and root page are
/// placeholder handles no cull path dereferences.
fn unit_bounds_prototype(gpu_scene: &mut PersistentGpuScene) -> crate::GpuScenePrototypeHandle {
    let material = match gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
            GpuSceneMaterialRecord {
                table: GpuHandle {
                    index: 1,
                    generation: 1,
                },
                source_revision: 1,
            },
        ))
        .expect("material")
    {
        GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
        other => panic!("unexpected {other:?}"),
    };
    let page = match gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
            table: GpuHandle {
                index: 2,
                generation: 1,
            },
            parent: None,
            source_generation: 1,
            flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
        }))
        .expect("page")
    {
        GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
        other => panic!("unexpected {other:?}"),
    };
    match gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
            GpuScenePrototypeRecord {
                geometry: GpuHandle {
                    index: 3,
                    generation: 1,
                },
                materials: std::sync::Arc::from([material]),
                deformation: None,
                sdfs: Vec::new().into(),
                root_page: page,
                page_bounds: std::sync::Arc::from([]),
                bounds: [0.0, 0.0, 0.0, 1.0],
                source_generation: 1,
                flags: 0,
                mechanics: [0; 4],
            },
        ))
        .expect("prototype")
    {
        GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
        other => panic!("unexpected {other:?}"),
    }
}

/// A slot-sorted displaced-row table holding `rows`, addressable by the cull and the traversal.
fn displaced_rows_buffer(device: &Device, rows: &[crate::DisplacedRow]) -> Buffer {
    let buffer = Buffer::new(
        device.resources(),
        (rows.len().max(1) * size_of::<crate::DisplacedRow>()) as u64,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("displaced rows buffer");
    // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytemuck::cast_slice::<_, u8>(rows).as_ptr(),
            buffer.mapped_ptr(),
            std::mem::size_of_val(rows),
        );
    }
    buffer
}

/// A wind sway record buffer sized for the tests' 64-slot views, seeded from `records`
/// (slot order) and zero beyond them. The deformation prepass is what fills this in
/// production; a test that wants a wind-deformed instance stands in for it here.
fn wind_records_buffer(device: &Device, records: &[crate::GpuWindInstanceRecord]) -> Buffer {
    let slots = 64;
    assert!(
        records.len() <= slots,
        "the tests' views hold {slots} slots"
    );
    let buffer = Buffer::new(
        device.resources(),
        slots as u64 * size_of::<crate::GpuWindInstanceRecord>() as u64,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("wind records buffer");
    let mut seeded = vec![crate::GpuWindInstanceRecord::default(); slots];
    seeded[..records.len()].copy_from_slice(records);
    // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytemuck::cast_slice::<_, u8>(&seeded).as_ptr(),
            buffer.mapped_ptr(),
            std::mem::size_of_val(seeded.as_slice()),
        );
    }
    buffer
}

/// The instance set (set 2) as the renderer writes it: the material-parameter arena, the
/// frame's GPU-scene address block, and the view's record + command streams. Every
/// production raster pass reaches the executor's records through this set, so a test that
/// draws the cut binds the same one.
fn production_instance_set(
    descriptors: &Descriptors,
    gpu_data: &GlobalGpuData,
    view: &SceneVisibilityView,
    frame: usize,
    address: (vk::Buffer, u64, u64),
) -> vk::DescriptorSet {
    let set = descriptors
        .allocate_set(descriptors.instance_set_layout())
        .expect("instance set");
    descriptors.write_storage_buffer(
        set,
        2,
        gpu_data.material_parameters.buffer(),
        vk::WHOLE_SIZE,
    );
    descriptors.write_uniform_buffer_at(set, 3, address.0, address.1, address.2);
    descriptors.write_storage_buffer(
        set,
        4,
        view.records(frame),
        u64::from(view.record_capacity()) * size_of::<crate::GpuDrawRecord>() as u64,
    );
    descriptors.write_storage_buffer(
        set,
        5,
        view.commands(frame),
        u64::from(view.command_slots()) * 20,
    );
    set
}

/// The frame's binned cut drawn through the production depth pre-pass: the übershader's
/// `vertexMainExecutor` over `depthPrepassFragment`, sets 0 + 2, and one counted-indirect
/// draw per bucket over the index stream that bucket's representation reads — the same
/// recorder the depth, shadow, G-buffer, and motion passes use.
#[allow(clippy::too_many_arguments)]
fn add_depth_prepass(
    device: &Device,
    graph: &mut RenderGraph,
    name: &'static str,
    prepass: &Arc<crate::Pipeline>,
    target: &Image,
    sets: (vk::DescriptorSet, vk::DescriptorSet),
    view_proj: Mat4,
    inputs: crate::ExecutorDrawInputs,
    pages_buffer: vk::Buffer,
    buckets: &[ExecutorBucket],
) {
    use crate::render_graph::RgAttachment;

    let depth_res = graph.import_image(
        target.handle(),
        target.view(),
        vk::ImageAspectFlags::DEPTH,
        vk::ImageLayout::UNDEFINED,
        None,
    );
    let pages_res = graph.import_buffer(pages_buffer, None);
    let commands_res = graph.import_buffer(inputs.commands, None);
    let counters_res = graph.import_buffer(inputs.counters, None);
    let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
    let raw = device.raw().clone();
    let handle = prepass.handle();
    let layout = prepass.layout();
    let draw_indirect_count = device.capabilities.draw_indirect_count;
    let draws: Vec<(ExecutorBucket, bool, Arc<crate::Pipeline>)> = buckets
        .iter()
        .map(|bucket| (*bucket, false, Arc::clone(prepass)))
        .collect();
    graph.add_pass(
        RgPass::graphics(
            name,
            vk::Extent2D {
                width: 64,
                height: 64,
            },
        )
        .depth_attachment(RgAttachment {
            resource: depth_res,
            load_op: vk::AttachmentLoadOp::CLEAR,
            store_op: vk::AttachmentStoreOp::STORE,
            clear_value: vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
            resolve: None,
        })
        .access(pages_res, RgUsage::IndexInputRead)
        .access(commands_res, RgUsage::IndirectCommandRead)
        .access(counters_res, RgUsage::IndirectCountRead)
        .access(bucket_counts_res, RgUsage::IndirectCountRead)
        .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            crate::record_executor_depth_family(
                &raw,
                cmd,
                (handle, layout),
                vk::ShaderStageFlags::VERTEX,
                bytemuck::bytes_of(&view_proj),
                sets.0,
                sets.1,
                inputs,
                pages_buffer,
                draw_indirect_count,
                &draws,
                false,
            );
        }),
    );
}

fn one_shot<F: FnOnce(vk::CommandBuffer)>(device: &Device, record: F) {
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Everything is destroyed after the fence wait.
    unsafe {
        let pool = raw.create_command_pool(&pool_info, None).expect("pool");
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = raw.allocate_command_buffers(&alloc).expect("cmd")[0];
        let fence = raw
            .create_fence(&vk::FenceCreateInfo::default(), None)
            .expect("fence");
        raw.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
        .expect("begin");
        record(cmd);
        raw.end_command_buffer(cmd).expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submit, fence, "visibility test")
            .expect("submit");
        raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
}

fn read_words(device: &Device, buffer: vk::Buffer, words: usize) -> Vec<u32> {
    let staging = Buffer::new(
        device.resources(),
        (words * 4) as u64,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("staging");
    let raw = device.raw().clone();
    one_shot(device, |cmd| {
        let barrier = vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
        let barriers = [barrier];
        // SAFETY: the ash seam. One-off copy under the fence below.
        unsafe {
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&barriers),
            );
            raw.cmd_copy_buffer(
                cmd,
                buffer,
                staging.handle(),
                &[vk::BufferCopy {
                    src_offset: 0,
                    dst_offset: 0,
                    size: (words * 4) as u64,
                }],
            );
        }
    });
    let mut out = vec![0_u8; words * 4];
    // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
    unsafe {
        std::ptr::copy_nonoverlapping(staging.mapped_ptr(), out.as_mut_ptr(), out.len());
    }
    out.chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

fn clear_image(device: &Device, image: vk::Image, layout_from_undefined: bool, value: f32) {
    let raw = device.raw().clone();
    one_shot(device, |cmd| {
        let old_layout = if layout_from_undefined {
            vk::ImageLayout::UNDEFINED
        } else {
            vk::ImageLayout::GENERAL
        };
        let barrier = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
            .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers = [barrier];
        // SAFETY: the ash seam. One-off clear under the fence below.
        unsafe {
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            raw.cmd_clear_color_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: [value, value, value, value],
                },
                &[vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: 1,
                }],
            );
            let back = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let backs = [back];
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&backs),
            );
        }
    });
}

/// A 64x64 `D32_SFLOAT` attachment the executor rasterizes into, readable back through
/// [`count_written_depth`].
fn depth_target(device: &Device) -> Image {
    Image::new(
        device.resources(),
        &ImageDesc {
            extent: vk::Extent2D {
                width: 64,
                height: 64,
            },
            format: vk::Format::D32_SFLOAT,
            usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC,
            aspect: vk::ImageAspectFlags::DEPTH,
            view_type: vk::ImageViewType::TYPE_2D,
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        },
    )
    .expect("depth target")
}

/// Depth texels a 64x64 target holds that are nearer than the clear — the "what did it
/// rasterize" measure the executor tests are scored by.
fn count_written_depth(device: &Device, target: &Image) -> usize {
    let staging = Buffer::new(
        device.resources(),
        64 * 64 * 4,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("staging");
    let raw = device.raw().clone();
    one_shot(device, |cmd| {
        let barrier = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
            .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .old_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .image(target.handle())
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::DEPTH,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers = [barrier];
        // SAFETY: the ash seam. One-off readback under the fence below.
        unsafe {
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            raw.cmd_copy_image_to_buffer(
                cmd,
                target.handle(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging.handle(),
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: 64,
                        height: 64,
                        depth: 1,
                    },
                }],
            );
        }
    });
    let mut depth_bytes = vec![0_u8; 64 * 64 * 4];
    // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
    unsafe {
        std::ptr::copy_nonoverlapping(
            staging.mapped_ptr(),
            depth_bytes.as_mut_ptr(),
            depth_bytes.len(),
        );
    }
    let written = depth_bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .filter(|depth| *depth < 0.999)
        .count();
    device.wait_idle().expect("idle after readback");
    written
}

fn hzb_image(device: &Device, value: f32) -> Image {
    let image = Image::new(
        device.resources(),
        &ImageDesc {
            extent: vk::Extent2D {
                width: 64,
                height: 64,
            },
            format: vk::Format::R32_SFLOAT,
            usage: vk::ImageUsageFlags::STORAGE
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_DST,
            aspect: vk::ImageAspectFlags::COLOR,
            view_type: vk::ImageViewType::TYPE_2D,
            mip_levels: 7,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        },
    )
    .expect("hzb image");
    clear_image(device, image.handle(), true, value);
    image
}

fn create_instance(
    gpu_scene: &mut PersistentGpuScene,
    prototype: crate::GpuScenePrototypeHandle,
    translation: Vec3,
    flags: u32,
) -> crate::GpuSceneInstanceHandle {
    let transform =
        GpuSceneDynamicTransform::new(Mat4::from_translation(translation), Mat4::IDENTITY)
            .expect("transform");
    match gpu_scene
        .apply_world_delta(
            WORLD,
            GpuSceneWorldDelta::CreateInstance(GpuSceneInstanceRecord {
                prototype,
                transform: GpuSceneTransform::Dynamic(transform),
                material_overrides: std::sync::Arc::from([]),
                deformation: None,
                source_generation: 1,
                flags,
                combination: 0,
                vegetation: None,
            }),
        )
        .expect("instance")
    {
        GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
        other => panic!("unexpected {other:?}"),
    }
}

/// One instance of one cooked hierarchy, seen from one camera, with whatever wind record the
/// deformation prepass would have written for it.
struct ClusterWalk {
    hierarchy: saffron_geometry::PortableVirtualHierarchy,
    /// Conservative prototype sphere. Spanning every cluster keeps the instance-level cull out
    /// of the way, so a counter difference can only have come from the node/cluster tests.
    prototype_bounds: [f32; 4],
    instance_flags: u32,
    wind_record: crate::GpuWindInstanceRecord,
    eye: Vec3,
    target: Vec3,
}

/// Publishes `walk`'s hierarchy as one guaranteed-root page, creates one instance of it, and
/// runs the cull plus traversal once per `node_cull` setting, returning each walk's counter
/// words. Every walk shares the frame, so a difference between them is the setting alone.
fn cluster_walk_counters(walk: &ClusterWalk, node_cull: &[u32]) -> Vec<Vec<u32>> {
    use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
    use crate::page_residency::{PageResidency, PageResidencyBudgets};
    use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord};
    use std::sync::Arc;

    let device = offscreen_device();
    let before = validation_issue_count();
    let counters;
    {
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
        let visibility = SceneVisibility::new(&device).expect("visibility");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let cull = pipelines
            .request_scene_visibility(visibility.layout())
            .expect("cull pso");
        let traversal = pipelines
            .request_scene_traversal(visibility.traversal_layout())
            .expect("traversal pso");

        let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut pending = GpuScenePendingUploads::default();
        let mut residency = PageResidency::new(PageResidencyBudgets::default());
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");

        let resident_material = gpu_data
            .materials
            .insert(GpuMaterialTableRecord {
                base_color_texture: GpuHandle::INVALID,
                normal_texture: GpuHandle::INVALID,
                coverage: GpuHandle::INVALID,
                parameter_index: 0,
                material_class: crate::GpuMaterialClass::new(
                    saffron_material::AlphaClassification::Opaque,
                    crate::GpuSidedness::Single,
                    saffron_material::SurfaceModel::Standard,
                    crate::GpuTransparency::Opaque,
                    false,
                ),
                shader_index: 0,
                flags: 0,
                proxy_albedo: 0,
                occupancy: 1.0,
            })
            .expect("resident material");
        pending.stage_record(GlobalGpuTableKind::Material, resident_material);

        let page_handle = gpu_data
            .page_table
            .insert(GpuPageRecord {
                parent: GpuHandle::INVALID,
                dependencies: crate::GpuArenaRange::default(),
                byte_offset: 0,
                byte_length: 0,
                resident_generation: 0,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                reserved: 0,
            })
            .expect("page record");
        pending.stage_record(GlobalGpuTableKind::Page, page_handle);
        residency.register_page(page_handle, None, true);
        let payload = crate::build_page_payload(&walk.hierarchy, 0).expect("payload");
        for handle in residency.take_load_requests(16) {
            if handle == page_handle {
                residency.complete_load(handle, payload.bytes.clone());
            }
        }
        residency
            .publish_ready(&mut gpu_data, &mut pending)
            .expect("publish root");

        let material = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: resident_material,
                    source_revision: 1,
                },
            ))
            .expect("material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let scene_root = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: page_handle,
                parent: None,
                source_generation: 1,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
            }))
            .expect("scene page")
        {
            GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let prototype = match gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    geometry: GpuHandle {
                        index: 3,
                        generation: 1,
                    },
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: scene_root,
                    page_bounds: std::sync::Arc::from([]),
                    bounds: walk.prototype_bounds,
                    source_generation: 1,
                    flags: 0,
                    mechanics: [0; 4],
                },
            ))
            .expect("prototype")
        {
            GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        create_instance(&mut gpu_scene, prototype, Vec3::ZERO, walk.instance_flags);

        gpu_data.begin_frame(0).expect("gpu data");
        uploader.begin_frame(0).expect("uploader");
        gpu_scene.begin_frame(0).expect("scene");
        let mut graph = RenderGraph::new();
        record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
            .expect("drain pending");
        uploader
            .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
            .expect("record");
        one_shot(&device, |cmd| graph.execute(&device, cmd));

        let wind_buffer = wind_records_buffer(&device, &[walk.wind_record]);
        let block = uploader.build_address_block(
            &device,
            &gpu_data,
            WORLD,
            0,
            (0, 0),
            device.buffer_device_address(wind_buffer.handle()),
            0,
            0,
            crate::DisplacedFrameAddresses::default(),
            (0, 0),
            0,
        );
        let address_ubo = Buffer::new(
            device.resources(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("address ubo");
        // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&block).as_ptr(),
                address_ubo.mapped_ptr(),
                size_of::<crate::GpuSceneAddressBlock>(),
            );
        }
        let address = (
            address_ubo.handle(),
            0,
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );

        let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
            .expect("view lists");
        let open = hzb_image(&device, 1.0);
        let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 20000.0)
            * Mat4::look_at_rh(walk.eye, walk.target, Vec3::Y);
        view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

        counters = node_cull
            .iter()
            .map(|node_cull| {
                let mut graph = RenderGraph::new();
                let hzb_res = graph.import_image(
                    open.handle(),
                    open.view(),
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::GENERAL,
                    None,
                );
                let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
                view.add_cull_pass(
                    &device,
                    &mut graph,
                    &cull,
                    0,
                    hzb_res,
                    wind_records_res,
                    64,
                    SceneVisibilityPush {
                        view_proj: view_proj.to_cols_array(),
                        prev_view_proj: view_proj.to_cols_array(),
                        hzb_extent: [64, 64],
                        hzb_mip_count: 7,
                        pass_kind: SCENE_VISIBILITY_PASS_CULL,
                        history_valid: 0,
                        list_capacity: 64,
                        reserved: [0; 2],
                        reach_min: [0.0; 4],
                        reach_max: [0.0; 4],
                    },
                );
                view.add_traversal_pass(
                    &device,
                    &mut graph,
                    &traversal,
                    0,
                    SceneTraversalPush {
                        view_proj: view_proj.to_cols_array(),
                        eye: walk.eye.to_array(),
                        proj_scale: 1000.0,
                        error_threshold_px: 1.0e9,
                        record_capacity: 256,
                        list_capacity: 64,
                        survivor: 0,
                        displaced_records: 0,
                        transition_frames: 0,
                        frame_stamp: 0,
                        representation_override: SCENE_CUT_AUTO,
                        node_cull: *node_cull,
                        demand_only: 0,
                        view_class: SceneViewClass::Camera.ordinal(),
                    },
                );
                one_shot(&device, |cmd| graph.execute(&device, cmd));
                read_words(
                    &device,
                    view.counters(0),
                    crate::SCENE_VISIBILITY_COUNTER_WORDS as usize,
                )
            })
            .collect();

        device.wait_idle().expect("idle");
        drop(view);
        drop(wind_buffer);
        drop(open);
        drop(address_ubo);
        drop(cull);
        drop(traversal);
        drop(pipelines);
        drop(residency);
        drop(gpu_scene);
        drop(uploader);
        drop(gpu_data);
        drop(visibility);
        drop(descriptors);
    }
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
    counters
}

fn cooked_quad() -> saffron_geometry::PortableVirtualHierarchy {
    use saffron_geometry::glam::{Vec2, Vec3 as GVec3};
    use saffron_geometry::{Mesh, PortableHierarchyInput, Submesh, Vertex};
    let vert = |x: f32, y: f32| Vertex {
        position: GVec3::new(x, y, 0.0),
        normal: GVec3::Z,
        uv0: Vec2::new(x, y),
        ..Default::default()
    };
    let mesh = Mesh {
        vertices: vec![
            vert(0.0, 0.0),
            vert(1.0, 0.0),
            vert(0.0, 1.0),
            vert(1.0, 1.0),
        ],
        indices: vec![0, 1, 2, 1, 3, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 6,
            vertex_offset: 0,
            material_slot: 0,
        }],
    };
    let input = PortableHierarchyInput::from_mesh(&mesh, &[]).expect("input");
    saffron_geometry::cook_portable_virtual_hierarchy(&input).expect("cook")
}
