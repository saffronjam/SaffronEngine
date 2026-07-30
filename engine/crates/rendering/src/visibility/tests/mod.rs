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

/// A device-local wind sway record buffer sized for the tests' 64-slot views.
fn wind_records_buffer(device: &Device) -> Buffer {
    Buffer::new(
        device.resources(),
        64 * size_of::<crate::GpuWindInstanceRecord>() as u64,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        },
    )
    .expect("wind records buffer")
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
        u64::from(view.record_capacity()) * 20,
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
) -> GpuHandle {
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
                flags: 0,
                combination: 0,
                vegetation: None,
            }),
        )
        .expect("instance")
    {
        GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle.raw(),
        other => panic!("unexpected {other:?}"),
    }
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
