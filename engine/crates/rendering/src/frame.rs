//! The per-frame command/sync ring: `MAX_FRAMES_IN_FLIGHT` slots, each one command pool + buffer +
//! image-available semaphore + in-flight fence. The handles borrow the device and cannot Drop
//! themselves, so the renderer calls [`FrameRing::destroy`] before device teardown.

use ash::vk;

use crate::{Device, Error, Result, RgQueueAssignment, checked};

/// Frames the GPU may have in flight before the CPU blocks — the double-buffer depth.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// One frame slot's command recording + synchronization primitives.
struct FrameData {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    graph_graphics_commands: Vec<vk::CommandBuffer>,
    compute_command_pool: Option<vk::CommandPool>,
    graph_compute_commands: Vec<vk::CommandBuffer>,
    graphics_timeline: vk::Semaphore,
    graphics_timeline_value: u64,
    compute_timeline: Option<vk::Semaphore>,
    compute_timeline_value: u64,
    image_available: vk::Semaphore,
    in_flight: vk::Fence,
}

#[derive(Default)]
struct PendingFrameHandles {
    command_pool: Option<vk::CommandPool>,
    compute_command_pool: Option<vk::CommandPool>,
    graphics_timeline: Option<vk::Semaphore>,
    compute_timeline: Option<vk::Semaphore>,
    image_available: Option<vk::Semaphore>,
    in_flight: Option<vk::Fence>,
}

impl PendingFrameHandles {
    unsafe fn destroy(self, raw: &ash::Device) {
        unsafe {
            if let Some(fence) = self.in_flight {
                raw.destroy_fence(fence, None);
            }
            if let Some(semaphore) = self.image_available {
                raw.destroy_semaphore(semaphore, None);
            }
            if let Some(semaphore) = self.compute_timeline {
                raw.destroy_semaphore(semaphore, None);
            }
            if let Some(semaphore) = self.graphics_timeline {
                raw.destroy_semaphore(semaphore, None);
            }
            if let Some(pool) = self.compute_command_pool {
                raw.destroy_command_pool(pool, None);
            }
            if let Some(pool) = self.command_pool {
                raw.destroy_command_pool(pool, None);
            }
        }
    }
}

/// Command buffers reserved for one compiled graph plus its final graphics tail.
pub(crate) struct FrameGraphCommands {
    pub(crate) graphics: Vec<vk::CommandBuffer>,
    pub(crate) compute: Vec<vk::CommandBuffer>,
    pub(crate) tail: vk::CommandBuffer,
}

/// One queue's per-frame timeline semaphore point.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameTimelinePoint {
    pub(crate) semaphore: vk::Semaphore,
    pub(crate) value: u64,
}

/// The ring of [`FrameData`] slots plus the current index.
///
/// Vulkan command pools are not thread-safe and the handles borrow the device, so
/// this is not a `Drop` type: the owning [`crate::Renderer`] calls
/// [`FrameRing::destroy`] after `wait_idle`, before the device is destroyed.
pub struct FrameRing {
    frames: Vec<FrameData>,
    index: usize,
}

impl FrameRing {
    /// Allocates the per-frame command pools/buffers, image-available semaphores,
    /// and (signaled) in-flight fences.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; on partial
    /// failure the already-created handles are freed before returning.
    pub fn new(device: &Device) -> Result<Self> {
        let raw = device.raw();
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            match Self::create_frame(device) {
                Ok(frame) => frames.push(frame),
                Err(err) => {
                    for frame in &frames {
                        // SAFETY: the ash seam. Each handle was created on this
                        // device and is destroyed exactly once on the error path.
                        unsafe { Self::free_frame(raw, frame) };
                    }
                    return Err(err);
                }
            }
        }
        Ok(Self { frames, index: 0 })
    }

    fn create_frame(device: &Device) -> Result<FrameData> {
        let raw = device.raw();
        let mut pending = PendingFrameHandles::default();
        let result = (|| {
            let pool_info = vk::CommandPoolCreateInfo::default()
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                .queue_family_index(device.graphics_queue_family);
            let command_pool = checked(
                unsafe { raw.create_command_pool(&pool_info, None) },
                "create_command_pool",
            )?;
            pending.command_pool = Some(command_pool);

            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let command_buffer = checked(
                unsafe { raw.allocate_command_buffers(&alloc_info) },
                "allocate_command_buffers",
            )?[0];

            let compute_command_pool = if let Some(family) = device.compute_queue_family {
                let info = vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(family);
                let pool = checked(
                    unsafe { raw.create_command_pool(&info, None) },
                    "create_compute_command_pool",
                )?;
                pending.compute_command_pool = Some(pool);
                Some(pool)
            } else {
                None
            };

            let graphics_timeline = create_timeline_semaphore(raw)?;
            pending.graphics_timeline = Some(graphics_timeline);
            let compute_timeline = if device.compute_queue.is_some() {
                let semaphore = create_timeline_semaphore(raw)?;
                pending.compute_timeline = Some(semaphore);
                Some(semaphore)
            } else {
                None
            };

            let image_available = checked(
                unsafe { raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) },
                "create_semaphore",
            )?;
            pending.image_available = Some(image_available);

            let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
            let in_flight = checked(
                unsafe { raw.create_fence(&fence_info, None) },
                "create_fence",
            )?;
            pending.in_flight = Some(in_flight);

            Ok(FrameData {
                command_pool,
                command_buffer,
                graph_graphics_commands: Vec::new(),
                compute_command_pool,
                graph_compute_commands: Vec::new(),
                graphics_timeline,
                graphics_timeline_value: 0,
                compute_timeline,
                compute_timeline_value: 0,
                image_available,
                in_flight,
            })
        })();
        if result.is_err() {
            unsafe { pending.destroy(raw) };
        }
        result
    }

    /// Destroys every slot's handles. Must be called after `wait_idle`, before the
    /// device is torn down (the handles borrow the device).
    pub fn destroy(&mut self, device: &Device) {
        // The published hang records hold these timeline handles; drop them before the semaphores
        // go, so a stale handle can never reach `vkGetSemaphoreCounterValue`.
        crate::watchdog::clear_frames();
        let raw = device.raw();
        for frame in &self.frames {
            // SAFETY: the ash seam. `wait_idle` ran first, so no handle is in use;
            // each is destroyed exactly once.
            unsafe { Self::free_frame(raw, frame) };
        }
        self.frames.clear();
    }

    /// Frees one slot's handles. The pool free also frees its command buffer.
    unsafe fn free_frame(raw: &ash::Device, frame: &FrameData) {
        // SAFETY: the ash seam. The caller guarantees the device is idle and these
        // handles were created on it.
        unsafe {
            raw.destroy_fence(frame.in_flight, None);
            raw.destroy_semaphore(frame.image_available, None);
            if let Some(semaphore) = frame.compute_timeline {
                raw.destroy_semaphore(semaphore, None);
            }
            raw.destroy_semaphore(frame.graphics_timeline, None);
            if let Some(pool) = frame.compute_command_pool {
                raw.destroy_command_pool(pool, None);
            }
            raw.destroy_command_pool(frame.command_pool, None);
        }
    }

    /// The current frame slot's in-flight fence.
    pub fn in_flight(&self) -> vk::Fence {
        self.frames[self.index].in_flight
    }

    /// The current frame slot's image-available semaphore.
    pub fn image_available(&self) -> vk::Semaphore {
        self.frames[self.index].image_available
    }

    /// The image-available semaphore for an explicit slot `index`. The windowed present path
    /// acquires in `begin_frame` (the current slot) but presents in `end_frame` after the
    /// offscreen submit advanced the ring, so it reads the just-rendered slot's semaphore by
    /// index rather than the (already advanced) current slot.
    pub fn image_available_for(&self, index: usize) -> vk::Semaphore {
        self.frames[index].image_available
    }

    /// The current frame slot's command pool.
    pub fn command_pool(&self) -> vk::CommandPool {
        self.frames[self.index].command_pool
    }

    /// The current frame slot's command buffer.
    pub fn command_buffer(&self) -> vk::CommandBuffer {
        self.frames[self.index].command_buffer
    }

    /// Resets both command pools for the current fence-cleared frame slot.
    pub fn reset_command_pools(&self, device: &Device) -> Result<()> {
        let raw = device.raw();
        let frame = &self.frames[self.index];
        checked(
            unsafe {
                raw.reset_command_pool(frame.command_pool, vk::CommandPoolResetFlags::empty())
            },
            "reset graphics command pool",
        )?;
        if let Some(pool) = frame.compute_command_pool {
            checked(
                unsafe { raw.reset_command_pool(pool, vk::CommandPoolResetFlags::empty()) },
                "reset compute command pool",
            )?;
        }
        Ok(())
    }

    /// Ensures one primary command per graph batch and a final graphics tail command.
    pub fn prepare_graph_commands(
        &mut self,
        device: &Device,
        graphics_batches: usize,
        compute_batches: usize,
    ) -> Result<FrameGraphCommands> {
        let raw = device.raw();
        let frame = &mut self.frames[self.index];
        ensure_commands(
            raw,
            frame.command_pool,
            &mut frame.graph_graphics_commands,
            graphics_batches + 1,
        )?;
        if compute_batches != 0 {
            let pool = frame.compute_command_pool.ok_or(Error::PresentState(
                "compute graph batches require a compute command pool",
            ))?;
            ensure_commands(
                raw,
                pool,
                &mut frame.graph_compute_commands,
                compute_batches,
            )?;
        }
        Ok(FrameGraphCommands {
            graphics: frame.graph_graphics_commands[..graphics_batches].to_vec(),
            compute: frame.graph_compute_commands[..compute_batches].to_vec(),
            tail: frame.graph_graphics_commands[graphics_batches],
        })
    }

    /// Reserves the next timeline point for one queue in the current frame slot.
    pub fn reserve_timeline(&mut self, queue: RgQueueAssignment) -> Result<FrameTimelinePoint> {
        let frame = &mut self.frames[self.index];
        let (semaphore, value) = match queue {
            RgQueueAssignment::Graphics => {
                frame.graphics_timeline_value = frame
                    .graphics_timeline_value
                    .checked_add(1)
                    .ok_or(Error::TimelineValueOverflow)?;
                (frame.graphics_timeline, frame.graphics_timeline_value)
            }
            RgQueueAssignment::AsyncCompute => {
                let semaphore = frame.compute_timeline.ok_or(Error::PresentState(
                    "async-compute timeline requested without an async-compute queue",
                ))?;
                frame.compute_timeline_value = frame
                    .compute_timeline_value
                    .checked_add(1)
                    .ok_or(Error::TimelineValueOverflow)?;
                (semaphore, frame.compute_timeline_value)
            }
        };
        Ok(FrameTimelinePoint { semaphore, value })
    }

    /// The current frame slot's index (0..[`MAX_FRAMES_IN_FLIGHT`)), keying the
    /// per-frame instance / material SSBOs.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Advances to the next slot in the ring.
    pub fn advance(&mut self) {
        self.index = (self.index + 1) % MAX_FRAMES_IN_FLIGHT;
    }
}

fn create_timeline_semaphore(raw: &ash::Device) -> Result<vk::Semaphore> {
    let mut timeline = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let info = vk::SemaphoreCreateInfo::default().push_next(&mut timeline);
    checked(
        unsafe { raw.create_semaphore(&info, None) },
        "create timeline semaphore",
    )
}

fn ensure_commands(
    raw: &ash::Device,
    pool: vk::CommandPool,
    commands: &mut Vec<vk::CommandBuffer>,
    count: usize,
) -> Result<()> {
    if commands.len() >= count {
        return Ok(());
    }
    let additional = count - commands.len();
    let command_buffer_count = u32::try_from(additional)
        .map_err(|_| Error::InvalidUploadData("render-graph command count exceeds u32".into()))?;
    let info = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(command_buffer_count);
    commands.extend(checked(
        unsafe { raw.allocate_command_buffers(&info) },
        "allocate render-graph command buffers",
    )?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_values_never_wrap() {
        let frame = FrameData {
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            graph_graphics_commands: Vec::new(),
            compute_command_pool: None,
            graph_compute_commands: Vec::new(),
            graphics_timeline: vk::Semaphore::null(),
            graphics_timeline_value: u64::MAX,
            compute_timeline: None,
            compute_timeline_value: 0,
            image_available: vk::Semaphore::null(),
            in_flight: vk::Fence::null(),
        };
        let mut ring = FrameRing {
            frames: vec![frame],
            index: 0,
        };
        assert!(matches!(
            ring.reserve_timeline(RgQueueAssignment::Graphics),
            Err(Error::TimelineValueOverflow)
        ));
    }
}
