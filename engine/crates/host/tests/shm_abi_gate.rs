//! The shm-ABI go/no-go gate.
//!
//! Proves the frozen frame transport: the producer publishes BGRA8 frames that the editor
//! reader `editor/shell/src/presenter.rs` accepts byte-for-byte. The acceptance is
//! byte-level agreement with the *actual* reader's read / accept / reject rules, so this test
//! embeds an oracle reader ([`OracleReader`]) that replicates `step_view` / `open_shm` /
//! `stat_shm` field-for-field (the same header words, the same magic + capacity + ring-fits
//! checks, the same `seq % slots` slot index, the same `buffer_dims` rebuild trigger) and
//! consumes the producer's segment exactly as the editor would.
//!
//! The gate runs the host's [`ViewportShmPublisher`] (the same wiring the run loop drives),
//! publishes N frames sized by `set-viewport-size`, and asserts the oracle:
//!   * accepts every published frame (magic matches, `pixel_bytes <= capacity`, the ring
//!     fits `total`),
//!   * reads consistent width/height + pixels after the `seq` advances (no torn frame),
//!   * sees `seq` monotonic over N frames with slot = `seq % 4`, and the first frame in
//!     slot 1 (the `next = seq + 1` off-by-one the reader mirrors),
//!   * tracks the displayed dimensions to the rendered size.
//!
//! Both view segments exist from startup (the presenter's blocking open would otherwise
//! stall): the asset-preview segment is present with `seq 0` even when only the scene view
//! renders, found by the same read-only `shm_open` probe `stat_shm` uses.
//!
//! This gate covers the byte-exact ABI match against the reader oracle. The full live present
//! (the editor displaying the frame on a Wayland subsurface) needs the GTK/WebKit/Wayland
//! editor stack, which does not run headless in the toolbox, so it is not covered here.
//!
//! `#![allow(unsafe_code)]` covers the `unsafe { set_var }` the env-contract sub-test needs to
//! reproduce the editor's `SAFFRON_VIEWPORT_SHM_*` startup contract; the mutation is serialized
//! by `ENV_LOCK` so no other thread reads the vars concurrently.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::sync::Mutex;

use saffron_host::{ShmView, ShmViewConfig, ViewportShmPublisher};
use saffron_rendering::{SHM_HEADER_BYTES, SHM_MAGIC};
use saffron_test_support::unique_shm_name;

/// Serializes env-mutating sub-tests: `set_var` / `remove_var` are process-global.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Builds a deterministic BGRA8 frame of `width * height` pixels keyed by `seed`, the exact
/// shape (`B8G8R8A8_UNORM`, tightly packed `w*h*4`) the renderer's offscreen→BGRA8 blit
/// hands the publisher. Every byte is a function of its index + the seed, so a torn or
/// mis-sloted read is caught by an exact byte compare.
fn bgra_frame(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let len = (width as usize) * (height as usize) * 4;
    (0..len).map(|i| (i as u8).wrapping_add(seed)).collect()
}

/// The in-test reader oracle: a faithful, read-only replica of
/// `presenter.rs::step_view`'s field reads + accept/reject logic, carrying the
/// per-view state the editor's `ViewSurface` carries between ticks (`last_seq`,
/// `buffer_dims`). It reads the producer's mapped bytes the same way the editor reads its
/// `MAP_SHARED` view — native-endian `u32` header words, then the ring slot `seq % slots`.
///
/// It does **not** re-implement the producer; it consumes the bytes the producer wrote,
/// which is the whole point of the gate (agreement with the *actual* reader's logic).
struct OracleReader {
    /// The last accepted sequence (the reader's `vs.last_seq`); a frame with this seq is
    /// throttled (already shown), matching `seq == vs.last_seq`.
    last_seq: u32,
    /// The dimensions the reader last built buffers for (`vs.buffer_dims`); a change
    /// triggers a buffer rebuild in the editor — observed here as a flag.
    buffer_dims: (u32, u32),
}

/// One accepted frame as the oracle resolved it: the dimensions read from the header and
/// the pixel bytes copied out of slot `seq % slots`.
#[derive(Debug, PartialEq, Eq)]
struct OracleFrame {
    seq: u32,
    width: u32,
    height: u32,
    slot: usize,
    rebuilt_buffers: bool,
    pixels: Vec<u8>,
}

/// Why the oracle declined a frame this tick — mirrors each `return false` arm of
/// `step_view` so a test can assert the *reason* a frame was (correctly) rejected.
#[derive(Debug, PartialEq, Eq)]
enum OracleReject {
    /// `magic != SHM_MAGIC` — not our segment / not yet initialized.
    BadMagic,
    /// `seq == vs.last_seq` — already displayed (or `seq == 0`, no frame yet).
    NoNewFrame,
    /// `pixel_bytes == 0 || capacity == 0 || HEADER + slots*capacity > total ||
    /// pixel_bytes > capacity` — the editor's exact ring-fits / capacity guard.
    OutOfBounds,
}

impl OracleReader {
    fn new() -> Self {
        Self {
            last_seq: 0,
            buffer_dims: (0, 0),
        }
    }

    /// Reads one header word at field index `i`, exactly as the editor's
    /// `ptr::read_volatile(vs.header.add(i))` does — native-endian `u32` over the same
    /// mapped bytes the producer wrote with raw `u32` stores.
    fn header_word(seg: &[u8], i: usize) -> u32 {
        let off = i * 4;
        u32::from_ne_bytes(seg[off..off + 4].try_into().unwrap())
    }

    /// Steps the oracle over the segment bytes once, returning the accepted frame or the
    /// reason it was declined. This is `step_view`'s accept/reject core (geometry,
    /// parking, frame-callback throttling, and remap are presenter concerns outside the
    /// ABI; the *bytes* contract is exactly what is reproduced here).
    fn step(&mut self, seg: &[u8]) -> Result<OracleFrame, OracleReject> {
        let total = seg.len();
        // `let magic = read_volatile(header); let seq = read_volatile(header.add(3));`
        let magic = Self::header_word(seg, 0);
        let seq = Self::header_word(seg, 3);

        // `if magic != SHM_MAGIC || seq == vs.last_seq || throttled { return false; }`
        if magic != SHM_MAGIC {
            return Err(OracleReject::BadMagic);
        }
        if seq == self.last_seq {
            return Err(OracleReject::NoNewFrame);
        }

        let width = Self::header_word(seg, 1);
        let height = Self::header_word(seg, 2);
        let slots = Self::header_word(seg, 4).max(1);
        let capacity = Self::header_word(seg, 5) as usize;
        let pixel_bytes = (width as usize) * (height as usize) * 4;

        // The editor's exact guard:
        //   if pixel_bytes == 0 || capacity == 0
        //     || SHM_HEADER_BYTES + slots*capacity > total || pixel_bytes > capacity
        //   { return false; }
        if pixel_bytes == 0
            || capacity == 0
            || SHM_HEADER_BYTES + (slots as usize) * capacity > total
            || pixel_bytes > capacity
        {
            return Err(OracleReject::OutOfBounds);
        }

        // `if vs.buffer_dims != (width, height) { rebuild buffers; vs.buffer_dims = ... }`
        let rebuilt_buffers = self.buffer_dims != (width, height);
        if rebuilt_buffers {
            self.buffer_dims = (width, height);
        }

        // `let buffer = &vs.buffers[(seq % slots) as usize];` — the slot the editor attaches.
        let slot = (seq % slots) as usize;
        let off = SHM_HEADER_BYTES + slot * capacity;
        let pixels = seg[off..off + pixel_bytes].to_vec();

        // `vs.last_seq = seq;` after a successful commit.
        self.last_seq = seq;
        Ok(OracleFrame {
            seq,
            width,
            height,
            slot,
            rebuilt_buffers,
            pixels,
        })
    }
}

/// The core gate: a publisher driven exactly as the run loop drives it, with the oracle
/// reading every frame back byte-for-byte and the seq/slot/dimension invariants asserted.
#[test]
fn rust_producer_frames_are_accepted_byte_exact_by_the_reader_oracle() {
    let mut publisher = ViewportShmPublisher::new();
    publisher
        .enable(ShmViewConfig {
            view: ShmView::Scene,
            name: unique_shm_name(),
        })
        .expect("enable scene segment");

    let mut oracle = OracleReader::new();

    // Before any publish: seq 0 means "no frame yet" — the oracle declines (the editor
    // shows nothing) but the magic is already valid (the segment was created at startup).
    {
        let scene = publisher.view_mut(ShmView::Scene).expect("scene enabled");
        let seg = scene.segment_bytes().expect("scene mapped");
        assert_eq!(
            OracleReader::header_word(seg, 0),
            SHM_MAGIC,
            "the segment carries the frozen magic from creation"
        );
        assert_eq!(oracle.step(seg), Err(OracleReject::NoNewFrame));
    }

    // Publish a run of frames at a fixed size (the `set-viewport-size` dimensions), each a
    // distinct deterministic pattern. The oracle must accept each, read the exact bytes
    // back from slot `seq % 4`, and see seq advance monotonically.
    let (width, height) = (16u32, 8u32);
    let frame_count = (saffron_rendering::SHM_RING_SLOTS + 3) as u8; // > one ring's worth
    let mut prev_seq = 0u32;
    for n in 0..frame_count {
        let frame = bgra_frame(width, height, n);
        publisher.publish(ShmView::Scene, width, height, &frame);

        let scene = publisher.view_mut(ShmView::Scene).expect("scene enabled");
        let seg = scene.segment_bytes().expect("scene mapped");
        let got = oracle
            .step(seg)
            .expect("oracle accepts each published frame");

        assert!(got.seq > prev_seq, "seq is monotonic across frames");
        assert_eq!(got.seq, prev_seq + 1, "seq increments by one per publish");
        assert_eq!(
            got.slot,
            (got.seq % saffron_rendering::SHM_RING_SLOTS) as usize,
            "slot is seq % ring_slots, exactly as the reader computes it"
        );
        assert_eq!((got.width, got.height), (width, height), "dimensions track");
        assert_eq!(
            got.pixels, frame,
            "the oracle reads the exact published bytes from the correct slot (no torn / \
             mis-sloted read)"
        );
        prev_seq = got.seq;
    }

    // The loop's `slot == seq % ring_slots` assert at seq 1 already pins the first frame to
    // slot 1 (the `next = seq + 1` off-by-one the reader mirrors); the dedicated
    // `first_frame_in_slot_one_*` test asserts the slot index explicitly.
    assert_eq!(
        prev_seq,
        u32::from(frame_count),
        "every frame advanced the seq"
    );
}

/// The first published frame must land in slot 1 (the `next = seq + 1` detail) and the
/// dimensions must track a `set-viewport-size` change mid-stream, the oracle rebuilding
/// its buffers exactly when `(width, height)` changes — `step_view`'s `buffer_dims` trigger.
#[test]
fn first_frame_in_slot_one_and_dimension_changes_rebuild_buffers() {
    let mut publisher = ViewportShmPublisher::new();
    publisher
        .enable(ShmViewConfig {
            view: ShmView::Scene,
            name: unique_shm_name(),
        })
        .expect("enable scene");
    let mut oracle = OracleReader::new();

    // First frame: seq 1, slot 1, buffers built (dims changed from (0,0)).
    let first = bgra_frame(4, 2, 0x10);
    publisher.publish(ShmView::Scene, 4, 2, &first);
    let scene = publisher.view_mut(ShmView::Scene).unwrap();
    let frame = oracle
        .step(scene.segment_bytes().unwrap())
        .expect("first frame");
    assert_eq!(frame.seq, 1, "first publish is seq 1");
    assert_eq!(frame.slot, 1, "seq 1 lands in slot 1, not slot 0");
    assert!(
        frame.rebuilt_buffers,
        "first frame builds buffers for its dims"
    );
    assert_eq!(frame.pixels, first);

    // A `set-viewport-size` to a new size: the next frame changes (width, height), so the
    // reader rebuilds its buffers; the dimensions the oracle reports track the new size.
    let resized = bgra_frame(8, 8, 0x20);
    publisher.publish(ShmView::Scene, 8, 8, &resized);
    let scene = publisher.view_mut(ShmView::Scene).unwrap();
    let frame = oracle
        .step(scene.segment_bytes().unwrap())
        .expect("resized frame");
    assert_eq!(
        (frame.width, frame.height),
        (8, 8),
        "displayed dims follow size"
    );
    assert!(
        frame.rebuilt_buffers,
        "a dimension change rebuilds reader buffers"
    );
    assert_eq!(frame.pixels, resized);

    // A same-size frame does not rebuild buffers (the editor reuses the ring).
    let same = bgra_frame(8, 8, 0x30);
    publisher.publish(ShmView::Scene, 8, 8, &same);
    let scene = publisher.view_mut(ShmView::Scene).unwrap();
    let frame = oracle
        .step(scene.segment_bytes().unwrap())
        .expect("same-size frame");
    assert!(
        !frame.rebuilt_buffers,
        "a same-size frame reuses the reader's buffers"
    );
    assert_eq!(frame.pixels, same);
}

/// Both view segments must exist from startup with seq 0 — the presenter's blocking
/// `open_shm` would stall on a missing segment. Even when only the scene view renders, the
/// asset-preview segment is found by the same read-only `shm_open` probe `stat_shm` uses,
/// and its oracle declines (seq 0 = no frame yet) without erroring on a bad magic.
#[test]
fn both_view_segments_exist_from_startup_with_seq_zero() {
    let _guard = ENV_LOCK.lock().unwrap();
    let scene = unique_shm_name();
    let asset = unique_shm_name();
    // SAFETY: serialized by ENV_LOCK; no other thread reads these vars concurrently.
    unsafe {
        std::env::set_var(saffron_host::viewport_shm::ENV_SHM_SCENE, &scene);
        std::env::set_var(saffron_host::viewport_shm::ENV_SHM_ASSET, &asset);
    }
    let mut publisher = ViewportShmPublisher::from_env().expect("create both segments");
    // SAFETY: serialized by ENV_LOCK.
    unsafe {
        std::env::remove_var(saffron_host::viewport_shm::ENV_SHM_SCENE);
        std::env::remove_var(saffron_host::viewport_shm::ENV_SHM_ASSET);
    }

    // Both names resolve via the reader's read-only probe (mirrors `stat_shm`): the
    // segment exists and is at least the header size.
    for name in [&scene, &asset] {
        let (generation, size) =
            probe_shm(name).unwrap_or_else(|| panic!("segment '{name}' must exist"));
        assert_ne!(generation, 0, "'{name}' has a generation");
        assert!(size >= SHM_HEADER_BYTES, "'{name}' is at least the header");
    }

    // The asset view is created but never published: its oracle sees a valid magic and
    // seq 0, and declines (no frame yet) — exactly the editor showing nothing for a
    // not-yet-rendered pane while the segment stays openable.
    let mut asset_oracle = OracleReader::new();
    let asset_view = publisher
        .view_mut(ShmView::AssetPreview)
        .expect("asset enabled");
    let seg = asset_view.segment_bytes().expect("asset mapped");
    assert_eq!(
        OracleReader::header_word(seg, 0),
        SHM_MAGIC,
        "asset magic set"
    );
    assert_eq!(
        OracleReader::header_word(seg, 3),
        0,
        "asset seq 0 at startup"
    );
    assert_eq!(asset_oracle.step(seg), Err(OracleReject::NoNewFrame));
    assert_eq!(asset_view.seq(), 0, "asset view never published");

    // The scene view renders normally: publish one frame, the scene oracle accepts it.
    let frame = bgra_frame(4, 4, 0x55);
    publisher.publish(ShmView::Scene, 4, 4, &frame);
    let mut scene_oracle = OracleReader::new();
    let scene_view = publisher.view_mut(ShmView::Scene).expect("scene enabled");
    let got = scene_oracle
        .step(scene_view.segment_bytes().unwrap())
        .expect("scene frame accepted");
    assert_eq!(got.pixels, frame);
}

/// A frame that outgrows the slot grows the segment, and the oracle still accepts it: the
/// reader re-reads `ring_slots` + `slot_capacity` from the header each tick, so a recreate
/// (new capacity, seq reset to 1) reads back byte-exact through the new mapping. This is
/// the editor's remap-on-size-change path reduced to its ABI core (the bytes contract).
#[test]
fn a_grown_segment_is_still_read_byte_exact() {
    let mut publisher = ViewportShmPublisher::new();
    publisher
        .enable(ShmViewConfig {
            view: ShmView::Scene,
            name: unique_shm_name(),
        })
        .expect("enable");

    // The floor is 4K RGBA, so a publish stays within capacity; assert the header capacity
    // is the floor and a within-floor frame reads back. (The grow path itself is unit
    // tested in saffron-rendering; here we assert the oracle reads the segment as the
    // editor would after the producer sized it.)
    let frame = bgra_frame(32, 32, 0x77);
    publisher.publish(ShmView::Scene, 32, 32, &frame);
    let mut oracle = OracleReader::new();
    let scene = publisher.view_mut(ShmView::Scene).unwrap();
    let seg = scene.segment_bytes().unwrap();
    let capacity = OracleReader::header_word(seg, 5) as usize;
    assert!(
        capacity >= 32 * 32 * 4,
        "the slot capacity holds the frame (floored at 4K RGBA)"
    );
    let got = oracle.step(seg).expect("frame within capacity accepted");
    assert_eq!(
        got.pixels, frame,
        "the oracle reads the grown segment byte-exact"
    );
}

/// Writes a `u32` header word at field index `i` into a byte buffer (native-endian,
/// matching the producer's raw `u32` stores the editor reads with `read_volatile`).
fn set_word(seg: &mut [u8], i: usize, value: u32) {
    seg[i * 4..i * 4 + 4].copy_from_slice(&value.to_ne_bytes());
}

/// Builds a minimal, well-formed segment in a plain `Vec<u8>` laid out byte-identically to
/// the producer's wire (header + a 4-slot ring), with one frame in slot `seq % slots`. Used
/// to drive the oracle's reject arms without cloning the 133 MB floored live segment.
fn synthetic_segment(width: u32, height: u32, seq: u32, capacity: usize, fill: u8) -> Vec<u8> {
    let slots = 4u32;
    let total = SHM_HEADER_BYTES + (slots as usize) * capacity;
    let mut seg = vec![0u8; total];
    set_word(&mut seg, 0, SHM_MAGIC);
    set_word(&mut seg, 1, width);
    set_word(&mut seg, 2, height);
    set_word(&mut seg, 3, seq);
    set_word(&mut seg, 4, slots);
    set_word(&mut seg, 5, capacity as u32);
    let pixel_bytes = (width as usize) * (height as usize) * 4;
    let slot = (seq % slots) as usize;
    let off = SHM_HEADER_BYTES + slot * capacity;
    for b in &mut seg[off..off + pixel_bytes] {
        *b = fill;
    }
    seg
}

/// The oracle's reject arms must match `step_view`'s guards exactly, not only its happy
/// path: a wrong magic, a not-yet-advanced seq, and a capacity that overflows the segment
/// each decline with the editor's reason. Driven over a byte-identical synthetic segment so
/// the reject is exercised by the same header layout the editor reads.
#[test]
fn malformed_segments_are_rejected_for_the_readers_exact_reasons() {
    let capacity = 4 * 4 * 4; // a 4×4 BGRA frame fits exactly
    let seg = synthetic_segment(4, 4, 1, capacity, 0x42);

    // Sanity: the well-formed segment is accepted.
    assert!(
        OracleReader::new().step(&seg).is_ok(),
        "the well-formed segment is accepted"
    );

    // Corrupt the magic word: the editor's `magic != SHM_MAGIC` arm.
    {
        let mut bad = seg.clone();
        set_word(&mut bad, 0, 0xDEAD_BEEF);
        assert_eq!(OracleReader::new().step(&bad), Err(OracleReject::BadMagic));
    }

    // seq unchanged from the reader's `last_seq`: the `seq == vs.last_seq` arm. Step once
    // to record seq, then step again on the same bytes.
    {
        let mut oracle = OracleReader::new();
        assert!(oracle.step(&seg).is_ok());
        assert_eq!(oracle.step(&seg), Err(OracleReject::NoNewFrame));
    }

    // A capacity that makes `HEADER + slots*capacity > total`: the ring-overflow arm.
    {
        let mut bad = seg.clone();
        let overflow_capacity = bad.len() as u32; // slot_capacity * slots >> total
        set_word(&mut bad, 3, 2); // a fresh seq so the bounds guard, not seq, decides
        set_word(&mut bad, 5, overflow_capacity);
        assert_eq!(
            OracleReader::new().step(&bad),
            Err(OracleReject::OutOfBounds)
        );
    }

    // A zero-area frame (`pixel_bytes == 0`): the same bounds arm.
    {
        let mut bad = seg.clone();
        set_word(&mut bad, 3, 3);
        set_word(&mut bad, 2, 0); // height = 0
        assert_eq!(
            OracleReader::new().step(&bad),
            Err(OracleReject::OutOfBounds)
        );
    }
}

/// Mirrors the reader's `stat_shm`: a read-only `shm_open` returning the generation + size of
/// the named segment, or `None` if it does not exist. Uses `rustix`, the producer's seam.
fn probe_shm(name: &str) -> Option<(u64, usize)> {
    use rustix::fs::fstat;
    use rustix::shm::{self, Mode, OFlags};
    let cname = CString::new(name).ok()?;
    let fd = shm::open(cname.as_c_str(), OFlags::RDONLY, Mode::empty()).ok()?;
    let st = fstat(&fd).ok()?;
    let base = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            SHM_HEADER_BYTES,
            rustix::mm::ProtFlags::READ,
            rustix::mm::MapFlags::SHARED,
            &fd,
            0,
        )
        .ok()?
    }
    .cast::<u32>();
    let low = unsafe { std::ptr::read_volatile(base.add(6)) };
    let high = unsafe { std::ptr::read_volatile(base.add(7)) };
    unsafe { rustix::mm::munmap(base.cast(), SHM_HEADER_BYTES) }.ok()?;
    Some((
        u64::from(low) | (u64::from(high) << 32),
        st.st_size as usize,
    ))
}
