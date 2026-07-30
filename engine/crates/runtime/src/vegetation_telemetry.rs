//! Compact vegetation telemetry.
//!
//! Everything here is a counter or a duration accumulated at the fixed synchronization point. Nothing
//! reads back per-instance data and nothing walks a resident cell to answer a query; a caller wanting
//! per-instance detail asks for a capture through the inspection commands instead.
//!
//! The durations are the wall clock of one synchronization split by the stage that spent it, so a
//! frame that got slower says which stage did.

use std::time::{Duration, Instant};

/// Wall-clock time one synchronization spent, split by stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationStageTimes {
    /// Residency scheduling: which cells are wanted, which arrived, which were retired.
    pub residency: Duration,
    /// Promotion transitions committing.
    pub promotion: Duration,
    /// Collision residency deriving and batching proxy bodies.
    pub collision: Duration,
    /// The navigation seam republishing changed cells.
    pub navigation: Duration,
    /// Ecology ticks executed inside the synchronization.
    pub ecology: Duration,
}

impl VegetationStageTimes {
    /// The whole synchronization, which is the sum of its stages.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.residency + self.promotion + self.collision + self.navigation + self.ecology
    }
}

/// Counts of work the runtime did, since the world was bound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationWorkCounters {
    /// Synchronization points executed.
    pub synchronizations: u64,
    /// Spatial queries answered.
    pub queries: u64,
    /// Plants those queries returned, which is what makes a query expensive.
    pub query_hits: u64,
    /// Mutation transactions reduced.
    pub mutations: u64,
    /// Bytes those transactions carried.
    pub mutation_bytes: u64,
    /// State snapshots exported.
    pub snapshots: u64,
    /// Bytes those snapshots carried.
    pub snapshot_bytes: u64,
    /// Ecology ticks executed.
    pub ecology_ticks: u64,
}

/// The vegetation runtime's compact telemetry: stage times for the last synchronization, an
/// exponential average across them, and counters since the world was bound.
#[derive(Clone, Debug, Default)]
pub struct VegetationTelemetry {
    last: VegetationStageTimes,
    average: VegetationStageTimes,
    work: VegetationWorkCounters,
    pending: VegetationStageTimes,
    /// Completed stage spans awaiting a profiler that can accept them.
    ///
    /// Timed on `CLOCK_MONOTONIC` so they share the renderer's span epoch exactly — a capture that
    /// placed these on a second timeline would show cooking and residency floating beside the
    /// frame rather than inside it, which is worse than not showing them.
    spans: Vec<(VegetationStage, u64, u64)>,
}

/// The monotonic clock the renderer's CPU spans are stamped on.
fn monotonic_now_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

impl VegetationTelemetry {
    /// Times one stage, folding its duration into the synchronization in progress.
    pub fn stage<T>(&mut self, stage: VegetationStage, body: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let started_ns = monotonic_now_ns();
        let value = body();
        let elapsed = started.elapsed();
        self.spans
            .push((stage, started_ns, elapsed.as_nanos() as u64));
        let slot = match stage {
            VegetationStage::Residency => &mut self.pending.residency,
            VegetationStage::Promotion => &mut self.pending.promotion,
            VegetationStage::Collision => &mut self.pending.collision,
            VegetationStage::Navigation => &mut self.pending.navigation,
            VegetationStage::Ecology => &mut self.pending.ecology,
        };
        *slot += elapsed;
        value
    }

    /// Closes the synchronization in progress, publishing its stage times.
    ///
    /// The average is an eighth-weighted exponential fold rather than a window of samples, so it
    /// costs one multiply and holds no history to walk.
    pub fn commit(&mut self) {
        self.work.synchronizations += 1;
        self.last = std::mem::take(&mut self.pending);
        let fold = |average: &mut Duration, sample: Duration| {
            *average = (*average * 7 + sample) / 8;
        };
        fold(&mut self.average.residency, self.last.residency);
        fold(&mut self.average.promotion, self.last.promotion);
        fold(&mut self.average.collision, self.last.collision);
        fold(&mut self.average.navigation, self.last.navigation);
        fold(&mut self.average.ecology, self.last.ecology);
    }

    /// Takes the completed stage spans, leaving the buffer empty.
    ///
    /// Drained rather than read so a consumer that stops pumping cannot grow it without bound —
    /// the spans are only useful to a profiler that is actually capturing.
    pub fn take_spans(&mut self) -> Vec<(VegetationStage, u64, u64)> {
        std::mem::take(&mut self.spans)
    }

    /// Records one answered query and how many plants it returned.
    pub fn record_query(&mut self, hits: usize) {
        self.work.queries += 1;
        self.work.query_hits += hits as u64;
    }

    /// Records one reduced mutation transaction and the bytes it carried.
    pub fn record_mutation(&mut self, bytes: usize) {
        self.work.mutations += 1;
        self.work.mutation_bytes += bytes as u64;
    }

    /// Records one exported state snapshot and the bytes it carried.
    pub fn record_snapshot(&mut self, bytes: usize) {
        self.work.snapshots += 1;
        self.work.snapshot_bytes += bytes as u64;
    }

    /// Records executed ecology ticks.
    pub fn record_ecology_ticks(&mut self, ticks: u64) {
        self.work.ecology_ticks += ticks;
    }

    /// Stage times of the last completed synchronization.
    #[must_use]
    pub fn last(&self) -> VegetationStageTimes {
        self.last
    }

    /// Exponentially averaged stage times.
    #[must_use]
    pub fn average(&self) -> VegetationStageTimes {
        self.average
    }

    /// Counters since the world was bound.
    #[must_use]
    pub fn work(&self) -> VegetationWorkCounters {
        self.work
    }

    /// Clears everything, which is what binding a different world means.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Which synchronization stage a duration belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VegetationStage {
    /// Residency scheduling.
    Residency,
    /// Promotion transitions.
    Promotion,
    /// Collision residency.
    Collision,
    /// The navigation seam.
    Navigation,
    /// Ecology ticks.
    Ecology,
}

impl VegetationStage {
    /// The stable span name this stage appears under in a capture.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Residency => "vegetation-residency",
            Self::Promotion => "vegetation-promotion",
            Self::Collision => "vegetation-collision",
            Self::Navigation => "vegetation-navigation",
            Self::Ecology => "vegetation-ecology",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synchronization publishes what each stage spent, and the average follows the samples
    /// without holding any history.
    #[test]
    fn stage_times_publish_on_commit() {
        let mut telemetry = VegetationTelemetry::default();
        assert_eq!(telemetry.last(), VegetationStageTimes::default());

        telemetry.stage(VegetationStage::Residency, || {});
        telemetry.stage(VegetationStage::Collision, || {});
        // Nothing is published until the synchronization closes: a half-timed frame is not a frame.
        assert_eq!(telemetry.last(), VegetationStageTimes::default());
        telemetry.commit();
        assert_eq!(telemetry.work().synchronizations, 1);
        assert!(telemetry.last().total() >= telemetry.last().residency);

        // The pending block starts empty again, so one stage's time cannot be counted twice.
        telemetry.commit();
        assert_eq!(telemetry.last(), VegetationStageTimes::default());
        assert_eq!(telemetry.work().synchronizations, 2);
    }

    /// The counters are additive and survive across synchronizations, and a rebind clears them.
    #[test]
    fn counters_accumulate_until_a_rebind() {
        let mut telemetry = VegetationTelemetry::default();
        telemetry.record_query(12);
        telemetry.record_query(0);
        telemetry.record_mutation(256);
        telemetry.record_snapshot(4_096);
        telemetry.record_ecology_ticks(3);
        let work = telemetry.work();
        assert_eq!((work.queries, work.query_hits), (2, 12));
        assert_eq!((work.mutations, work.mutation_bytes), (1, 256));
        assert_eq!((work.snapshots, work.snapshot_bytes), (1, 4_096));
        assert_eq!(work.ecology_ticks, 3);

        telemetry.reset();
        assert_eq!(telemetry.work(), VegetationWorkCounters::default());
    }
}
