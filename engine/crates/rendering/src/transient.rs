//! Per-frame-in-flight **transient (scratch) resources** for the render graph.
//!
//! A render pass often needs a buffer/image that lives for only part of a frame — a compute
//! prepass's output consumed by later passes, then discarded. The render graph derives barriers for
//! any resource it is handed (a transient is just an imported resource with fresh state), but it is
//! rebuilt from scratch every frame, so it cannot *own* an allocation: a buffer freed at graph
//! teardown would be released while the GPU is still reading it (use-after-free).
//!
//! This pool owns the allocations instead, keyed per frame-in-flight and **grow-only** — exactly the
//! skinning deformed-buffer discipline. A pass acquires a transient for the current frame slot; the
//! pool reuses a prior allocation of matching size/usage at the same acquire position or grows a new
//! one. Because a frame slot is only reset ([`TransientResources::begin_frame`]) after that slot's
//! fence has signalled — `MAX_FRAMES_IN_FLIGHT` frames later — an acquired transient safely outlives
//! the GPU work that reads it. Aliasing distinct lifetimes onto one allocation is a future
//! optimization; this is the correct, portable baseline.

use std::sync::Arc;

use ash::vk;

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{Buffer, DeviceResources, Image, ImageDesc};

/// Initial transient-buffer capacity (bytes) — the first allocation of any acquire slot rounds up to
/// at least this, and doubles from there, so a hot slot stops reallocating quickly.
const INITIAL_TRANSIENT_BYTES: u64 = 64 * 1024;

/// Grow-only capacity: keep the current size if it already fits, else double until it does (never
/// shrink), starting from [`INITIAL_TRANSIENT_BYTES`].
fn grow_bytes(current: u64, needed: u64) -> u64 {
    let mut capacity = if current == 0 {
        INITIAL_TRANSIENT_BYTES
    } else {
        current
    };
    while capacity < needed {
        capacity *= 2;
    }
    capacity
}

struct TransientBuffer {
    buffer: Buffer,
    capacity: u64,
    usage: vk::BufferUsageFlags,
}

struct TransientImage {
    image: Image,
    desc: ImageDesc,
}

#[derive(Default)]
struct FrameTransient {
    buffers: Vec<TransientBuffer>,
    buffer_cursor: usize,
    images: Vec<TransientImage>,
    image_cursor: usize,
}

/// The render graph's scratch-resource allocator, one growable pool per frame-in-flight.
pub struct TransientResources {
    resources: Arc<DeviceResources>,
    frames: Vec<FrameTransient>,
}

impl TransientResources {
    /// A pool with an empty per-frame ring, allocating from the device's VMA allocator.
    pub fn new(resources: Arc<DeviceResources>) -> Self {
        let frames = (0..MAX_FRAMES_IN_FLIGHT)
            .map(|_| FrameTransient::default())
            .collect();
        Self { resources, frames }
    }

    /// Recycle a frame slot's transients — call at frame begin **after** the slot's in-flight fence
    /// has been waited on, so a prior frame's still-in-flight transients are only reused once its GPU
    /// work has completed. Rewinds the acquire cursors; the allocations are kept (grow-only).
    pub fn begin_frame(&mut self, frame: usize) {
        let f = &mut self.frames[frame];
        f.buffer_cursor = 0;
        f.image_cursor = 0;
    }

    /// Acquire a transient buffer of at least `size` bytes carrying `usage`, for `frame`. Passes must
    /// acquire in the same order every frame (a fixed graph does): each acquire advances a cursor and
    /// reuses the allocation parked there when it already fits + covers the usage, else reallocates
    /// it. Returns the raw handle to hand to [`crate::render_graph::RenderGraph::import_buffer`].
    pub fn acquire_buffer(
        &mut self,
        frame: usize,
        size: u64,
        usage: vk::BufferUsageFlags,
    ) -> crate::Result<vk::Buffer> {
        let f = &mut self.frames[frame];
        let i = f.buffer_cursor;
        f.buffer_cursor += 1;
        let fits =
            matches!(f.buffers.get(i), Some(b) if b.capacity >= size && b.usage.contains(usage));
        if !fits {
            let capacity = grow_bytes(f.buffers.get(i).map_or(0, |b| b.capacity), size.max(1));
            let alloc = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            };
            let buffer = Buffer::new(&self.resources, capacity, usage, &alloc)?;
            let entry = TransientBuffer {
                buffer,
                capacity,
                usage,
            };
            if i < f.buffers.len() {
                f.buffers[i] = entry;
            } else {
                f.buffers.push(entry);
            }
        }
        Ok(f.buffers[i].buffer.handle())
    }

    /// Acquire a transient image matching `desc`, for `frame` (same cursor discipline as
    /// [`TransientResources::acquire_buffer`]). Returns `(image, view)` handles for
    /// [`crate::render_graph::RenderGraph::import_image`] (its initial layout is `UNDEFINED`).
    pub fn acquire_image(
        &mut self,
        frame: usize,
        desc: &ImageDesc,
    ) -> crate::Result<(vk::Image, vk::ImageView)> {
        let f = &mut self.frames[frame];
        let i = f.image_cursor;
        f.image_cursor += 1;
        let fits = matches!(f.images.get(i), Some(im) if im.desc == *desc);
        if !fits {
            let image = Image::new(&self.resources, desc)?;
            let entry = TransientImage { image, desc: *desc };
            if i < f.images.len() {
                f.images[i] = entry;
            } else {
                f.images.push(entry);
            }
        }
        let im = &f.images[i];
        Ok((im.image.handle(), im.image.view()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_bytes_doubles_and_never_shrinks() {
        // From empty: rounds up to the initial floor, then doubles until it fits.
        assert_eq!(grow_bytes(0, 1), INITIAL_TRANSIENT_BYTES);
        assert_eq!(
            grow_bytes(0, INITIAL_TRANSIENT_BYTES + 1),
            INITIAL_TRANSIENT_BYTES * 2
        );
        // Already big enough: unchanged (grow-only never shrinks).
        assert_eq!(
            grow_bytes(INITIAL_TRANSIENT_BYTES * 4, 10),
            INITIAL_TRANSIENT_BYTES * 4
        );
        // Grows from the current high-water mark, not the floor.
        assert_eq!(
            grow_bytes(INITIAL_TRANSIENT_BYTES * 4, INITIAL_TRANSIENT_BYTES * 5),
            INITIAL_TRANSIENT_BYTES * 8
        );
    }
}
