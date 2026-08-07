mod layout;
mod page_requests;
mod pending;
mod tables;

use super::*;
use crate::GPU_SCENE_TRANSFORM_DYNAMIC;
use crate::device::validation_issue_count;
use crate::global_gpu_data::{
    GlobalGpuData, GlobalGpuTableKind, GpuArenaRange, GpuHandle, GpuSceneInstanceGpuRecord,
    GpuSceneLightGpuRecord, GpuScenePrototypeGpuRecord, GpuSceneReferenceGpuRecord,
    GpuTableSlotHeader, SceneInstanceTable,
};
use crate::gpu_scene_upload::{
    GpuArenaUploadRequest, GpuScenePendingUploads, GpuSceneTableStorage, GpuSceneUploader,
    PageRequestDrain, record_pending_global_uploads,
};
use crate::gpu_types::MaterialParamsData;
use crate::persistent_gpu_scene::{
    GpuSceneDynamicTransform, GpuSceneLightRecord, GpuSceneMaterialOverride,
    GpuSceneMaterialRecord, GpuScenePageRecord, GpuSceneSharedDelta, GpuSceneSharedDeltaResult,
    GpuSceneUploadLimits, GpuSceneWorldDelta, GpuSceneWorldDeltaResult,
};
use crate::persistent_gpu_scene::{GpuSceneTransform, GpuSceneWorldId, PersistentGpuScene};
use crate::render_graph::RenderGraph;
use crate::{Device, GpuLight, SurfaceSource};
use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};
use std::sync::Arc;

const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

fn device_or_skip() -> Option<Device> {
    match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Some(device),
        Err(err) => {
            eprintln!("skipping (no Vulkan device): {err}");
            None
        }
    }
}

fn device_handle(index: u32) -> GpuHandle {
    GpuHandle {
        index,
        generation: 1,
    }
}

/// Appends `slots` to `class`'s region of frame slot 0 exactly as the shader would,
/// including the count word running past the budget on the ones that did not fit.
fn append_page_requests(uploader: &GpuSceneUploader, class: crate::SceneViewClass, slots: &[u32]) {
    let region = class.ordinal() as usize;
    let budget = uploader.page_request_budget() as usize;
    // SAFETY: HOST_VISIBLE + MAPPED, nothing in flight in this test, and every offset
    // is inside frame slot 0's own slice.
    unsafe {
        let count_ptr = uploader
            .page_requests
            .mapped_ptr()
            .add(region * 4)
            .cast::<u32>();
        let region_base = PAGE_REQUEST_HEADER_BYTES
            + region * PAGE_REQUEST_CAPACITY as usize * PAGE_REQUEST_ENTRY_BYTES;
        for slot in slots {
            let index = count_ptr.read_unaligned();
            count_ptr.write_unaligned(index + 1);
            if (index as usize) < budget {
                uploader
                    .page_requests
                    .mapped_ptr()
                    .add(region_base + index as usize * PAGE_REQUEST_ENTRY_BYTES)
                    .cast::<u32>()
                    .write_unaligned(*slot);
            }
        }
    }
}

/// Records and submits `graph` on a throwaway pool, waiting for completion.
fn run_graph(device: &Device, graph: &mut RenderGraph) {
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Pool/cmd/fence are destroyed after the wait below.
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
        graph.execute(device, cmd);
        raw.end_command_buffer(cmd).expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submit, fence, "gpu-scene upload test")
            .expect("submit");
        raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
}

/// Copies `bytes` from a device-local buffer into host memory.
fn read_device_buffer(device: &Device, buffer: vk::Buffer, bytes: u64) -> Vec<u8> {
    let staging = crate::Buffer::new(
        device.resources(),
        bytes,
        vk::BufferUsageFlags::TRANSFER_DST,
        &vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        },
    )
    .expect("staging");
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. One-off copy; everything destroyed after the wait.
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
        let barrier = vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
        let barriers = [barrier];
        raw.cmd_pipeline_barrier2(
            cmd,
            &vk::DependencyInfo::default().memory_barriers(&barriers),
        );
        let region = vk::BufferCopy {
            src_offset: 0,
            dst_offset: 0,
            size: bytes,
        };
        raw.cmd_copy_buffer(cmd, buffer, staging.handle(), &[region]);
        raw.end_command_buffer(cmd).expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submit, fence, "gpu-scene readback")
            .expect("submit");
        raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    let mut out = vec![0_u8; bytes as usize];
    // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
    unsafe {
        std::ptr::copy_nonoverlapping(staging.mapped_ptr(), out.as_mut_ptr(), out.len());
    }
    out
}

fn slot_bytes(all: &[u8], stride: u64, slot: u32) -> &[u8] {
    let start = (u64::from(slot) * stride) as usize;
    &all[start..start + stride as usize]
}

struct Harness {
    gpu_scene: PersistentGpuScene,
    uploader: GpuSceneUploader,
    gpu_data: GlobalGpuData,
    device: Device,
}

fn harness() -> Option<Harness> {
    let device = device_or_skip()?;
    let gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
    let uploader = GpuSceneUploader::new(&device).expect("uploader");
    let mut gpu_scene = PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
    gpu_scene.create_world(WORLD).expect("world");
    Some(Harness {
        gpu_scene,
        uploader,
        gpu_data,
        device,
    })
}

impl Harness {
    fn begin(&mut self, slot: usize) {
        self.gpu_data.begin_frame(slot).expect("gpu data begin");
        self.uploader.begin_frame(slot).expect("uploader begin");
        self.gpu_scene.begin_frame(slot).expect("scene begin");
    }

    fn record_and_run(&mut self, slot: usize) -> GpuSceneUploadRunStats {
        let mut graph = RenderGraph::new();
        let stats = self
            .uploader
            .record_frame(
                &self.device,
                &mut graph,
                &mut self.gpu_data,
                &mut self.gpu_scene,
                slot,
            )
            .expect("record frame");
        run_graph(&self.device, &mut graph);
        stats
    }

    fn finish(self) {
        let Harness {
            gpu_scene,
            uploader,
            gpu_data,
            device,
        } = self;
        device.wait_idle().expect("idle");
        drop(gpu_scene);
        drop(uploader);
        drop(gpu_data);
        drop(device);
    }
}
