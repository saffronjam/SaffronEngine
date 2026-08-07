use saffron_spatial::UnitInterval;

/// Health gained by a fully suitable tick, and lost by a fully unsuitable one.
pub(super) const HEALTH_STEP: u16 = 3_000;

/// How suitable a condition is for a species: at or above its tolerance is fully suitable, and
/// below it falls off linearly to nothing.
pub(super) fn suitability(condition: UnitInterval, tolerance: UnitInterval) -> UnitInterval {
    if condition.bits() >= tolerance.bits() {
        return UnitInterval::ONE;
    }
    if tolerance.bits() == 0 {
        return UnitInterval::ONE;
    }
    let scaled = u32::from(condition.bits()) * u32::from(UnitInterval::ONE.bits())
        / u32::from(tolerance.bits());
    saturating_unit(scaled)
}

/// Moves `value` toward `target` by at most `step`, never overshooting.
pub(super) fn approach(value: UnitInterval, target: UnitInterval, step: u16) -> UnitInterval {
    let current = i32::from(value.bits());
    let goal = i32::from(target.bits());
    let step = i32::from(step);
    let next = if goal > current {
        (current + step).min(goal)
    } else {
        (current - step).max(goal)
    };
    UnitInterval::from_bits(next.clamp(0, i32::from(UnitInterval::ONE.bits())) as u16)
}

/// `value` scaled by `factor`, both closed unit values.
pub(super) fn scale(value: UnitInterval, factor: UnitInterval) -> UnitInterval {
    UnitInterval::from_bits(
        (u32::from(value.bits()) * u32::from(factor.bits()) / u32::from(UnitInterval::ONE.bits()))
            as u16,
    )
}

pub(super) fn invert(value: UnitInterval) -> UnitInterval {
    UnitInterval::from_bits(UnitInterval::ONE.bits() - value.bits())
}

pub(super) fn saturating_unit(value: u32) -> UnitInterval {
    UnitInterval::from_bits(value.min(u32::from(UnitInterval::ONE.bits())) as u16)
}
