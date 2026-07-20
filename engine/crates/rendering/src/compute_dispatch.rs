//! Reusable owned Vulkan compute dispatch for runtime kernels and conformance capture.

use std::sync::Arc;

use ash::vk;

use crate::shader_artifact::{ShaderArtifactIdentity, load_shader_artifact};
use crate::{Buffer, Device, Pipeline};

const FENCE_POLL_NANOSECONDS: u64 = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComputeDispatchAbort {
    Cancelled,
    DeadlineExceeded,
}

pub(crate) enum ComputeDispatchOutcome {
    Complete(Vec<Vec<u8>>),
    Aborted(ComputeDispatchAbort),
}

pub(crate) struct ComputeBuffer {
    pub(crate) bytes: Vec<u8>,
}

impl ComputeBuffer {
    pub(crate) fn zeroed(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

struct Cleanup<'a> {
    raw: &'a ash::Device,
    module: vk::ShaderModule,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    descriptor_pool: vk::DescriptorPool,
    command_pool: vk::CommandPool,
    fence: vk::Fence,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.fence != vk::Fence::null() {
                self.raw.destroy_fence(self.fence, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                self.raw.destroy_command_pool(self.command_pool, None);
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                self.raw.destroy_descriptor_pool(self.descriptor_pool, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                self.raw.destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.set_layout != vk::DescriptorSetLayout::null() {
                self.raw
                    .destroy_descriptor_set_layout(self.set_layout, None);
            }
            if self.module != vk::ShaderModule::null() {
                self.raw.destroy_shader_module(self.module, None);
            }
        }
    }
}

pub(crate) struct ComputeDispatch {
    device: Arc<Device>,
    binding_count: usize,
    shader_artifact_identity: ShaderArtifactIdentity,
    set_layout: vk::DescriptorSetLayout,
    pipeline: Pipeline,
    descriptor_pool: vk::DescriptorPool,
    command_pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
}

impl ComputeDispatch {
    pub(crate) fn new(
        device: Arc<Device>,
        shader: &str,
        binding_count: usize,
    ) -> crate::Result<Self> {
        if binding_count == 0 {
            return Err(crate::Error::ShaderLoad(
                "compute dispatch requires at least one buffer binding".to_owned(),
            ));
        }
        let raw = device.raw();
        let (module, shader_artifact_identity) = load_shader(&device, shader)?;
        let mut cleanup = Cleanup {
            raw,
            module,
            set_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            command_pool: vk::CommandPool::null(),
            fence: vk::Fence::null(),
        };
        let bindings = (0..binding_count)
            .map(|binding| {
                Ok(vk::DescriptorSetLayoutBinding::default()
                    .binding(u32::try_from(binding).map_err(|_| {
                        crate::Error::ShaderLoad(
                            "compute binding count exceeds Vulkan limits".to_owned(),
                        )
                    })?)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE))
            })
            .collect::<crate::Result<Vec<_>>>()?;
        let set_layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        cleanup.set_layout = crate::checked(
            unsafe { raw.create_descriptor_set_layout(&set_layout_info, None) },
            "create_descriptor_set_layout (compute dispatch)",
        )?;
        let set_layouts = [cleanup.set_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        cleanup.pipeline_layout = crate::checked(
            unsafe { raw.create_pipeline_layout(&layout_info, None) },
            "create_pipeline_layout (compute dispatch)",
        )?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"computeMain");
        let pipeline_info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(cleanup.pipeline_layout);
        let pipeline_handle = match unsafe {
            raw.create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        } {
            Ok(pipelines) => pipelines[0],
            Err((_, result)) => {
                return Err(crate::Error::Vk {
                    context: "create_compute_pipelines (compute dispatch)",
                    result,
                });
            }
        };
        let pipeline =
            Pipeline::from_parts(device.resources(), pipeline_handle, cleanup.pipeline_layout);
        cleanup.pipeline_layout = vk::PipelineLayout::null();

        let descriptor_count = u32::try_from(binding_count).map_err(|_| {
            crate::Error::ShaderLoad("compute binding count exceeds Vulkan limits".to_owned())
        })?;
        let pool_size = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(descriptor_count)];
        let descriptor_pool_info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(1)
            .pool_sizes(&pool_size);
        cleanup.descriptor_pool = crate::checked(
            unsafe { raw.create_descriptor_pool(&descriptor_pool_info, None) },
            "create_descriptor_pool (compute dispatch)",
        )?;

        let command_pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .queue_family_index(device.graphics_queue_family);
        cleanup.command_pool = crate::checked(
            unsafe { raw.create_command_pool(&command_pool_info, None) },
            "create_command_pool (compute dispatch)",
        )?;
        let command_allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(cleanup.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command = crate::checked(
            unsafe { raw.allocate_command_buffers(&command_allocate) },
            "allocate_command_buffers (compute dispatch)",
        )?[0];
        cleanup.fence = crate::checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "create_fence (compute dispatch)",
        )?;

        let set_layout = cleanup.set_layout;
        let descriptor_pool = cleanup.descriptor_pool;
        let command_pool = cleanup.command_pool;
        let fence = cleanup.fence;
        cleanup.set_layout = vk::DescriptorSetLayout::null();
        cleanup.descriptor_pool = vk::DescriptorPool::null();
        cleanup.command_pool = vk::CommandPool::null();
        cleanup.fence = vk::Fence::null();
        drop(cleanup);
        Ok(Self {
            device,
            binding_count,
            shader_artifact_identity,
            set_layout,
            pipeline,
            descriptor_pool,
            command_pool,
            command,
            fence,
        })
    }

    pub(crate) fn shader_artifact_identity(&self) -> &ShaderArtifactIdentity {
        &self.shader_artifact_identity
    }

    #[cfg(test)]
    pub(crate) fn run(
        &mut self,
        buffers: Vec<ComputeBuffer>,
        dispatch: [u32; 3],
    ) -> crate::Result<Vec<Vec<u8>>> {
        match self.run_interruptible(buffers, dispatch, || None)? {
            ComputeDispatchOutcome::Complete(buffers) => Ok(buffers),
            ComputeDispatchOutcome::Aborted(_) => Err(crate::Error::ShaderLoad(
                "uninterruptible compute dispatch aborted".to_owned(),
            )),
        }
    }

    pub(crate) fn run_interruptible(
        &mut self,
        buffers: Vec<ComputeBuffer>,
        dispatch: [u32; 3],
        mut abort: impl FnMut() -> Option<ComputeDispatchAbort>,
    ) -> crate::Result<ComputeDispatchOutcome> {
        if buffers.len() != self.binding_count
            || buffers.iter().any(|buffer| buffer.bytes.is_empty())
        {
            return Err(crate::Error::ShaderLoad(format!(
                "compute dispatch requires {} non-empty buffers",
                self.binding_count
            )));
        }
        if dispatch.contains(&0) {
            return Err(crate::Error::ShaderLoad(
                "compute dispatch dimensions must be nonzero".to_owned(),
            ));
        }
        if let Some(reason) = abort() {
            return Ok(ComputeDispatchOutcome::Aborted(reason));
        }
        let device = &self.device;
        let raw = device.raw();

        let allocation = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let mut gpu_buffers = buffers
            .into_iter()
            .map(|source| {
                let mut buffer = Buffer::new(
                    device.resources(),
                    source.bytes.len() as u64,
                    vk::BufferUsageFlags::STORAGE_BUFFER,
                    &allocation,
                )?;
                buffer
                    .mapped_bytes()
                    .ok_or_else(|| {
                        crate::Error::ShaderLoad("compute buffer is not mapped".to_owned())
                    })?
                    .copy_from_slice(&source.bytes);
                buffer.flush_mapped()?;
                Ok(buffer)
            })
            .collect::<crate::Result<Vec<_>>>()?;
        if let Some(reason) = abort() {
            return Ok(ComputeDispatchOutcome::Aborted(reason));
        }

        unsafe {
            crate::checked(
                raw.reset_descriptor_pool(
                    self.descriptor_pool,
                    vk::DescriptorPoolResetFlags::empty(),
                ),
                "reset_descriptor_pool (compute dispatch)",
            )?;
            crate::checked(
                raw.reset_command_pool(self.command_pool, vk::CommandPoolResetFlags::empty()),
                "reset_command_pool (compute dispatch)",
            )?;
            crate::checked(
                raw.reset_fences(&[self.fence]),
                "reset_fence (compute dispatch)",
            )?;
        }
        let set_layouts = [self.set_layout];
        let allocate_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&set_layouts);
        let descriptor_set = crate::checked(
            unsafe { raw.allocate_descriptor_sets(&allocate_info) },
            "allocate_descriptor_sets (compute dispatch)",
        )?[0];
        let buffer_infos = gpu_buffers
            .iter()
            .map(|buffer| {
                [vk::DescriptorBufferInfo::default()
                    .buffer(buffer.handle())
                    .range(buffer.size())]
            })
            .collect::<Vec<_>>();
        let writes = buffer_infos
            .iter()
            .enumerate()
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(u32::try_from(binding).unwrap())
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(info)
            })
            .collect::<Vec<_>>();
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            crate::checked(
                raw.begin_command_buffer(self.command, &begin),
                "begin compute dispatch",
            )?;
            raw.cmd_bind_pipeline(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline.handle(),
            );
            raw.cmd_bind_descriptor_sets(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline.layout(),
                0,
                &[descriptor_set],
                &[],
            );
            raw.cmd_dispatch(self.command, dispatch[0], dispatch[1], dispatch[2]);
            let barrier = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)];
            let dependency = vk::DependencyInfo::default().memory_barriers(&barrier);
            raw.cmd_pipeline_barrier2(self.command, &dependency);
            crate::checked(raw.end_command_buffer(self.command), "end compute dispatch")?;
            let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(self.command)];
            let submits = [vk::SubmitInfo2::default().command_buffer_infos(&commands)];
            if let Some(reason) = abort() {
                return Ok(ComputeDispatchOutcome::Aborted(reason));
            }
            device
                .graphics_queue
                .submit2(raw, &submits, self.fence, "submit compute dispatch")?;
            let mut abort_reason = None;
            loop {
                match raw.wait_for_fences(&[self.fence], true, FENCE_POLL_NANOSECONDS) {
                    Ok(()) => break,
                    Err(vk::Result::TIMEOUT) => {
                        if abort_reason.is_none() {
                            abort_reason = abort();
                        }
                    }
                    Err(result) => {
                        return Err(crate::Error::Vk {
                            context: "wait compute dispatch",
                            result,
                        });
                    }
                }
            }
            if abort_reason.is_none() {
                abort_reason = abort();
            }
            if let Some(reason) = abort_reason {
                return Ok(ComputeDispatchOutcome::Aborted(reason));
            }
        }

        let mut result = Vec::with_capacity(gpu_buffers.len());
        for buffer in &mut gpu_buffers {
            buffer.invalidate_mapped()?;
            result.push(
                buffer
                    .mapped_bytes()
                    .ok_or_else(|| {
                        crate::Error::ShaderLoad("compute buffer is not mapped".to_owned())
                    })?
                    .to_vec(),
            );
        }
        Ok(ComputeDispatchOutcome::Complete(result))
    }
}

impl Drop for ComputeDispatch {
    fn drop(&mut self) {
        unsafe {
            self.device.raw().destroy_fence(self.fence, None);
            self.device
                .raw()
                .destroy_command_pool(self.command_pool, None);
            self.device
                .raw()
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .raw()
                .destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

#[cfg(test)]
pub(crate) fn run_compute(
    device: Arc<Device>,
    shader: &str,
    buffers: Vec<ComputeBuffer>,
    dispatch: [u32; 3],
) -> crate::Result<Vec<Vec<u8>>> {
    ComputeDispatch::new(device, shader, buffers.len())?.run(buffers, dispatch)
}

fn load_shader(
    device: &Device,
    shader: &str,
) -> crate::Result<(vk::ShaderModule, ShaderArtifactIdentity)> {
    let shader_dir = crate::pipelines::resolve_shader_dir();
    let (identity, bytes) = load_shader_artifact(&shader_dir, shader)?;
    let path = shader_dir.join(identity.artifact());
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(crate::Error::ShaderLoad(format!(
            "invalid SPIR-V size for '{}'",
            path.display()
        )));
    }
    let words = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect::<Vec<_>>();
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    let module = crate::checked(
        unsafe { device.raw().create_shader_module(&info, None) },
        "create_shader_module (compute conformance)",
    )?;
    Ok((module, identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SurfaceSource, validation_issue_count};

    #[test]
    fn abort_after_submission_waits_and_leaves_dispatcher_reusable() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Arc::new(device),
            Err(error) => {
                eprintln!("skipping: no Vulkan device obtainable ({error})");
                return;
            }
        };
        let before = validation_issue_count();
        let mut dispatcher =
            ComputeDispatch::new(Arc::clone(&device), "spatial_numeric_test", 1).unwrap();
        let mut checks = 0;
        let outcome = dispatcher
            .run_interruptible(
                vec![ComputeBuffer::zeroed(24 * size_of::<u32>())],
                [1, 1, 1],
                || {
                    checks += 1;
                    (checks >= 4).then_some(ComputeDispatchAbort::Cancelled)
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ComputeDispatchOutcome::Aborted(ComputeDispatchAbort::Cancelled)
        ));
        dispatcher
            .run(
                vec![ComputeBuffer::zeroed(24 * size_of::<u32>())],
                [1, 1, 1],
            )
            .expect("dispatcher remains reusable after a safely completed abort");

        drop(dispatcher);
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
