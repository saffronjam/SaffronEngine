//! Fence-safe physical allocations for graph-owned resources.
//!
//! The graph owns resource declarations while this pool retains their Vulkan allocations beyond the
//! short-lived graph instance. Transients are keyed per frame-in-flight. Persistent buffers are keyed
//! globally, and a replaced allocation stays retired until every potentially referencing frame slot
//! has crossed its fence.
//!
//! A frame slot is recycled by [`RenderGraphResources::begin_frame`] only after its fence signals, so
//! transient images and buffers cannot be freed under live GPU work. Stable keys make allocation
//! reuse independent of conditional pass order.

use std::sync::Arc;

use ash::vk;

use crate::descriptors::MAX_BLOOM_MIPS;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::render_graph::{RgBufferDesc, RgBufferLifetime, RgBufferResource};
use crate::resources::{Buffer, DeviceResources, Image, Image3D, ImageDesc};

/// The stable transient keys for the bloom mip pyramid — one keyed image per level so each level's
/// barriers stay independent (the transient pool returns one view per key). Sized to
/// [`MAX_BLOOM_MIPS`]; the live level count is chosen per frame off the display extent.
pub(crate) const BLOOM_MIP_KEYS: [&str; MAX_BLOOM_MIPS] = [
    "bloom-mip-0",
    "bloom-mip-1",
    "bloom-mip-2",
    "bloom-mip-3",
    "bloom-mip-4",
    "bloom-mip-5",
    "bloom-mip-6",
];

/// The two ping-pong buffers for the anamorphic streak: a horizontally-squeezed blur of the bright
/// pyramid widened across two passes. Keyed like the mip chain so each buffer's barriers stay
/// independent and nothing outlives the frame.
pub(crate) const BLOOM_STREAK_KEYS: [&str; 2] = ["bloom-streak-0", "bloom-streak-1"];

/// The stable transient keys for the froxel-volume 3D scratch — one keyed [`Image3D`] per logical
/// volume so each keeps an independent grow-only slot and independent barriers, the same discipline
/// as [`BLOOM_MIP_KEYS`]. Reserved for a transient froxel scratch volume; the volumetric-fog stage
/// (`froxel_fog::FroxelFog`) owns its scatter + integration volumes as fixed-size persistent images,
/// so the transient 3D path stays available for later local-fog / aerial-perspective scratch.
pub const FROXEL_VOLUME_KEYS: [&str; 1] = ["froxel-integration"];

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

/// How many frames of acquire history the reclaim window tracks: a slot is only shrunk when its
/// capacity has exceeded the peak request across this many consecutive frames.
const RECLAIM_WINDOW: usize = 8;
/// A slot is shrunk only when its capacity exceeds the recent peak request by this factor — hysteresis
/// so a slot at (or just above) its working size is never churned.
const RECLAIM_MARGIN: u64 = 2;

struct GraphBuffer {
    key: &'static str,
    buffer: Buffer,
    capacity: u64,
    usage: vk::BufferUsageFlags,
    /// Ring of the peak size requested in each of the last [`RECLAIM_WINDOW`] recycle cycles.
    window: [u64; RECLAIM_WINDOW],
    /// Next write position in `window`.
    window_pos: usize,
    /// Peak size requested since the last recycle (folded into `window` at `begin_frame`).
    requested_this_cycle: u64,
}

struct RetiredPersistentBuffer {
    _buffer: Buffer,
    pending_frame_slots: u64,
}

struct TransientImage {
    key: &'static str,
    image: Image,
    desc: ImageDesc,
}

/// A keyed transient 3D volume — the [`Image3D`] analog of [`TransientImage`]. The extent (depth
/// included), format, and usage together form the key's match test: a re-acquire that changes any of
/// them rebuilds the slot, exactly as the 2D path rebuilds on an [`ImageDesc`] mismatch.
struct TransientImage3D {
    key: &'static str,
    image: Image3D,
    extent: vk::Extent3D,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
}

#[derive(Default)]
struct FrameTransient {
    buffers: Vec<GraphBuffer>,
    images: Vec<TransientImage>,
    images_3d: Vec<TransientImage3D>,
}

/// The render graph's scratch-resource allocator, one growable pool per frame-in-flight.
pub struct RenderGraphResources {
    resources: Arc<DeviceResources>,
    frames: Vec<FrameTransient>,
    persistent_buffers: Vec<GraphBuffer>,
    retired_persistent_buffers: Vec<RetiredPersistentBuffer>,
}

impl RenderGraphResources {
    /// A pool with an empty per-frame ring, allocating from the device's VMA allocator.
    pub fn new(resources: Arc<DeviceResources>) -> Self {
        let frames = (0..MAX_FRAMES_IN_FLIGHT)
            .map(|_| FrameTransient::default())
            .collect();
        Self {
            resources,
            frames,
            persistent_buffers: Vec::new(),
            retired_persistent_buffers: Vec::new(),
        }
    }

    pub(crate) fn buffer_state(&self, handle: vk::Buffer) -> crate::RgExternalBufferState {
        self.frames
            .iter()
            .flat_map(|frame| frame.buffers.iter())
            .chain(self.persistent_buffers.iter())
            .find(|buffer| buffer.buffer.handle() == handle)
            .map(|buffer| buffer.buffer.graph_state())
            .expect("graph-owned buffer belongs to the render-graph resource pool")
    }

    pub(crate) fn resolve_buffer_states(&self, graph: &crate::RenderGraph) {
        for (handle, state) in graph.resolved_graph_buffer_states() {
            let buffer = self
                .frames
                .iter()
                .flat_map(|frame| frame.buffers.iter())
                .chain(self.persistent_buffers.iter())
                .find(|buffer| buffer.buffer.handle() == handle)
                .expect("resolved graph buffer belongs to the render-graph resource pool");
            buffer.buffer.set_graph_state(state);
        }
    }

    /// The per-frame recycle hook — call at frame begin **after** the slot's in-flight fence has been
    /// waited on, so this frame slot's transients are done on the GPU and safe to reallocate. With
    /// keyed slots there is no acquire cursor to rewind (a skipped consumer simply does not touch its
    /// key). This is the **shrink/reclaim** point: each key folds its peak request for the cycle into a
    /// rolling window, and a slot whose capacity has exceeded the window's peak by [`RECLAIM_MARGIN`]×
    /// for the whole window is reallocated down to fit that peak (grow-only becomes grow-mostly). The
    /// reclaim is fence-safe because it only frees this slot's allocations, whose fence has signalled.
    pub fn begin_frame(&mut self, frame: usize) {
        let completed_slot = 1_u64 << frame;
        for retired in &mut self.retired_persistent_buffers {
            retired.pending_frame_slots &= !completed_slot;
        }
        self.retired_persistent_buffers
            .retain(|retired| retired.pending_frame_slots != 0);

        let resources = Arc::clone(&self.resources);
        for buf in &mut self.frames[frame].buffers {
            buf.window[buf.window_pos] = buf.requested_this_cycle;
            buf.window_pos = (buf.window_pos + 1) % RECLAIM_WINDOW;
            buf.requested_this_cycle = 0;

            let peak = buf.window.iter().copied().max().unwrap_or(0);
            if peak == 0 || buf.capacity <= peak.saturating_mul(RECLAIM_MARGIN) {
                continue;
            }
            // The slot has run far under its capacity for a full window — shrink it to fit the peak.
            let target = grow_bytes(0, peak);
            if target >= buf.capacity {
                continue;
            }
            let alloc = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            };
            match Buffer::new(&resources, target, buf.usage, &alloc) {
                Ok(smaller) => {
                    buf.buffer = smaller; // drops (frees) the old, fence-cleared allocation
                    buf.capacity = target;
                }
                Err(err) => tracing::warn!("transient reclaim of '{}' failed: {err}", buf.key),
            }
        }
    }

    /// Allocates a graph-declared transient or persistent buffer under one stable key.
    pub(crate) fn allocate_graph_buffer(
        &mut self,
        frame: usize,
        key: &'static str,
        desc: RgBufferDesc,
    ) -> crate::Result<RgBufferResource> {
        if desc.size == 0 {
            return Err(crate::Error::InvalidUploadData(
                "render-graph buffer size must be nonzero".to_owned(),
            ));
        }
        let buffers = match desc.lifetime {
            RgBufferLifetime::Transient => &mut self.frames[frame].buffers,
            RgBufferLifetime::Persistent => &mut self.persistent_buffers,
            RgBufferLifetime::Imported => {
                return Err(crate::Error::InvalidUploadData(
                    "an imported render-graph buffer cannot be allocated".to_owned(),
                ));
            }
        };
        let existing = buffers.iter().position(|b| b.key == key);
        if let Some(i) = existing {
            // Record the request for the reclaim window whether or not the slot already fits.
            buffers[i].requested_this_cycle = buffers[i].requested_this_cycle.max(desc.size);
            if buffers[i].capacity >= desc.size && buffers[i].usage.contains(desc.usage) {
                return Ok(RgBufferResource {
                    buffer: buffers[i].buffer.handle(),
                    size: buffers[i].capacity,
                    usage: buffers[i].usage,
                    lifetime: desc.lifetime,
                });
            }
        }
        // Grow from the key's current high-water (not the floor) and union the usage so a later
        // acquire with a subset of the flags still reuses the allocation.
        let current_cap = existing.map_or(0, |i| buffers[i].capacity);
        let usage = existing.map_or(desc.usage, |i| buffers[i].usage | desc.usage);
        let window = existing.map_or([0; RECLAIM_WINDOW], |i| buffers[i].window);
        let window_pos = existing.map_or(0, |i| buffers[i].window_pos);
        let capacity = grow_bytes(current_cap, desc.size);
        let alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let buffer = Buffer::new(&self.resources, capacity, usage, &alloc)?;
        let entry = GraphBuffer {
            key,
            buffer,
            capacity,
            usage,
            window,
            window_pos,
            requested_this_cycle: desc.size,
        };
        let mut retired_buffer = None;
        let i = match existing {
            Some(i) => {
                let old = std::mem::replace(&mut buffers[i], entry);
                if desc.lifetime == RgBufferLifetime::Persistent {
                    retired_buffer = Some(old.buffer);
                }
                i
            }
            None => {
                buffers.push(entry);
                buffers.len() - 1
            }
        };
        let resource = RgBufferResource {
            buffer: buffers[i].buffer.handle(),
            size: buffers[i].capacity,
            usage: buffers[i].usage,
            lifetime: desc.lifetime,
        };
        if let Some(buffer) = retired_buffer {
            let all_slots = (1_u64 << MAX_FRAMES_IN_FLIGHT) - 1;
            self.retired_persistent_buffers
                .push(RetiredPersistentBuffer {
                    _buffer: buffer,
                    pending_frame_slots: all_slots,
                });
        }
        Ok(resource)
    }

    /// Acquire a transient image matching `desc`, for `frame`, under a stable `&'static str` `key`
    /// (same keyed discipline as graph-owned buffers). Returns `(image, view)`
    /// handles for [`crate::render_graph::RenderGraph::import_image`] (its initial layout is
    /// `UNDEFINED`).
    pub fn acquire_image(
        &mut self,
        frame: usize,
        key: &'static str,
        desc: &ImageDesc,
    ) -> crate::Result<(vk::Image, vk::ImageView)> {
        let f = &mut self.frames[frame];
        let existing = f.images.iter().position(|im| im.key == key);
        if let Some(i) = existing
            && f.images[i].desc == *desc
        {
            let im = &f.images[i];
            return Ok((im.image.handle(), im.image.view()));
        }
        let image = Image::new(&self.resources, desc)?;
        let entry = TransientImage {
            key,
            image,
            desc: *desc,
        };
        let i = match existing {
            Some(i) => {
                f.images[i] = entry;
                i
            }
            None => {
                f.images.push(entry);
                f.images.len() - 1
            }
        };
        let im = &f.images[i];
        Ok((im.image.handle(), im.image.view()))
    }

    /// Acquire a transient 3D image under a stable `&'static str` `key`, keyed exactly like
    /// [`acquire_image`] — the same grow-only, order- and skip-independent discipline, one key per
    /// logical volume (see [`FROXEL_VOLUME_KEYS`]). Returns `(image, view)` handles for
    /// [`crate::render_graph::RenderGraph::import_image_3d`] (initial layout `UNDEFINED`); the view
    /// is `TYPE_3D`. Backed by the existing [`Image3D`] wrapper — one 3D image type, one 3D acquire.
    /// A re-acquire whose `extent`/`format`/`usage` all match reuses the allocation; any change
    /// rebuilds the slot (fence-safe: a slot is only touched after its frame's fence has signalled).
    pub fn acquire_image_3d(
        &mut self,
        frame: usize,
        key: &'static str,
        extent: vk::Extent3D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> crate::Result<(vk::Image, vk::ImageView)> {
        let f = &mut self.frames[frame];
        let existing = f.images_3d.iter().position(|im| im.key == key);
        if let Some(i) = existing {
            let im = &f.images_3d[i];
            if im.extent == extent && im.format == format && im.usage == usage {
                return Ok((im.image.handle(), im.image.view()));
            }
        }
        let image = Image3D::new(&self.resources, extent, format, 1, usage)?;
        let entry = TransientImage3D {
            key,
            image,
            extent,
            format,
            usage,
        };
        let i = match existing {
            Some(i) => {
                f.images_3d[i] = entry;
                i
            }
            None => {
                f.images_3d.push(entry);
                f.images_3d.len() - 1
            }
        };
        let im = &f.images_3d[i];
        Ok((im.image.handle(), im.image.view()))
    }

    /// The current grow-only capacity of a keyed buffer slot, for reclaim assertions.
    #[cfg(test)]
    fn capacity_of(&self, frame: usize, key: &'static str) -> Option<u64> {
        self.frames[frame]
            .buffers
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.capacity)
    }
}

#[cfg(test)]
mod tests {
    use ash::vk;

    use super::*;

    fn acquire(
        pool: &mut RenderGraphResources,
        frame: usize,
        key: &'static str,
        size: u64,
        usage: vk::BufferUsageFlags,
    ) -> crate::Result<vk::Buffer> {
        let mut graph = crate::RenderGraph::new();
        let resource = graph.create_buffer(
            pool,
            frame,
            key,
            RgBufferDesc {
                size,
                usage,
                lifetime: RgBufferLifetime::Transient,
            },
        )?;
        Ok(graph.buffer(resource))
    }

    /// Keyed acquire is order- and skip-independent: a key maps to its own grow-only slot, so a
    /// conditionally-present pass reordering or skipping its acquire cannot hand another key the wrong
    /// allocation (the desync the positional cursor risked). Also exercises `INDIRECT_BUFFER` flowing
    /// through `Buffer::new` and reuse on re-acquire (§2). Device-backed; skips with no GPU.
    /// One high-water frame pins a large capacity; after a full window of small-request cycles the
    /// slot is reclaimed below the session peak. Reclaim happens only in `begin_frame` (post-fence by
    /// contract), so it can never free an allocation the GPU is still reading. Device-backed; skips
    /// with no GPU.
    #[test]
    fn keyed_slot_reclaims_after_a_window_of_small_frames() {
        let device = match crate::Device::new(&crate::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let mut pool = RenderGraphResources::new(Arc::clone(device.resources()));
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER;

        let big = 4 * 1024 * 1024;
        acquire(&mut pool, 0, "vb", big, usage).expect("big");
        pool.begin_frame(0); // fold the big request into the window
        let peak = pool.capacity_of(0, "vb").unwrap();
        assert!(peak >= big);

        // A full window of small requests pushes the big request out of the ring, then the slot shrinks.
        for _ in 0..RECLAIM_WINDOW {
            acquire(&mut pool, 0, "vb", 1024, usage).expect("small");
            pool.begin_frame(0);
        }
        let after = pool.capacity_of(0, "vb").unwrap();
        assert!(
            after < peak,
            "capacity reclaimed below the session peak ({after} < {peak})"
        );
        // A re-acquire at the small size still hits the (shrunken) slot.
        let h1 = acquire(&mut pool, 0, "vb", 1024, usage).expect("reuse");
        let h2 = acquire(&mut pool, 0, "vb", 1024, usage).expect("reuse");
        assert_eq!(h1, h2, "the reclaimed slot is stable across re-acquires");
    }

    #[test]
    fn keyed_acquire_is_order_and_skip_independent() {
        let device = match crate::Device::new(&crate::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let mut pool = RenderGraphResources::new(Arc::clone(device.resources()));
        let stor = vk::BufferUsageFlags::STORAGE_BUFFER;
        let args = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::INDIRECT_BUFFER;

        let a1 = acquire(&mut pool, 0, "a", 1000, stor).expect("a");
        let b1 = acquire(&mut pool, 0, "b", 2000, args).expect("b");
        assert_ne!(a1, b1, "distinct keys are distinct allocations");

        // Reversed acquire order next cycle: keys map to their own slots, not positions.
        pool.begin_frame(0);
        let b2 = acquire(&mut pool, 0, "b", 2000, args).expect("b");
        let a2 = acquire(&mut pool, 0, "a", 1000, stor).expect("a");
        assert_eq!(
            a1, a2,
            "same key, same allocation regardless of acquire order"
        );
        assert_eq!(
            b1, b2,
            "same key, same allocation regardless of acquire order"
        );

        // Skip "b" entirely this cycle: "a" is still stable (a skipped consumer cannot desync).
        pool.begin_frame(0);
        let a3 = acquire(&mut pool, 0, "a", 1000, stor).expect("a");
        assert_eq!(a1, a3, "skipping b does not desync a");

        // The INDIRECT_BUFFER-usage buffer reuses its allocation on a same-key re-acquire.
        let b3 = acquire(&mut pool, 0, "b", 2000, args).expect("b");
        assert_eq!(
            b1, b3,
            "indirect-args buffer reuses on re-acquire of equal size + usage"
        );
    }

    #[test]
    fn persistent_buffer_growth_retires_the_old_allocation_until_all_fences_pass() {
        let device = match crate::Device::new(&crate::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let mut pool = RenderGraphResources::new(Arc::clone(device.resources()));
        let desc = |size| RgBufferDesc {
            size,
            usage: vk::BufferUsageFlags::STORAGE_BUFFER,
            lifetime: RgBufferLifetime::Persistent,
        };
        let mut first_graph = crate::RenderGraph::new();
        let first = first_graph
            .create_buffer(&mut pool, 0, "persistent", desc(1024))
            .expect("first persistent buffer");
        let first = first_graph.buffer(first);

        let mut reuse_graph = crate::RenderGraph::new();
        let reused = reuse_graph
            .create_buffer(&mut pool, 1, "persistent", desc(1024))
            .expect("reuse persistent buffer");
        assert_eq!(first, reuse_graph.buffer(reused));

        let mut growth_graph = crate::RenderGraph::new();
        let grown = growth_graph
            .create_buffer(&mut pool, 0, "persistent", desc(256 * 1024))
            .expect("grow persistent buffer");
        assert_ne!(first, growth_graph.buffer(grown));
        assert_eq!(pool.retired_persistent_buffers.len(), 1);

        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            pool.begin_frame(frame);
        }
        assert!(pool.retired_persistent_buffers.is_empty());
        device.wait_idle().expect("idle before teardown");
    }

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
