use crate::PlantLifecycle;

/// Whether a lifecycle stage is a living plant that grows, shades, and competes.
pub(super) const fn live(lifecycle: PlantLifecycle) -> bool {
    matches!(
        lifecycle,
        PlantLifecycle::Seed
            | PlantLifecycle::Sprout
            | PlantLifecycle::Juvenile
            | PlantLifecycle::Mature
            | PlantLifecycle::Senescent
    )
}

/// The stage a plant of `age` belongs in, never moving backwards from `current`.
pub(super) fn stage_for(age: u64, thresholds: [u64; 4], current: PlantLifecycle) -> PlantLifecycle {
    let staged = if age >= thresholds[3] {
        PlantLifecycle::Senescent
    } else if age >= thresholds[2] {
        PlantLifecycle::Mature
    } else if age >= thresholds[1] {
        PlantLifecycle::Juvenile
    } else if age >= thresholds[0] {
        PlantLifecycle::Sprout
    } else {
        PlantLifecycle::Seed
    };
    // Growth is monotonic: a stage reached is never un-reached by a rule change or a stale age.
    if stage_rank(staged) > stage_rank(current) {
        staged
    } else {
        current
    }
}

const fn stage_rank(lifecycle: PlantLifecycle) -> u8 {
    match lifecycle {
        PlantLifecycle::Seed => 0,
        PlantLifecycle::Sprout => 1,
        PlantLifecycle::Juvenile => 2,
        PlantLifecycle::Mature => 3,
        PlantLifecycle::Senescent => 4,
        PlantLifecycle::Dead => 5,
        PlantLifecycle::Stump => 6,
        PlantLifecycle::Removed => 7,
    }
}
