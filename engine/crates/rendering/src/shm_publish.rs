//! The viewport shm publish: the byte-exact seqlock producer the editor's Wayland
//! presenter reads.
//!
//! The segment is a **frozen wire contract**: a 32-byte header of eight `u32`s
//! `[magic, width, height, seq, ring_slots, slot_capacity, 0, 0]` followed by
//! `ring_slots` fixed-capacity BGRA8 frames. Frame `s` lands in ring slot
//! `s % ring_slots`; `seq` is bumped last under a [`Ordering::Release`] fence so a
//! reader that observes the new `seq` is guaranteed the matching width/height + pixels
//! (the seqlock). The reader oracle is `editor/shell/src/presenter.rs`,
//! which reads slot `seq % ring_slots` after checking `magic` and a `seq` change.
//!
//! # The `unsafe` seam
//!
//! This is the README §6 / phase grounding shm seam: `rustix` opens the POSIX shared
//! memory object, `ftruncate`s it, and `mmap`s it `MAP_SHARED`; the producer writes
//! the header + the ring via a raw pointer into that mapping. The seqlock ordering is
//! the load-bearing invariant — `fence(Release)` between the pixel/dimension writes
//! and the final `seq` store.

use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};
use std::ptr;
use std::sync::atomic::{Ordering, fence};

use rustix::fs::ftruncate;
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use rustix::shm::{self, Mode};

/// The segment magic, "SFV2" little-endian. Must equal the reader's `SHM_MAGIC`.
pub const SHM_MAGIC: u32 = 0x5346_5632;

/// The header size in bytes: eight `u32`s. Must equal the reader's `SHM_HEADER_BYTES`.
pub const SHM_HEADER_BYTES: usize = 32;

/// The ring depth. Frame `s` lands in slot `s % SHM_RING_SLOTS`; the reader reads
/// `seq % slots` (it re-reads `ring_slots` from the header, so this is the producer's
/// fixed choice, not a wire-pinned constant on the read side).
pub const SHM_RING_SLOTS: u32 = 4;

/// The minimum per-slot capacity (4K RGBA): floors the segment so ordinary resizes
/// never reallocate it. shm pages are sparse, so unused capacity costs nothing.
pub const MIN_SHM_SLOT_CAPACITY: usize = 3840 * 2160 * 4;

/// Pins the frozen byte layout at compile time: the header is exactly eight `u32`s (32
/// bytes) and the floor is the 4K-RGBA constant the reader assumes. A drift here is a
/// wire-break with the unchanged `presenter.rs` reader, so it must fail the build.
const _: () = {
    const HEADER_FIELDS: usize = 8;
    assert!(SHM_HEADER_BYTES == HEADER_FIELDS * size_of::<u32>());
    assert!(MIN_SHM_SLOT_CAPACITY == 3840 * 2160 * 4);
    assert!(SHM_MAGIC == 0x5346_5632);
    assert!(SHM_RING_SLOTS == 4);
};

/// The header field indices, named so the producer and the tests cannot drift from the
/// frozen `[magic, width, height, seq, ring_slots, slot_capacity, 0, 0]` order.
mod field {
    pub const MAGIC: usize = 0;
    pub const WIDTH: usize = 1;
    pub const HEIGHT: usize = 2;
    pub const SEQ: usize = 3;
    pub const RING_SLOTS: usize = 4;
    pub const SLOT_CAPACITY: usize = 5;
}

/// The producer side of one view's shm segment.
///
/// Owns the POSIX shm fd + the `MAP_SHARED` mapping and writes the seqlock. The
/// per-frame-in-flight GPU slots (the BGRA8 image + staging buffer) live on the
/// renderer's capture record because they need the device/allocator — this type is the
/// pure, GPU-free producer that owns the wire contract. [`Drop`] unmaps, closes, and
/// `shm_unlink`s the name.
#[derive(Default)]
pub struct ShmPublish {
    /// The shm object name (`shm_open` path, e.g. `/saffron-scene`). Empty until enabled.
    name: CString,
    /// The mapped segment fd + base, or `None` until [`ShmPublish::enable`] creates it.
    mapping: Option<Mapping>,
    /// Per-slot capacity in bytes; the current segment holds `SHM_RING_SLOTS` of these.
    slot_capacity: usize,
    /// The last published sequence number; `0` means no frame yet.
    seq: u32,
    /// Whether publishing is enabled for this view.
    enabled: bool,
}

/// The owned `mmap` mapping + its fd. `Drop`-free on its own (the owner's `Drop`
/// unmaps); held together so a recreate can tear down the previous mapping cleanly.
struct Mapping {
    fd: OwnedFd,
    base: *mut u8,
    size: usize,
}

// SAFETY: the mapping is a process-private `MAP_SHARED` region the producer writes
// from one thread (the render thread). The raw pointer is never aliased mutably across
// threads; `Send` is needed only so the renderer aggregate that owns this can move
// across the bring-up boundary. There is no shared `&mut` access.
unsafe impl Send for ShmPublish {}

impl ShmPublish {
    /// Enables publishing for this view under `name`, creating the segment *now*
    /// (floored at [`MIN_SHM_SLOT_CAPACITY`]) — not lazily on the first publish.
    ///
    /// The editor's presenter blocks at startup until each view's segment exists, so a
    /// view that is not rendered yet (the asset preview before it is opened) must still
    /// have its segment, or the whole present loop stalls. `seq` stays `0` until the
    /// first real frame, so the reader shows nothing for this view until then.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the segment cannot be created.
    pub fn enable(&mut self, name: &str) -> std::io::Result<()> {
        self.enabled = true;
        self.name = CString::new(name).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "shm name has a nul byte")
        })?;
        self.recreate_segment(MIN_SHM_SLOT_CAPACITY)
    }

    /// Whether this view's segment is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The current per-slot capacity in bytes (`0` until enabled).
    pub fn slot_capacity(&self) -> usize {
        self.slot_capacity
    }

    /// The last published sequence number (`0` = no frame yet).
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// (Re)creates the segment sized for `capacity` bytes per ring slot: drops any
    /// prior mapping, opens + `ftruncate`s the header + ring, `mmap`s it, and writes
    /// the header (`magic` / `ring_slots` / `slot_capacity`; `seq = 0` = no frame yet).
    ///
    /// Grow-only: the floor at [`MIN_SHM_SLOT_CAPACITY`] means ordinary resizes never
    /// recreate it. The reader remaps on inode/size change.
    fn recreate_segment(&mut self, capacity: usize) -> std::io::Result<()> {
        let total_bytes = SHM_HEADER_BYTES + (SHM_RING_SLOTS as usize) * capacity;

        // Drop the prior mapping (unmap + close) before re-opening the same name.
        self.unmap();
        let _ = shm::unlink(self.name.as_c_str());

        let fd = shm::open(
            self.name.as_c_str(),
            shm::OFlags::CREATE | shm::OFlags::RDWR,
            Mode::from(0o600),
        )?;
        ftruncate(&fd, total_bytes as u64)?;
        // SAFETY: the shm seam. `fd` is a freshly truncated shm object of `total_bytes`;
        // `mmap` over the whole length with `MAP_SHARED` read/write is the POSIX
        // contract. The returned pointer is non-null on success (`mmap` returns an error
        // otherwise, propagated by `?`).
        let base = unsafe {
            mmap(
                ptr::null_mut(),
                total_bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        }
        .cast::<u8>();

        self.slot_capacity = capacity;
        self.seq = 0;
        self.mapping = Some(Mapping {
            fd,
            base,
            size: total_bytes,
        });
        self.write_header_init(capacity);
        Ok(())
    }

    /// Writes the initial header: magic, zero width/height/seq (no frame yet), the ring
    /// depth, the per-slot capacity, and the two trailing zero words.
    fn write_header_init(&mut self, capacity: usize) {
        let header = self.header_ptr().expect("segment mapped");
        // SAFETY: `header` points at the 32-byte (8×u32) header inside the live mapping.
        unsafe {
            *header.add(field::MAGIC) = SHM_MAGIC;
            *header.add(field::WIDTH) = 0;
            *header.add(field::HEIGHT) = 0;
            *header.add(field::SEQ) = 0;
            *header.add(field::RING_SLOTS) = SHM_RING_SLOTS;
            *header.add(field::SLOT_CAPACITY) = capacity as u32;
            *header.add(6) = 0;
            *header.add(7) = 0;
        }
    }

    /// Publishes one BGRA8 frame: copies `pixels` into the next ring slot, writes the
    /// dimensions, then bumps `seq` last under a [`Ordering::Release`] fence (the
    /// seqlock). `pixels` must be tightly packed `width * height * 4` BGRA8 bytes.
    ///
    /// Grows the segment first if the frame outgrows the current slot capacity. A
    /// zero-area frame or a too-short `pixels` slice is a no-op (the producer never
    /// writes a torn frame).
    pub fn publish(&mut self, width: u32, height: u32, pixels: &[u8]) {
        let pixel_bytes = (width as usize) * (height as usize) * 4;
        if pixel_bytes == 0 || pixels.len() < pixel_bytes {
            return;
        }

        if (self.mapping.is_none() || pixel_bytes > self.slot_capacity)
            && self
                .recreate_segment(pixel_bytes.max(MIN_SHM_SLOT_CAPACITY))
                .is_err()
        {
            return;
        }

        let next = self.seq.wrapping_add(1);
        let slot = (next % SHM_RING_SLOTS) as usize;
        let mapping = self.mapping.as_ref().expect("segment mapped");
        // SAFETY: `slot < SHM_RING_SLOTS` and the segment holds `SHM_RING_SLOTS *
        // slot_capacity` bytes after the header; `pixel_bytes <= slot_capacity`, so the
        // destination slice is wholly inside the mapping. `pixels` is read-only here.
        unsafe {
            let dst = mapping
                .base
                .add(SHM_HEADER_BYTES + slot * self.slot_capacity);
            ptr::copy_nonoverlapping(pixels.as_ptr(), dst, pixel_bytes);
        }

        // Write dimensions first, then bump `seq` last under a release fence so a reader
        // that sees the new `seq` is guaranteed the matching width/height + pixels.
        let header = mapping.base.cast::<u32>();
        // SAFETY: `header` is the live mapping's header; the writes are within it.
        unsafe {
            *header.add(field::WIDTH) = width;
            *header.add(field::HEIGHT) = height;
        }
        self.seq = next;
        fence(Ordering::Release);
        // SAFETY: as above — the final `seq` store completing the seqlock.
        unsafe {
            *header.add(field::SEQ) = next;
        }
    }

    /// The header as a `*mut u32`, or `None` when no segment is mapped.
    fn header_ptr(&self) -> Option<*mut u32> {
        self.mapping.as_ref().map(|m| m.base.cast::<u32>())
    }

    /// The fd backing the current mapping (the editor presenter dups it). `None` until
    /// enabled.
    pub fn fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.mapping.as_ref().map(|m| m.fd.as_fd())
    }

    /// The mapped segment as a read-only byte slice, for in-process verification
    /// (the parity test reads it exactly as the editor's reader does).
    pub fn segment_bytes(&self) -> Option<&[u8]> {
        self.mapping.as_ref().map(|m| {
            // SAFETY: `base`/`size` describe the live `MAP_SHARED` region; the slice is
            // read-only and lives only as long as `&self`.
            unsafe { std::slice::from_raw_parts(m.base.cast_const(), m.size) }
        })
    }

    /// Unmaps + closes the current mapping (idempotent). The fd's `Drop` closes it.
    fn unmap(&mut self) {
        if let Some(mapping) = self.mapping.take() {
            // SAFETY: `base`/`size` came from the matching `mmap`; nothing aliases the
            // region after this. The fd is closed when `mapping.fd` drops below.
            unsafe {
                let _ = munmap(mapping.base.cast(), mapping.size);
            }
            drop(mapping.fd);
        }
    }
}

impl Drop for ShmPublish {
    fn drop(&mut self) {
        self.unmap();
        if self.enabled && !self.name.as_bytes().is_empty() {
            let _ = shm::unlink(self.name.as_c_str());
        }
        self.slot_capacity = 0;
        self.enabled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique shm name per test process so concurrent runs never collide.
    fn unique_name(tag: &str) -> String {
        use std::sync::atomic::AtomicU32;
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("/saffron-test-{tag}-{}-{n}", std::process::id())
    }

    /// Reads one header field by index, exactly as the editor's reader does
    /// (`ptr::read_volatile(header.add(i))`).
    fn header_word(seg: &[u8], i: usize) -> u32 {
        let off = i * 4;
        u32::from_ne_bytes(seg[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn header_is_byte_identical_to_the_frozen_layout() {
        let mut shm = ShmPublish::default();
        shm.enable(&unique_name("header")).expect("enable");
        let seg = shm.segment_bytes().expect("mapped");

        // Magic + field order + the seq=0-until-first-frame rule.
        assert_eq!(header_word(seg, field::MAGIC), SHM_MAGIC);
        assert_eq!(header_word(seg, field::WIDTH), 0, "no frame published yet");
        assert_eq!(header_word(seg, field::HEIGHT), 0);
        assert_eq!(header_word(seg, field::SEQ), 0, "seq=0 means show nothing");
        assert_eq!(header_word(seg, field::RING_SLOTS), SHM_RING_SLOTS);
        assert_eq!(
            header_word(seg, field::SLOT_CAPACITY),
            MIN_SHM_SLOT_CAPACITY as u32
        );
        assert_eq!(header_word(seg, 6), 0, "trailing reserved word");
        assert_eq!(header_word(seg, 7), 0, "trailing reserved word");

        // The total segment is header + ring_slots * capacity.
        assert_eq!(
            seg.len(),
            SHM_HEADER_BYTES + (SHM_RING_SLOTS as usize) * MIN_SHM_SLOT_CAPACITY
        );
    }

    #[test]
    fn published_frame_reads_back_consistently_via_the_reader_seqlock() {
        let mut shm = ShmPublish::default();
        shm.enable(&unique_name("publish")).expect("enable");

        let (w, h) = (4u32, 2u32);
        // A recognisable BGRA8 pattern: byte i = i as u8.
        let pixels: Vec<u8> = (0..(w * h * 4)).map(|i| i as u8).collect();
        shm.publish(w, h, &pixels);

        // Read it back exactly as `step_view` does: check magic + seq, then read the
        // slot `seq % ring_slots`.
        let seg = shm.segment_bytes().expect("mapped");
        assert_eq!(header_word(seg, field::MAGIC), SHM_MAGIC);
        let seq = header_word(seg, field::SEQ);
        assert_eq!(seq, 1, "first publish bumps seq to 1");
        assert_eq!(header_word(seg, field::WIDTH), w);
        assert_eq!(header_word(seg, field::HEIGHT), h);
        let slots = header_word(seg, field::RING_SLOTS).max(1);
        let capacity = header_word(seg, field::SLOT_CAPACITY) as usize;

        // Frame s lands in slot s % ring_slots; the reader reads seq % slots.
        let slot = (seq % slots) as usize;
        let off = SHM_HEADER_BYTES + slot * capacity;
        let frame = &seg[off..off + (w * h * 4) as usize];
        assert_eq!(frame, &pixels[..], "no torn read; bytes match the source");
    }

    #[test]
    fn seq_walks_the_ring_with_modular_slot_placement() {
        let mut shm = ShmPublish::default();
        shm.enable(&unique_name("ring")).expect("enable");
        let (w, h) = (2u32, 2u32);

        // Publish more than one ring's worth; each lands in slot (seq) % ring_slots,
        // where seq is 1-based (the first publish is seq=1, slot 1).
        for frame in 0..(SHM_RING_SLOTS + 2) {
            let pixels = vec![frame as u8; (w * h * 4) as usize];
            shm.publish(w, h, &pixels);
            let seq = shm.seq();
            assert_eq!(seq, frame + 1);

            let seg = shm.segment_bytes().unwrap();
            let slots = header_word(seg, field::RING_SLOTS);
            let capacity = header_word(seg, field::SLOT_CAPACITY) as usize;
            let slot = (seq % slots) as usize;
            let off = SHM_HEADER_BYTES + slot * capacity;
            assert_eq!(
                seg[off], frame as u8,
                "frame {frame} landed in slot {slot} = (seq {seq}) % {slots}"
            );
        }
    }

    #[test]
    fn first_frame_lands_in_slot_1_not_slot_0() {
        // `publish` computes `next = seq + 1` then writes slot `next % ring_slots`, so
        // the first published frame (seq 1) lands in slot 1, NOT slot 0 — the off-by-one
        // the reader (`seq % slots`) mirrors. A drift to slot 0 would silently misread.
        let mut shm = ShmPublish::default();
        shm.enable(&unique_name("slot1")).expect("enable");

        let (w, h) = (2u32, 2u32);
        let pixels = vec![0xABu8; (w * h * 4) as usize];
        shm.publish(w, h, &pixels);

        assert_eq!(shm.seq(), 1, "first publish bumps seq to 1");
        let seg = shm.segment_bytes().expect("mapped");
        assert_eq!(header_word(seg, field::SEQ), 1);
        let capacity = shm.slot_capacity();

        // Pixels are at HEADER + 1*capacity (slot 1), and slot 0 is untouched.
        let slot1 = SHM_HEADER_BYTES + capacity;
        let frame = &seg[slot1..slot1 + (w * h * 4) as usize];
        assert_eq!(frame, &pixels[..], "frame published into slot 1");

        let slot0 = SHM_HEADER_BYTES;
        assert!(
            seg[slot0..slot0 + (w * h * 4) as usize]
                .iter()
                .all(|&b| b == 0),
            "slot 0 stays zero on the first publish"
        );
    }

    #[test]
    fn drop_munmaps_and_unlinks_the_segment() {
        let name = unique_name("drop");
        {
            let mut shm = ShmPublish::default();
            shm.enable(&name).expect("enable");
            // The segment exists while alive: a read-only open mirroring `stat_shm` succeeds.
            assert!(open_ro_probe(&name).is_some(), "segment exists while alive");
        }
        // Dropping it shm_unlinks the name, so the reader's inode probe finds nothing.
        assert!(
            open_ro_probe(&name).is_none(),
            "drop shm_unlinks the segment (stat_shm would return None)"
        );
    }

    /// Mirrors the reader's `stat_shm`: a read-only `shm_open` that returns `Some` iff the
    /// named segment exists. Uses `rustix`, not libc, matching the producer's seam.
    fn open_ro_probe(name: &str) -> Option<OwnedFd> {
        let cname = CString::new(name).unwrap();
        shm::open(cname.as_c_str(), shm::OFlags::RDONLY, Mode::empty()).ok()
    }

    #[test]
    fn a_frame_that_outgrows_the_slot_grows_the_segment() {
        let mut shm = ShmPublish::default();
        // Start a tiny segment by enabling, then publish a frame larger than the floor
        // — except the floor is 4K, so force the grow path with an explicit recreate at
        // a small capacity first.
        shm.enable(&unique_name("grow")).expect("enable");
        shm.recreate_segment(64).expect("small segment");
        assert_eq!(shm.slot_capacity(), 64);

        let (w, h) = (8u32, 8u32); // 8*8*4 = 256 bytes > 64
        let pixels = vec![0x7fu8; (w * h * 4) as usize];
        shm.publish(w, h, &pixels);

        // Grow floors at MIN_SHM_SLOT_CAPACITY, so it jumps straight to the floor.
        assert_eq!(shm.slot_capacity(), MIN_SHM_SLOT_CAPACITY);
        // The recreate resets seq, so this publish is seq=1 again.
        assert_eq!(shm.seq(), 1);
        let seg = shm.segment_bytes().unwrap();
        assert_eq!(
            header_word(seg, field::SLOT_CAPACITY),
            MIN_SHM_SLOT_CAPACITY as u32
        );
        assert_eq!(header_word(seg, field::WIDTH), w);
    }

    #[test]
    fn zero_area_and_short_slices_are_no_ops() {
        let mut shm = ShmPublish::default();
        shm.enable(&unique_name("noop")).expect("enable");
        shm.publish(0, 0, &[]);
        assert_eq!(shm.seq(), 0, "zero-area frame never bumps seq");
        shm.publish(4, 4, &[0u8; 8]); // too short for 4*4*4
        assert_eq!(shm.seq(), 0, "short slice never bumps seq");
    }
}
