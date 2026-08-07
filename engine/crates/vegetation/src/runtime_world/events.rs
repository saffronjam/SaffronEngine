//! The committed-transition ring and its cursor reads.

use crate::VegetationTransition;

use super::VegetationWorld;

/// How many committed transitions the event ring retains before evicting the oldest. A consumer
/// whose cursor falls behind that tail is told to resync rather than handed a gap.
pub const VEGETATION_EVENT_RING_CAP: usize = 4096;

/// One committed vegetation transition, sequence-stamped for cursor-based delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationEvent {
    /// Monotonic sequence number within this world.
    pub seq: u64,
    pub transition: VegetationTransition,
}

/// A cursor read of the event ring, plus the metadata a stale cursor needs to notice it missed
/// evicted events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationEventDrain {
    /// Events newer than the cursor, oldest first.
    pub events: Vec<VegetationEvent>,
    /// The newest sequence number the ring has stamped.
    pub high_water_seq: u64,
    /// The oldest sequence number still retained, or zero when the ring is empty.
    pub oldest_seq: u64,
    /// The cursor was older than the retained tail, so events were missed: resync.
    pub overflowed: bool,
}

impl VegetationWorld {
    /// Reads every committed transition with `seq > since`, oldest first, without consuming the
    /// ring — one cursor per consumer (scripts, VFX, audio, quests, navigation).
    #[must_use]
    pub fn drain_events(&self, since: u64) -> VegetationEventDrain {
        let events: Vec<VegetationEvent> = self
            .event_ring
            .iter()
            .filter(|event| event.seq > since)
            .copied()
            .collect();
        let oldest_seq = self.event_ring.front().map_or(0, |event| event.seq);
        VegetationEventDrain {
            events,
            high_water_seq: self.event_seq,
            oldest_seq,
            overflowed: oldest_seq > 0 && since + 1 < oldest_seq,
        }
    }

    pub(super) fn record_transitions(&mut self, transitions: &[VegetationTransition]) {
        for transition in transitions {
            self.event_seq += 1;
            if self.event_ring.len() >= VEGETATION_EVENT_RING_CAP {
                self.event_ring.pop_front();
            }
            self.event_ring.push_back(VegetationEvent {
                seq: self.event_seq,
                transition: *transition,
            });
        }
    }
}
