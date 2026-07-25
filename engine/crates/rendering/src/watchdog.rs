//! GPU hang watchdog: names the submission that is still in flight, while it is still in flight.
//!
//! A timing report printed after a submission completes can never name a submission that never
//! completes. A GPU hang is exactly that case, so the elapsed-time reports around a submit are
//! silent for the one failure they matter most for. This watchdog inverts the reporting: each
//! submission registers itself before it waits and unregisters after, and a plain background
//! thread reports whatever has been registered too long. It sleeps, so it keeps reporting even
//! when every other thread is blocked on a fence that will never signal.
//!
//! It runs in every build, shipped games included: a hang in the field is precisely where no one
//! can attach a debugger, and the log line is the whole diagnosis. That only holds if the cost is
//! nil, so registration allocates nothing and touches no growable container — a `&'static str`
//! label plus a serial into a fixed slot table, under a mutex no other thread contends for.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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

fn registry() -> &'static Mutex<[Option<Entry>; SLOTS]> {
    REGISTRY.get_or_init(|| Mutex::new([None; SLOTS]))
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
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("gpu hang watchdog could not start: {error}");
        }
    });
}
