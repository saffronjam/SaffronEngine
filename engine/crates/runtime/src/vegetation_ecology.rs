//! The world simulation clock vegetation biology advances on.
//!
//! Biological time is its own axis, counted in fixed ecology ticks. This clock is what makes it
//! advance *with the world* rather than only when a human asks: simulated play time accumulates
//! here, whole ticks fall out of it, and the bound vegetation world catches its dependency regions
//! up to them at the fixed synchronization point. Nothing here reads the calendar or the time of
//! day — those drive appearance, and rewinding them must not un-grow a tree.
//!
//! Simulated time accumulates in integer microseconds, so a long session cannot drift the tick rate
//! the way a repeatedly-summed float would.

use saffron_vegetation::{
    EcologyCatchUp, EcologyCatchUpBudget, EcologyCatchUpReport, EcologyInfluence, EcologyWeather,
    VegetationWorld,
};

/// Simulated milliseconds one ecology tick spans until a project says otherwise. One tick a minute
/// of play brings a default family to maturity across a long session rather than across a frame.
const DEFAULT_TICK_MILLISECONDS: u32 = 60_000;

/// Threads the pure per-region rule evaluation may spread across at most.
const MAX_ECOLOGY_WORKERS: usize = 8;

/// Weather the clock hands each tick until a weather system supplies it.
///
/// Vegetation consumes sampled water and warmth and owns only the biological response, so something
/// has to sample them; a temperate growing sample is what a world with no weather system should age
/// under, not the dormant zero a defaulted struct would give.
const DEFAULT_WEATHER: EcologyWeather = EcologyWeather {
    water: saffron_spatial::UnitInterval::from_bits(40_000),
    warmth: saffron_spatial::UnitInterval::from_bits(45_000),
};

/// What the last catch-up against the bound world found, and the ground it found it on.
#[derive(Clone, Copy, Debug)]
struct CatchUpStanding {
    report: EcologyCatchUpReport,
    /// The world's ecology ground revision after the catch-up ran. A poll that sees a different
    /// revision knows a cell loaded, unloaded, or gained plants since, so what a region owes is no
    /// longer what this report says.
    ground_revision: u64,
}

/// Drives biological time from simulated play time, and catches the bound world up to it.
#[derive(Clone, Debug)]
pub struct VegetationEcologyClock {
    tick_milliseconds: u32,
    pending_micros: u64,
    budget: EcologyCatchUpBudget,
    influence: EcologyInfluence,
    weather: EcologyWeather,
    running: bool,
    last: Option<CatchUpStanding>,
}

impl Default for VegetationEcologyClock {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_ECOLOGY_WORKERS);
        Self {
            tick_milliseconds: DEFAULT_TICK_MILLISECONDS,
            pending_micros: 0,
            budget: EcologyCatchUpBudget {
                workers: u32::try_from(workers).unwrap_or(1),
                ..EcologyCatchUpBudget::default()
            },
            influence: EcologyInfluence::default(),
            weather: DEFAULT_WEATHER,
            running: true,
            last: None,
        }
    }
}

impl VegetationEcologyClock {
    /// Folds one simulated step into the clock. A paused clock accumulates nothing: biology stops
    /// with the world, it does not race ahead while the world is held still.
    pub fn accumulate(&mut self, seconds: f32) {
        if !self.running || !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let micros = (f64::from(seconds) * 1_000_000.0) as u64;
        self.pending_micros = self.pending_micros.saturating_add(micros);
    }

    /// Whole ticks the accumulated simulated time has earned.
    #[must_use]
    pub fn due_ticks(&self) -> u64 {
        self.pending_micros / self.tick_micros()
    }

    /// Whether the synchronization point has ecology work to do: a tick has come due, a *resident*
    /// region is still behind world time, or the ground moved since the last catch-up looked at it.
    ///
    /// It keys on the arrears a region can actually pay. A region spanning ground outside the
    /// streaming window owes its ticks for as long as that ground stays unloaded, and treating that
    /// as outstanding work would rebuild the whole world's region closure every frame, forever, to
    /// run nothing. `ground_revision` is what re-arms the poll when that ground does load —
    /// [`VegetationWorld::ecology_ground_revision`].
    ///
    /// A stopped clock wants nothing at all, including the ticks a region already owes — pausing
    /// biology has to actually pause it, or the pause is a lie. The authoring surface still steps
    /// through [`advance_to`](Self::advance_to) while stopped.
    #[must_use]
    pub fn wants_advance(&self, ground_revision: u64) -> bool {
        self.running
            && (self.due_ticks() > 0
                || self.last.as_ref().is_none_or(|standing| {
                    standing.report.ticks_owed > 0 || standing.ground_revision != ground_revision
                }))
    }

    /// Moves world biological time by the ticks that came due and catches the world's dependency
    /// regions up to it, within the configured budget.
    ///
    /// # Errors
    ///
    /// Propagates [`VegetationWorld::advance_ecology`].
    pub fn advance(
        &mut self,
        world: &mut VegetationWorld,
    ) -> saffron_vegetation::Result<EcologyCatchUpReport> {
        let due = self.due_ticks();
        self.pending_micros -= due * self.tick_micros();
        let target = world
            .persistent_state()
            .ecology()
            .clock()
            .tick()
            .saturating_add(due);
        self.advance_to(world, target, self.budget.max_ticks)
    }

    /// Moves world biological time to an explicit `target` and runs up to `max_ticks` region ticks,
    /// under the clock's declared influence, weather, and worker count.
    ///
    /// The authoring surface drives this — step and run over the timeline — while
    /// [`advance`](Self::advance) is what the world clock drives. Both leave what they did not run
    /// owed, and record it, so the world clock resumes exactly where an explicit step stopped.
    ///
    /// # Errors
    ///
    /// Propagates [`VegetationWorld::advance_ecology`].
    pub fn advance_to(
        &mut self,
        world: &mut VegetationWorld,
        target: u64,
        max_ticks: u32,
    ) -> saffron_vegetation::Result<EcologyCatchUpReport> {
        let rules = world.ecology_rules();
        let relations = world.ecology_relations();
        let report = world.advance_ecology(&EcologyCatchUp {
            target_tick: target,
            budget: EcologyCatchUpBudget {
                max_ticks: max_ticks.max(1),
                workers: self.budget.workers,
            },
            influence: self.influence,
            rules: &rules,
            relations: &relations,
            weather: self.weather,
        })?;
        self.last = Some(CatchUpStanding {
            report,
            ground_revision: world.ecology_ground_revision(),
        });
        Ok(report)
    }

    /// Forgets the accumulated time and the outstanding work. A different bound generation's
    /// regions are not this one's, and its owed ticks belong to a world that is gone.
    pub fn rebind(&mut self) {
        self.pending_micros = 0;
        self.last = None;
    }

    /// Whether simulated time reaches the clock.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// Simulated milliseconds one ecology tick spans.
    #[must_use]
    pub const fn tick_milliseconds(&self) -> u32 {
        self.tick_milliseconds
    }

    /// Simulated milliseconds accumulated toward the next whole tick.
    #[must_use]
    pub const fn pending_milliseconds(&self) -> u64 {
        (self.pending_micros % self.tick_micros()) / 1_000
    }

    /// Region ticks one synchronization may execute.
    #[must_use]
    pub const fn max_ticks_per_sync(&self) -> u32 {
        self.budget.max_ticks
    }

    /// Threads the per-region rule evaluation spreads across.
    #[must_use]
    pub const fn workers(&self) -> u32 {
        self.budget.workers
    }

    /// The sampled weather each tick runs under.
    #[must_use]
    pub const fn weather(&self) -> EcologyWeather {
        self.weather
    }

    /// The declared cross-cell influence, which fixes how wide a dependency region is. One place
    /// holds it, so a reader reporting the region radius and a tick reading a halo cannot disagree.
    #[must_use]
    pub const fn influence(&self) -> EcologyInfluence {
        self.influence
    }

    /// The last catch-up's report, absent until one has run against the bound world.
    #[must_use]
    pub const fn last_report(&self) -> Option<EcologyCatchUpReport> {
        match self.last {
            Some(standing) => Some(standing.report),
            None => None,
        }
    }

    /// Starts or stops simulated time reaching the clock.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// Sets how much simulated time one ecology tick spans. Zero would make every frame a tick, so
    /// it clamps to a millisecond.
    pub fn set_tick_milliseconds(&mut self, milliseconds: u32) {
        self.tick_milliseconds = milliseconds.max(1);
    }

    /// Sets how many region ticks one synchronization may execute. Zero would owe forever, so it
    /// clamps to one.
    pub fn set_max_ticks_per_sync(&mut self, ticks: u32) {
        self.budget.max_ticks = ticks.max(1);
    }

    /// Sets how many threads the per-region rule evaluation spreads across.
    pub fn set_workers(&mut self, workers: u32) {
        self.budget.workers = workers.max(1);
    }

    /// Sets the sampled weather each tick runs under.
    pub fn set_weather(&mut self, weather: EcologyWeather) {
        self.weather = weather;
    }

    const fn tick_micros(&self) -> u64 {
        self.tick_milliseconds as u64 * 1_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_spatial::UnitInterval;

    /// Simulated time turns into whole ticks and keeps the remainder, so a tick rate that is not a
    /// multiple of the frame time neither loses nor invents biological time.
    #[test]
    fn simulated_time_becomes_whole_ticks_and_keeps_the_remainder() {
        let mut clock = VegetationEcologyClock::default();
        clock.set_tick_milliseconds(100);
        assert_eq!(clock.due_ticks(), 0);

        // 1/64 s frames are 15.625 ms exactly, so six of them fall short of a 100 ms tick.
        for _ in 0..6 {
            clock.accumulate(1.0 / 64.0);
        }
        assert_eq!(clock.due_ticks(), 0);
        assert_eq!(clock.pending_milliseconds(), 93);

        // The seventh crosses the tick, and the 9.375 ms past it stays toward the next one.
        clock.accumulate(1.0 / 64.0);
        assert_eq!(clock.due_ticks(), 1);
        assert_eq!(clock.pending_milliseconds(), 9);

        // Nothing consumes the accumulated time except an advance, so the ticks keep piling up.
        for _ in 0..7 {
            clock.accumulate(1.0 / 64.0);
        }
        assert_eq!(clock.due_ticks(), 2);
        assert_eq!(clock.pending_milliseconds(), 18);
    }

    /// A paused clock is a stopped one: no simulated time reaches it, so biology holds still with
    /// the world rather than catching up the moment it resumes.
    #[test]
    fn a_paused_clock_accumulates_nothing() {
        let mut clock = VegetationEcologyClock::default();
        clock.set_tick_milliseconds(10);
        clock.set_running(false);
        clock.accumulate(5.0);
        assert_eq!(clock.due_ticks(), 0);
        clock.set_running(true);
        clock.accumulate(0.05);
        assert_eq!(clock.due_ticks(), 5);
    }

    /// The synchronization point does no ecology work at steady state, and picks it up again the
    /// moment a tick comes due, the ground moves, or a rebind leaves the outstanding work unknown.
    #[test]
    fn work_is_wanted_only_when_a_tick_is_due_or_something_is_owed() {
        let mut clock = VegetationEcologyClock::default();
        clock.set_tick_milliseconds(1_000);
        assert!(clock.wants_advance(7), "nothing has run against this world");

        clock.last = Some(standing(EcologyCatchUpReport::default(), 7));
        assert!(!clock.wants_advance(7));

        clock.accumulate(1.0);
        assert!(clock.wants_advance(7), "a tick came due");

        clock.pending_micros = 0;
        clock.last = Some(standing(
            EcologyCatchUpReport {
                ticks_owed: 3,
                ..EcologyCatchUpReport::default()
            },
            7,
        ));
        assert!(
            clock.wants_advance(7),
            "a region is still behind world time"
        );

        clock.set_running(false);
        assert!(
            !clock.wants_advance(7),
            "a stopped clock does not work off owed ticks either",
        );
        clock.set_running(true);

        clock.rebind();
        assert!(
            clock.wants_advance(7),
            "a fresh world's standing is unknown"
        );
    }

    /// Ticks a region owes only because ground it spans is unloaded are not work to poll for: a
    /// world whose planted cells reach past the streaming window owes them for as long as that
    /// ground stays out, and rebuilding the whole region closure every frame to run nothing is not a
    /// catch-up. Loading the ground moves the world's ecology revision, which is what re-arms it.
    #[test]
    fn ticks_waiting_on_unloaded_ground_are_not_polled_for() {
        let mut clock = VegetationEcologyClock::default();
        clock.set_tick_milliseconds(1_000);
        let stalled = EcologyCatchUpReport {
            regions: 2,
            regions_awaiting_residency: 1,
            ticks_awaiting_residency: 120,
            ..EcologyCatchUpReport::default()
        };
        clock.last = Some(standing(stalled, 12));
        assert!(
            !clock.wants_advance(12),
            "the arrears are real but nothing can spend them",
        );
        assert!(
            clock.wants_advance(13),
            "a cell loaded, unloaded, or gained plants, so the standing is stale",
        );
    }

    fn standing(report: EcologyCatchUpReport, ground_revision: u64) -> CatchUpStanding {
        CatchUpStanding {
            report,
            ground_revision,
        }
    }

    /// The clamps exist because the alternatives are degenerate: a zero-length tick would fire every
    /// frame, and a zero budget would owe forever.
    #[test]
    fn degenerate_settings_clamp_instead_of_dividing_by_zero() {
        let mut clock = VegetationEcologyClock::default();
        clock.set_tick_milliseconds(0);
        assert_eq!(clock.tick_milliseconds(), 1);
        clock.set_max_ticks_per_sync(0);
        assert_eq!(clock.max_ticks_per_sync(), 1);
        clock.set_workers(0);
        assert_eq!(clock.workers(), 1);
        clock.accumulate(0.003);
        assert_eq!(clock.due_ticks(), 3);

        clock.set_weather(EcologyWeather {
            water: UnitInterval::ZERO,
            warmth: UnitInterval::ONE,
        });
        assert_eq!(clock.weather().warmth, UnitInterval::ONE);
    }
}
