//! GPU hang watchdog: names the submission that is still in flight, while it is still in flight.
//!
//! Each submission registers itself before it waits and unregisters after, and a background thread
//! reports whatever has been registered too long. The thread sleeps rather than waiting on GPU
//! work, so it keeps reporting even when every other thread is blocked on a fence that will never
//! signal. Registration allocates nothing and touches no growable container — a `&'static str`
//! label or a frame-ring slot index into a fixed slot table — so it is always on, shipped builds
//! included.
//!
//! A wedged frame names more than its own age. Every frame publishes the timeline points its
//! render-graph batches reserved into a fixed per-slot record built entirely from atomics, and the
//! watchdog reads that record back and asks the device for each timeline's current counter.
//! `vkGetSemaphoreCounterValue` is legal at any time, needs no external synchronization, and never
//! blocks, so the frontier — the first batch whose point has not signalled — is nameable while the
//! hang is still only a hang.
//!
//! The record is a seqlock over atomics rather than a shared lock: the render thread publishes
//! without ever holding something the watchdog could be stuck behind, and a read torn by a
//! concurrent publish is discarded instead of being a data race.
//!
//! Richer diagnostics come second. The per-pass checkpoint each queue reached and the driver's
//! fault report read valid data only while the device is in the lost state
//! (`VUID-vkGetQueueCheckpointDataNV-queue-02025`, `VUID-vkGetDeviceFaultInfoEXT-device-07336`), so
//! that dump stays gated on the loss having happened.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use ash::vk;
use ash::vk::Handle;

use crate::device::Device;
use crate::frame::MAX_FRAMES_IN_FLIGHT;

/// How long a submission may be in flight before the watchdog names it. Above any legitimate
/// frame or upload, below the driver's own command-buffer timeout, so the name lands first.
const HANG_THRESHOLD: Duration = Duration::from_secs(3);

/// How often the watchdog looks.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Concurrently tracked submissions. A frame and the uploads around it are a handful; a
/// registration beyond this is simply untracked rather than allocating.
const SLOTS: usize = 16;

/// Render-graph batches a frame slot's record holds. A frame submits one batch per contiguous
/// same-queue run of passes; a run past this is counted but not named.
const MAX_BATCHES: usize = 64;

/// 64-bit words of inline UTF-8 label per batch.
const LABEL_WORDS: usize = 8;

/// Bytes of inline UTF-8 label per batch.
const LABEL_BYTES: usize = LABEL_WORDS * 8;

/// Distinct timelines whose counters one report caches. A frame slot has a graphics timeline and
/// at most one async-compute timeline; the spare entries cost nothing.
const TIMELINE_CACHE: usize = 4;

/// Seqlock read attempts before a report gives up on a record the render thread keeps rewriting.
const SNAPSHOT_ATTEMPTS: usize = 16;

/// What a registration is waiting on.
#[derive(Clone, Copy)]
enum Watched {
    /// A frame-ring slot's in-flight fence. The slot's published record names the wedged batch.
    FrameSlot(usize),
    /// A one-off submission outside the frame ring, named by its static label.
    OneOff(&'static str),
}

#[derive(Clone, Copy)]
struct Entry {
    /// Registration identity, so a slot reused between polls cannot be reported twice.
    id: u64,
    watched: Watched,
    since: Instant,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Mutex<[Option<Entry>; SLOTS]>> = OnceLock::new();
static WATCHDOG: OnceLock<()> = OnceLock::new();
static DEVICE: OnceLock<Mutex<Weak<Device>>> = OnceLock::new();
static DEVICE_LOST: AtomicBool = AtomicBool::new(false);
static FRAMES: [PublishedSlot; MAX_FRAMES_IN_FLIGHT] =
    [const { PublishedSlot::new() }; MAX_FRAMES_IN_FLIGHT];

fn registry() -> &'static Mutex<[Option<Entry>; SLOTS]> {
    REGISTRY.get_or_init(|| Mutex::new([None; SLOTS]))
}

fn device_slot() -> &'static Mutex<Weak<Device>> {
    DEVICE.get_or_init(|| Mutex::new(Weak::new()))
}

/// Registers the device a hang report queries for counters and diagnostics. Held weakly, so the
/// watchdog never keeps a torn-down device alive; a later device replaces the registration.
pub(crate) fn attach_device(device: &Arc<Device>) {
    if let Ok(mut slot) = device_slot().lock() {
        *slot = Arc::downgrade(device);
    }
}

/// Notes that a Vulkan call reported `VK_ERROR_DEVICE_LOST`, so a hang report can dump the
/// post-loss diagnostics even when the thread that observed the loss is not the wedged one.
pub(crate) fn note_device_lost() {
    DEVICE_LOST.store(true, Ordering::Relaxed);
}

/// Whether any Vulkan call has reported `VK_ERROR_DEVICE_LOST` in this process.
pub(crate) fn device_lost_observed() -> bool {
    DEVICE_LOST.load(Ordering::Relaxed)
}

/// One render-graph batch's reserved timeline point, published as plain atomics.
struct PublishedBatch {
    semaphore: AtomicU64,
    value: AtomicU64,
    label: [AtomicU64; LABEL_WORDS],
}

impl PublishedBatch {
    const fn new() -> Self {
        Self {
            semaphore: AtomicU64::new(0),
            value: AtomicU64::new(0),
            label: [const { AtomicU64::new(0) }; LABEL_WORDS],
        }
    }

    fn store(&self, batch: &SubmittedBatch<'_>) {
        self.semaphore
            .store(batch.semaphore.as_raw(), Ordering::Relaxed);
        self.value.store(batch.value, Ordering::Relaxed);
        // Every word is written, so a shorter label cannot leave a longer one's tail behind.
        let bytes = batch.label.as_bytes();
        let mut len = bytes.len().min(LABEL_BYTES);
        while len > 0 && !batch.label.is_char_boundary(len) {
            len -= 1;
        }
        for (index, word) in self.label.iter().enumerate() {
            let start = index * 8;
            let mut chunk = [0_u8; 8];
            if start < len {
                let end = (start + 8).min(len);
                chunk[..end - start].copy_from_slice(&bytes[start..end]);
            }
            word.store(u64::from_le_bytes(chunk), Ordering::Relaxed);
        }
    }

    fn load(&self) -> BatchPoint {
        let mut label = [0_u8; LABEL_BYTES];
        for (index, word) in self.label.iter().enumerate() {
            let start = index * 8;
            label[start..start + 8].copy_from_slice(&word.load(Ordering::Relaxed).to_le_bytes());
        }
        BatchPoint {
            semaphore: self.semaphore.load(Ordering::Relaxed),
            value: self.value.load(Ordering::Relaxed),
            label,
        }
    }
}

/// One frame-ring slot's last submission, published under a seqlock: `seq` is odd while the render
/// thread rewrites the record and even once it is stable again.
struct PublishedSlot {
    seq: AtomicU64,
    /// The device the semaphores below belong to. A report queries only when it matches the
    /// attached device, so a handle from another renderer in the same process is never resolved.
    device: AtomicU64,
    serial: AtomicU64,
    /// Batches the frame submitted, which may exceed the `batches` the record holds.
    total: AtomicU32,
    batches: [PublishedBatch; MAX_BATCHES],
}

impl PublishedSlot {
    const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            device: AtomicU64::new(0),
            serial: AtomicU64::new(0),
            total: AtomicU32::new(0),
            batches: [const { PublishedBatch::new() }; MAX_BATCHES],
        }
    }

    fn publish(&self, device: vk::Device, serial: u64, batches: &[SubmittedBatch<'_>]) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        self.device.store(device.as_raw(), Ordering::Relaxed);
        self.serial.store(serial, Ordering::Relaxed);
        self.total.store(
            u32::try_from(batches.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        for (published, batch) in self.batches.iter().zip(batches) {
            published.store(batch);
        }
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Reads the record back, retrying while a publish is in progress. `None` once the retries run
    /// out — a record being rewritten this hard is not the one that is wedged.
    fn snapshot(&self) -> Option<FrameSubmission> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let before = self.seq.load(Ordering::Acquire);
            if !before.is_multiple_of(2) {
                continue;
            }
            let device = self.device.load(Ordering::Relaxed);
            let serial = self.serial.load(Ordering::Relaxed);
            let total = self.total.load(Ordering::Relaxed) as usize;
            let count = total.min(MAX_BATCHES);
            let mut batches = [BatchPoint::EMPTY; MAX_BATCHES];
            for (point, published) in batches[..count].iter_mut().zip(&self.batches) {
                *point = published.load();
            }
            // The payload above is read relaxed, so nothing but this fence keeps those loads from
            // sinking past the validating load and matching a sequence they did not come from.
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some(FrameSubmission {
                    device,
                    serial,
                    total,
                    count,
                    batches,
                });
            }
        }
        None
    }
}

/// One batch of a frame's submission, as the frame loop publishes it.
pub(crate) struct SubmittedBatch<'a> {
    /// What the batch covers — the passes recorded into it, or the submission's own name.
    pub(crate) label: &'a str,
    pub(crate) semaphore: vk::Semaphore,
    pub(crate) value: u64,
}

/// One render-graph batch's reserved timeline point, as the watchdog reads it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BatchPoint {
    semaphore: u64,
    value: u64,
    label: [u8; LABEL_BYTES],
}

impl BatchPoint {
    const EMPTY: Self = Self {
        semaphore: 0,
        value: 0,
        label: [0; LABEL_BYTES],
    };

    /// The inline label, trimmed at its NUL padding.
    fn label(&self) -> std::borrow::Cow<'_, str> {
        let end = self
            .label
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(LABEL_BYTES);
        String::from_utf8_lossy(&self.label[..end])
    }
}

/// A frame-ring slot's last submission, decoded from its published record.
struct FrameSubmission {
    device: u64,
    serial: u64,
    /// Batches the frame submitted; larger than `count` when the record truncated the run.
    total: usize,
    count: usize,
    batches: [BatchPoint; MAX_BATCHES],
}

impl FrameSubmission {
    fn batches(&self) -> &[BatchPoint] {
        &self.batches[..self.count]
    }
}

/// Publishes the timeline points frame `serial` reserved on `device`'s ring slot `slot`, in
/// submission order. The watchdog reads this back to name the batch a wedged slot is stuck in.
pub(crate) fn publish_frame(
    slot: usize,
    device: vk::Device,
    serial: u64,
    batches: &[SubmittedBatch<'_>],
) {
    if let Some(published) = FRAMES.get(slot) {
        published.publish(device, serial, batches);
    }
}

/// Drops every published record, before the frame ring's semaphores are destroyed — a stale handle
/// must never reach `vkGetSemaphoreCounterValue`.
pub(crate) fn clear_frames() {
    for published in &FRAMES {
        published.publish(vk::Device::null(), 0, &[]);
    }
}

/// A registered in-flight submission. Dropping it reports completion.
pub struct InFlight {
    slot: usize,
    id: u64,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.slot == usize::MAX {
            return;
        }
        if let Ok(mut slots) = registry().lock()
            && slots[self.slot].is_some_and(|entry| entry.id == self.id)
        {
            slots[self.slot] = None;
        }
    }
}

/// Registers a one-off submission as in flight until the returned guard drops, named by `label`.
pub(crate) fn watch(label: &'static str) -> InFlight {
    register(Watched::OneOff(label))
}

/// Registers a frame-ring slot's fence wait as in flight until the returned guard drops. A report
/// reads the slot's published record, so it names the batch the GPU is inside rather than the age
/// alone.
pub(crate) fn watch_frame(slot: usize) -> InFlight {
    register(Watched::FrameSlot(slot))
}

/// Takes a registry slot for `watched` and starts the watchdog thread the first time it is called.
/// Nothing is formatted until a report is actually due.
fn register(watched: Watched) -> InFlight {
    start();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let Ok(mut slots) = registry().lock() else {
        return InFlight {
            slot: usize::MAX,
            id,
        };
    };
    match slots.iter().position(Option::is_none) {
        Some(slot) => {
            slots[slot] = Some(Entry {
                id,
                watched,
                since: Instant::now(),
            });
            InFlight { slot, id }
        }
        // Every slot is busy: skip tracking rather than allocate or block a submission.
        None => InFlight {
            slot: usize::MAX,
            id,
        },
    }
}

/// Starts the single watchdog thread. Idempotent.
fn start() {
    WATCHDOG.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("gpu-hang-watchdog".to_owned())
            .spawn(|| {
                // The last whole second reported per slot-registration, so a hang produces a
                // readable one-line-per-second cadence rather than a flood.
                let mut reported: [(u64, u64); SLOTS] = [(0, 0); SLOTS];
                // The registration whose device diagnostics were already dumped, so a wedged
                // submission names its pass once and then only restates its age.
                let mut dumped: [u64; SLOTS] = [0; SLOTS];
                loop {
                    std::thread::sleep(POLL_INTERVAL);
                    let Ok(slots) = registry().lock() else {
                        continue;
                    };
                    let now = Instant::now();
                    let snapshot = *slots;
                    drop(slots);
                    for (index, entry) in snapshot.iter().enumerate() {
                        let Some(entry) = entry else {
                            continue;
                        };
                        let elapsed = now.saturating_duration_since(entry.since);
                        if elapsed < HANG_THRESHOLD {
                            continue;
                        }
                        let seconds = elapsed.as_secs();
                        if reported[index] == (entry.id, seconds) {
                            continue;
                        }
                        reported[index] = (entry.id, seconds);
                        report(entry.watched, seconds);
                        if dumped[index] != entry.id && report_device_diagnostics() {
                            dumped[index] = entry.id;
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("gpu hang watchdog could not start: {error}");
        }
    });
}

/// Emits one hang line, naming the wedged batch when the registration is a frame slot whose
/// published record and timeline counters can be read.
fn report(watched: Watched, seconds: u64) {
    match watched {
        Watched::OneOff(label) => tracing::error!(
            "GPU submission '{label}' has been in flight {seconds}s — a hang, not a slow frame"
        ),
        Watched::FrameSlot(slot) => match frame_wedge(slot) {
            Some(detail) => {
                tracing::error!(
                    "GPU frame ring slot {slot} has been in flight {seconds}s — {detail}"
                );
            }
            None => tracing::error!(
                "GPU frame ring slot {slot} has been in flight {seconds}s — a hang, not a slow frame"
            ),
        },
    }
}

/// Describes the wedge on frame ring slot `slot` from its published record and the device's
/// current timeline counters. `None` when nothing is published, the device is gone, or a counter
/// read failed — the caller then reports the age alone.
fn frame_wedge(slot: usize) -> Option<String> {
    let submission = FRAMES.get(slot)?.snapshot()?;
    let batches = submission.batches();
    if batches.is_empty() {
        return None;
    }
    let device = device_slot().lock().ok().and_then(|slot| slot.upgrade())?;
    // The record's semaphores belong to whichever device published them; resolving them against a
    // different renderer's device would be a handle from another address space.
    if device.raw().handle().as_raw() != submission.device {
        return None;
    }
    let mut counters = [0_u64; MAX_BATCHES];
    let mut cache = [(0_u64, 0_u64); TIMELINE_CACHE];
    let mut cached = 0_usize;
    for (counter, batch) in counters[..batches.len()].iter_mut().zip(batches) {
        *counter = match cache[..cached]
            .iter()
            .find(|(semaphore, _)| *semaphore == batch.semaphore)
        {
            Some((_, value)) => *value,
            None => {
                let value = device.timeline_counter(vk::Semaphore::from_raw(batch.semaphore))?;
                if cached < TIMELINE_CACHE {
                    cache[cached] = (batch.semaphore, value);
                    cached += 1;
                }
                value
            }
        };
    }
    let counters = &counters[..batches.len()];
    let serial = submission.serial;
    let total = submission.total;
    Some(match wedged_batch(batches, counters) {
        Some(index) => format!(
            "frame {serial} is wedged in render-graph batch {}/{total} '{}', whose timeline point \
             {} has not signalled (counter {})",
            index + 1,
            batches[index].label(),
            batches[index].value,
            counters[index]
        ),
        None if total > submission.count => format!(
            "frame {serial} signalled the first {} of its {total} render-graph batch points, so \
             the wedge is in a batch past the published record",
            submission.count
        ),
        None => format!(
            "frame {serial} signalled all {total} of its render-graph batch points, so its work \
             completed and only the fence has not"
        ),
    })
}

/// The index of the first batch whose reserved timeline point has not signalled — the frontier of
/// the frame's submission, so the wedge is inside that batch. Points are reserved and signalled in
/// submission order per queue, so an unsignalled point means that batch has not retired.
/// `counters[i]` is the current value of `batches[i]`'s timeline.
fn wedged_batch(batches: &[BatchPoint], counters: &[u64]) -> Option<usize> {
    if batches.len() != counters.len() {
        return None;
    }
    batches
        .iter()
        .zip(counters)
        .position(|(batch, counter)| *counter < batch.value)
}

/// Dumps the registered device's post-loss diagnostics, naming the pass each queue wedged in.
/// Reports whether it dumped: it cannot while the device is merely hung, so the caller retries on
/// the next poll instead of treating one attempt as the answer.
fn report_device_diagnostics() -> bool {
    let Some(device) = device_slot().lock().ok().and_then(|slot| slot.upgrade()) else {
        return false;
    };
    device.log_hang_diagnostics()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(label: &str, semaphore: u64, value: u64) -> BatchPoint {
        let published = PublishedBatch::new();
        published.store(&SubmittedBatch {
            label,
            semaphore: vk::Semaphore::from_raw(semaphore),
            value,
        });
        published.load()
    }

    #[test]
    fn wedged_batch_names_the_first_unsignalled_point() {
        let batches = [
            point("scene-prefix", 1, 1),
            point("gbuffer…ssgi", 1, 2),
            point("wind-deform", 2, 1),
            point("shadow-depth…tonemap", 1, 3),
        ];
        // Graphics reached 2 and compute reached 0: the compute batch is the frontier even though
        // a later graphics batch also owes a point.
        let counters = [2, 2, 0, 2];
        let index = wedged_batch(&batches, &counters).expect("a stuck sequence names a batch");
        assert_eq!(index, 2);
        assert_eq!(batches[index].label(), "wind-deform");
    }

    #[test]
    fn wedged_batch_names_nothing_once_every_point_signalled() {
        let batches = [
            point("scene-prefix", 1, 7),
            point("gbuffer", 1, 8),
            point("wind-deform", 2, 4),
        ];
        let counters = [8, 8, 4];
        assert_eq!(wedged_batch(&batches, &counters), None);
    }

    #[test]
    fn wedged_batch_names_nothing_without_a_counter_per_batch() {
        let batches = [point("gbuffer", 1, 2), point("tonemap", 1, 3)];
        assert_eq!(wedged_batch(&batches, &[1]), None);
    }

    #[test]
    fn published_record_round_trips_through_the_seqlock() {
        let published = PublishedSlot::new();
        published.publish(
            vk::Device::null(),
            41,
            &[
                SubmittedBatch {
                    label: "scene-prefix",
                    semaphore: vk::Semaphore::from_raw(9),
                    value: 1,
                },
                SubmittedBatch {
                    label: "gbuffer…tonemap",
                    semaphore: vk::Semaphore::from_raw(9),
                    value: 2,
                },
            ],
        );
        let snapshot = published.snapshot().expect("a stable record reads back");
        assert_eq!(snapshot.serial, 41);
        assert_eq!(snapshot.total, 2);
        let batches = snapshot.batches();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].label(), "scene-prefix");
        assert_eq!(batches[1].label(), "gbuffer…tonemap");
        assert_eq!(batches[1].semaphore, 9);
        assert_eq!(batches[1].value, 2);
    }

    #[test]
    fn a_republished_record_leaves_no_tail_of_the_longer_label() {
        let published = PublishedSlot::new();
        published.publish(
            vk::Device::null(),
            1,
            &[SubmittedBatch {
                label: "transparent-keys…transparent-scatter",
                semaphore: vk::Semaphore::from_raw(3),
                value: 1,
            }],
        );
        published.publish(
            vk::Device::null(),
            2,
            &[SubmittedBatch {
                label: "tonemap",
                semaphore: vk::Semaphore::from_raw(3),
                value: 2,
            }],
        );
        let snapshot = published.snapshot().expect("a stable record reads back");
        assert_eq!(snapshot.batches()[0].label(), "tonemap");
    }

    #[test]
    fn an_oversized_label_truncates_on_a_character_boundary() {
        let label = "…".repeat(LABEL_BYTES);
        let stored = point(&label, 1, 1);
        let read = stored.label();
        assert!(read.len() <= LABEL_BYTES);
        assert_eq!(read.chars().count(), LABEL_BYTES / "…".len());
        assert!(read.chars().all(|character| character == '…'));
    }

    /// The whole reason the record is a seqlock: the render thread republishes it every frame
    /// while the watchdog reads it, and a read that straddles a publish must be rejected rather
    /// than reported as a frame that submitted half one thing and half another.
    #[test]
    fn a_snapshot_never_mixes_two_publishes() {
        let slot = Arc::new(PublishedSlot::new());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let slot = Arc::clone(&slot);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut serial = 0_u64;
                while !stop.load(Ordering::Relaxed) {
                    serial = 3 - serial.max(1);
                    let label = if serial == 1 {
                        "scene-prefix"
                    } else {
                        "tonemap"
                    };
                    let batches: Vec<SubmittedBatch<'_>> = (0..MAX_BATCHES)
                        .map(|_| SubmittedBatch {
                            label,
                            semaphore: vk::Semaphore::from_raw(serial),
                            value: serial,
                        })
                        .collect();
                    slot.publish(vk::Device::null(), serial, &batches);
                }
            })
        };

        let mut stable = 0_u32;
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            let Some(snapshot) = slot.snapshot() else {
                continue;
            };
            let expected = if snapshot.serial == 1 {
                "scene-prefix"
            } else {
                "tonemap"
            };
            for batch in snapshot.batches() {
                assert_eq!(
                    batch.label(),
                    expected,
                    "a snapshot at serial {} carried another publish's label",
                    snapshot.serial
                );
                assert_eq!(batch.value, snapshot.serial);
                assert_eq!(batch.semaphore, snapshot.serial);
            }
            stable += 1;
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().expect("the publishing thread finishes");
        assert!(stable > 0, "the reader never observed a stable record");
    }

    #[test]
    fn a_record_holds_the_first_batches_and_counts_the_rest() {
        let labels: Vec<String> = (0..MAX_BATCHES + 3)
            .map(|index| format!("p{index}"))
            .collect();
        let batches: Vec<SubmittedBatch<'_>> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| SubmittedBatch {
                label,
                semaphore: vk::Semaphore::from_raw(1),
                value: index as u64 + 1,
            })
            .collect();
        let published = PublishedSlot::new();
        published.publish(vk::Device::null(), 5, &batches);
        let snapshot = published.snapshot().expect("a stable record reads back");
        assert_eq!(snapshot.total, MAX_BATCHES + 3);
        assert_eq!(snapshot.batches().len(), MAX_BATCHES);
    }
}
