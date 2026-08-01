//! GPU hang watchdog: names the submission that is still in flight, while it is still in flight.
//!
//! Each submission registers itself before it waits and unregisters after, and a background thread
//! reports whatever has been registered too long. The thread sleeps rather than waiting on GPU
//! work, so it keeps reporting even when every other thread is blocked on a fence that will never
//! signal. Registration allocates nothing and touches no growable container — a `&'static str`
//! label plus a serial into a fixed slot table — so it is always on, shipped builds included.
//!
//! Once a report is due the thread also asks the device for its post-loss diagnostics — the
//! per-pass checkpoint each queue last reached and the driver's fault report — so a wedged
//! submission names its render-graph pass. Those queries are legal only while the device is in the
//! lost state, so the dump is gated on the loss having happened; until then the report is the name
//! and the age alone.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use crate::device::Device;

/// How long a submission may be in flight before the watchdog names it. Above any legitimate
/// frame or upload, below the driver's own command-buffer timeout, so the name lands first.
const HANG_THRESHOLD: Duration = Duration::from_secs(3);

/// How often the watchdog looks.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Concurrently tracked submissions. A frame and the uploads around it are a handful; a
/// registration beyond this is simply untracked rather than allocating.
const SLOTS: usize = 16;

#[derive(Clone, Copy)]
struct Entry {
    /// Registration identity, so a slot reused between polls cannot be reported twice.
    id: u64,
    label: &'static str,
    serial: u64,
    since: Instant,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Mutex<[Option<Entry>; SLOTS]>> = OnceLock::new();
static WATCHDOG: OnceLock<()> = OnceLock::new();
static DEVICE: OnceLock<Mutex<Weak<Device>>> = OnceLock::new();
static DEVICE_LOST: AtomicBool = AtomicBool::new(false);

fn registry() -> &'static Mutex<[Option<Entry>; SLOTS]> {
    REGISTRY.get_or_init(|| Mutex::new([None; SLOTS]))
}

fn device_slot() -> &'static Mutex<Weak<Device>> {
    DEVICE.get_or_init(|| Mutex::new(Weak::new()))
}

/// Registers the device a hang report queries for diagnostics. Held weakly, so the watchdog never
/// keeps a torn-down device alive; a later device replaces the registration.
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

/// Registers `label`/`serial` as in flight until the returned guard drops, and starts the watchdog
/// thread the first time it is called.
///
/// `label` is a static name (a one-off's label, or `"frame"`); `serial` distinguishes repeats of
/// the same label and is omitted from the report when zero. Nothing is formatted until a report is
/// actually due.
pub fn watch(label: &'static str, serial: u64) -> InFlight {
    start();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let Ok(mut slots) = registry().lock() else {
        return InFlight {
            slot: usize::MAX,
            id,
        };
    };
    let free = slots.iter().position(Option::is_none);
    match free {
        Some(slot) => {
            slots[slot] = Some(Entry {
                id,
                label,
                serial,
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
                        if entry.serial == 0 {
                            tracing::error!(
                                "GPU submission '{}' has been in flight {seconds}s — a hang, not \
                                 a slow frame",
                                entry.label
                            );
                        } else {
                            tracing::error!(
                                "GPU submission '{} {}' has been in flight {seconds}s — a hang, \
                                 not a slow frame",
                                entry.label,
                                entry.serial
                            );
                        }
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

/// Dumps the registered device's post-loss diagnostics, naming the pass each queue wedged in.
/// Reports whether it dumped: it cannot while the device is merely hung, so the caller retries on
/// the next poll instead of treating one attempt as the answer.
fn report_device_diagnostics() -> bool {
    let Some(device) = device_slot().lock().ok().and_then(|slot| slot.upgrade()) else {
        return false;
    };
    device.log_hang_diagnostics()
}
