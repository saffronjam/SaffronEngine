//! The windowed standalone host's present path: blit the post-processed offscreen viewport image
//! onto the acquired swapchain image, then present.
//!
//! The blit is a second submit after [`crate::Renderer::render_scene_offscreen`], which signals a
//! per-slot scene-finished semaphore in this mode. The present submit waits on that semaphore and
//! on the acquire's image-available semaphore, records the layout transitions + the blit, signals
//! the swapchain image's render-finished semaphore, and presents.

use ash::vk;

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::{Device, Error, Result, checked};

/// The per-frame-slot sync + command resources for the windowed present blit.
///
/// One ring entry per in-flight frame: a command pool + buffer for the blit, the
/// "scene-finished" semaphore the offscreen submit signals and the present submit waits
/// on, and the present submit's own fence (so a slot's blit is complete before its
/// resources are reused). Not a `Drop` type — the handles borrow the device, so the owning
/// [`crate::Renderer`] calls [`PresentSync::destroy`] after `wait_idle`, before the device
/// is torn down.
pub struct PresentSync {
    slots: Vec<PresentSlot>,
    /// The swapchain acquisition currently owned by the frame loop. Internal offscreen renders
    /// run outside this state and therefore cannot signal a present semaphore.
    acquired_frame: Option<AcquiredPresentFrame>,
}

/// One acquire-to-present transaction and its binary-semaphore state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquiredPresentFrame {
    /// The acquired swapchain image.
    pub image_index: u32,
    /// The frame-ring slot whose image-available and present resources own the transaction.
    pub slot: usize,
    /// Whether an offscreen submit signaled this slot's scene-finished semaphore.
    pub scene_finished_signaled: bool,
}

/// One present-ring slot's resources.
struct PresentSlot {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    /// Signaled by the offscreen submit, waited by the present submit: "the scene is
    /// rendered into the offscreen, the blit may read it".
    scene_finished: vk::Semaphore,
    /// The present submit's fence, waited before the slot's resources are reused.
    present_fence: vk::Fence,
}

impl PresentSync {
    /// Allocates the per-slot present command pools/buffers, scene-finished semaphores, and
    /// (signaled) present fences — built only on the windowed present path, when a swapchain
    /// exists.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call; on partial failure the
    /// already-created handles are freed before returning.
    pub fn new(device: &Device) -> Result<Self> {
        let raw = device.raw();
        let mut slots = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            match Self::create_slot(device) {
                Ok(slot) => slots.push(slot),
                Err(err) => {
                    for slot in &slots {
                        // SAFETY: the ash seam. Each handle was created on this device and is
                        // destroyed exactly once on the error path.
                        unsafe { Self::free_slot(raw, slot) };
                    }
                    return Err(err);
                }
            }
        }
        Ok(Self {
            slots,
            acquired_frame: None,
        })
    }

    fn create_slot(device: &Device) -> Result<PresentSlot> {
        let raw = device.raw();
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. The create-info is valid; the pool is owned and freed in
        // `destroy` / the error path.
        let command_pool = checked(
            unsafe { raw.create_command_pool(&pool_info, None) },
            "present: create_command_pool",
        )?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the pool above.
        let command_buffer = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "present: allocate_command_buffers",
        )?[0];
        // SAFETY: the ash seam. Default-info semaphore creation.
        let scene_finished = checked(
            unsafe { raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) },
            "present: create_semaphore",
        )?;
        // Signaled so the first frame's wait returns immediately (the slot has no prior
        // present to await).
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: the ash seam. Creates the present fence signaled.
        let present_fence = checked(
            unsafe { raw.create_fence(&fence_info, None) },
            "present: create_fence",
        )?;
        Ok(PresentSlot {
            command_pool,
            command_buffer,
            scene_finished,
            present_fence,
        })
    }

    /// Destroys every slot's handles. Must be called after `wait_idle`, before the device is
    /// torn down (the handles borrow the device).
    pub fn destroy(&mut self, device: &Device) {
        let raw = device.raw();
        for slot in &self.slots {
            // SAFETY: the ash seam. `wait_idle` ran first; each handle is destroyed exactly
            // once and belongs to this device.
            unsafe { Self::free_slot(raw, slot) };
        }
        self.slots.clear();
    }

    unsafe fn free_slot(raw: &ash::Device, slot: &PresentSlot) {
        // SAFETY: the ash seam. The caller guarantees the device is idle and these handles
        // were created on it.
        unsafe {
            raw.destroy_fence(slot.present_fence, None);
            raw.destroy_semaphore(slot.scene_finished, None);
            raw.destroy_command_pool(slot.command_pool, None);
        }
    }

    /// The scene-finished semaphore for frame slot `index` (the offscreen submit signals it,
    /// the present submit waits on it).
    pub fn scene_finished(&self, index: usize) -> vk::Semaphore {
        self.slots[index].scene_finished
    }

    /// The blit command pool for frame slot `index`.
    pub fn command_pool(&self, index: usize) -> vk::CommandPool {
        self.slots[index].command_pool
    }

    /// The blit command buffer for frame slot `index`.
    pub fn command_buffer(&self, index: usize) -> vk::CommandBuffer {
        self.slots[index].command_buffer
    }

    /// The present submit's fence for frame slot `index`.
    pub fn present_fence(&self, index: usize) -> vk::Fence {
        self.slots[index].present_fence
    }

    /// Opens one acquire-to-present transaction for `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PresentState`] when a prior acquisition has not been presented.
    pub fn set_acquired_frame(&mut self, image_index: u32, slot: usize) -> Result<()> {
        if self.acquired_frame.is_some() {
            return Err(Error::PresentState(
                "cannot acquire before presenting the active frame",
            ));
        }
        self.acquired_frame = Some(AcquiredPresentFrame {
            image_index,
            slot,
            scene_finished_signaled: false,
        });
        Ok(())
    }

    /// Verifies that no acquire-to-present transaction is active.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PresentState`] when an acquired swapchain image has not been presented.
    pub fn ensure_no_acquired_frame(&self) -> Result<()> {
        if self.acquired_frame.is_some() {
            return Err(Error::PresentState(
                "cannot rebuild the swapchain during an active acquired frame",
            ));
        }
        Ok(())
    }

    /// Returns the scene-finished semaphore when `slot` owns an acquired frame. Internal
    /// offscreen renders return `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PresentState`] when an active acquisition belongs to another slot or its
    /// scene-finished semaphore was already signaled.
    pub fn scene_finished_to_signal(&self, slot: usize) -> Result<Option<vk::Semaphore>> {
        let Some(frame) = self.acquired_frame else {
            return Ok(None);
        };
        if frame.slot != slot {
            return Err(Error::PresentState(
                "offscreen render does not own the active acquired frame slot",
            ));
        }
        if frame.scene_finished_signaled {
            return Err(Error::PresentState(
                "cannot signal scene completion twice for one acquired frame",
            ));
        }
        Ok(Some(self.scene_finished(slot)))
    }

    /// Records that `slot`'s scene-finished semaphore was submitted for signaling.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PresentState`] when `slot` does not own the active acquisition or its
    /// semaphore was already signaled.
    pub fn mark_scene_finished_signaled(&mut self, slot: usize) -> Result<()> {
        let Some(frame) = self
            .acquired_frame
            .as_mut()
            .filter(|frame| frame.slot == slot && !frame.scene_finished_signaled)
        else {
            return Err(Error::PresentState(
                "scene signal does not belong to the active acquired frame",
            ));
        };
        frame.scene_finished_signaled = true;
        Ok(())
    }

    /// Takes the acquired frame transaction for presentation.
    pub fn take_acquired_frame(&mut self) -> Option<AcquiredPresentFrame> {
        self.acquired_frame.take()
    }
}

/// Builds the deduplicated fence set that must complete before a present slot and swapchain image
/// can be reused. All returned fences must be waited before `slot_fence` is reset.
pub(crate) fn reuse_fences(
    slot_fence: vk::Fence,
    image_fence: vk::Fence,
) -> ([vk::Fence; 2], usize) {
    let mut fences = [slot_fence, vk::Fence::null()];
    let count = if image_fence != vk::Fence::null() && image_fence != slot_fence {
        fences[1] = image_fence;
        2
    } else {
        1
    };
    (fences, count)
}

/// Records the offscreen → swapchain blit into `cmd` (already begun): transition the
/// offscreen (`from_layout`) → `TRANSFER_SRC`, the swapchain `UNDEFINED` → `TRANSFER_DST`,
/// the `vkCmdBlitImage` (RGBA16F → BGRA8, nearest), then the swapchain `TRANSFER_DST` →
/// `dst_final_layout`. The offscreen is left in `TRANSFER_SRC`; the caller tracks that so the
/// next frame's graph import seeds the right entry layout. The barrier sequence is sync2
/// throughout.
///
/// `dst_final_layout` is `PRESENT_SRC_KHR` for the real windowed present (the presentation
/// engine reads it); the headless content-correctness stand-in (on an offscreen device with no
/// `VK_KHR_swapchain`, where `PRESENT_SRC_KHR` is invalid) passes `TRANSFER_SRC_OPTIMAL` to read
/// the result straight back.
///
/// # Safety
///
/// `cmd` must be in the recording state; `offscreen` and `swapchain_image` must outlive the
/// recorded command. `from_stage`/`from_access` must match the offscreen's current layout's
/// last writer.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn record_present_blit(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    offscreen: vk::Image,
    offscreen_extent: vk::Extent2D,
    from_layout: vk::ImageLayout,
    from_stage: vk::PipelineStageFlags2,
    from_access: vk::AccessFlags2,
    swapchain_image: vk::Image,
    swapchain_extent: vk::Extent2D,
    dst_final_layout: vk::ImageLayout,
) {
    let color_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };

    // Offscreen: its current layout (COLOR_ATTACHMENT after the post chain, or
    // ShaderReadOnly after a prior read-back) → TRANSFER_SRC, so the blit reads it.
    unsafe {
        barrier(
            raw,
            cmd,
            offscreen,
            color_range,
            from_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            from_stage,
            from_access,
            vk::PipelineStageFlags2::BLIT,
            vk::AccessFlags2::TRANSFER_READ,
        );
        // Swapchain: UNDEFINED (its contents are not preserved) → TRANSFER_DST.
        barrier(
            raw,
            cmd,
            swapchain_image,
            color_range,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::TOP_OF_PIPE,
            vk::AccessFlags2::empty(),
            vk::PipelineStageFlags2::BLIT,
            vk::AccessFlags2::TRANSFER_WRITE,
        );
    }

    let layers = vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    };
    let region = vk::ImageBlit2::default()
        .src_subresource(layers)
        .src_offsets([
            vk::Offset3D { x: 0, y: 0, z: 0 },
            vk::Offset3D {
                x: offscreen_extent.width as i32,
                y: offscreen_extent.height as i32,
                z: 1,
            },
        ])
        .dst_subresource(layers)
        .dst_offsets([
            vk::Offset3D { x: 0, y: 0, z: 0 },
            vk::Offset3D {
                x: swapchain_extent.width as i32,
                y: swapchain_extent.height as i32,
                z: 1,
            },
        ]);
    let regions = [region];
    let blit = vk::BlitImageInfo2::default()
        .src_image(offscreen)
        .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .dst_image(swapchain_image)
        .dst_image_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .regions(&regions)
        .filter(vk::Filter::NEAREST);
    // SAFETY: the ash seam. Both images are in the layouts the barriers above set; the blit
    // converts the RGBA16F offscreen to the BGRA8 swapchain image during the copy.
    unsafe { raw.cmd_blit_image2(cmd, &blit) };

    // Swapchain: TRANSFER_DST → `dst_final_layout`. For the windowed present that is
    // `PRESENT_SRC_KHR` (the presentation engine reads it, no destination access); for the
    // headless read-back stand-in it is `TRANSFER_SRC_OPTIMAL` (the read-back copy reads it).
    let (dst_stage, dst_access) = match dst_final_layout {
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => (
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        ),
        _ => (
            vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
            vk::AccessFlags2::empty(),
        ),
    };
    unsafe {
        barrier(
            raw,
            cmd,
            swapchain_image,
            color_range,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            dst_final_layout,
            vk::PipelineStageFlags2::BLIT,
            vk::AccessFlags2::TRANSFER_WRITE,
            dst_stage,
            dst_access,
        );
    }
}

/// One whole-image sync2 layout transition (single color mip).
///
/// # Safety
///
/// `image` must outlive the recorded command; `cmd` must be recording.
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
    // SAFETY: forwarded from this function's contract — the image outlives the command.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

#[cfg(test)]
mod tests {
    use ash::vk::Handle;

    use super::*;

    fn sync_state() -> PresentSync {
        let slot = || PresentSlot {
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            scene_finished: vk::Semaphore::null(),
            present_fence: vk::Fence::null(),
        };
        PresentSync {
            slots: vec![slot(), slot()],
            acquired_frame: None,
        }
    }

    #[test]
    fn internal_offscreen_renders_never_signal_present_semaphores() {
        let sync = sync_state();

        assert!(sync.scene_finished_to_signal(0).expect("query").is_none());
        assert!(sync.scene_finished_to_signal(1).expect("query").is_none());
    }

    #[test]
    fn acquired_frame_signals_its_slot_once_and_carries_it_to_present() {
        let mut sync = sync_state();
        sync.set_acquired_frame(7, 1).expect("acquire");

        assert!(matches!(
            sync.scene_finished_to_signal(0),
            Err(Error::PresentState(_))
        ));
        assert!(sync.scene_finished_to_signal(1).expect("query").is_some());
        sync.mark_scene_finished_signaled(1).expect("mark signal");
        assert!(matches!(
            sync.scene_finished_to_signal(1),
            Err(Error::PresentState(_))
        ));
        assert_eq!(
            sync.take_acquired_frame(),
            Some(AcquiredPresentFrame {
                image_index: 7,
                slot: 1,
                scene_finished_signaled: true,
            })
        );
        assert!(sync.take_acquired_frame().is_none());
    }

    #[test]
    fn acquired_frame_without_scene_render_preserves_its_own_slot() {
        let mut sync = sync_state();
        sync.set_acquired_frame(3, 0).expect("acquire");

        assert_eq!(
            sync.take_acquired_frame(),
            Some(AcquiredPresentFrame {
                image_index: 3,
                slot: 0,
                scene_finished_signaled: false,
            })
        );
    }

    #[test]
    fn double_acquire_is_rejected_in_release_builds() {
        let mut sync = sync_state();
        sync.set_acquired_frame(3, 0).expect("first acquire");

        assert!(matches!(
            sync.set_acquired_frame(4, 1),
            Err(Error::PresentState(_))
        ));
        assert!(matches!(
            sync.ensure_no_acquired_frame(),
            Err(Error::PresentState(_))
        ));
        assert_eq!(
            sync.take_acquired_frame()
                .expect("active frame")
                .image_index,
            3
        );
        sync.ensure_no_acquired_frame().expect("idle after present");
    }

    #[test]
    fn present_reuse_waits_aliasing_slot_and_image_fence_once() {
        let slot = vk::Fence::from_raw(41);
        let other = vk::Fence::from_raw(82);

        let (same, same_count) = reuse_fences(slot, slot);
        assert_eq!(&same[..same_count], &[slot]);
        let (none, none_count) = reuse_fences(slot, vk::Fence::null());
        assert_eq!(&none[..none_count], &[slot]);
        let (distinct, distinct_count) = reuse_fences(slot, other);
        assert_eq!(&distinct[..distinct_count], &[slot, other]);
    }
}
