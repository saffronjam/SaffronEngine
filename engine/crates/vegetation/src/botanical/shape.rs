use std::collections::BTreeMap;

use saffron_spatial::{DecisionCurve, DecisionScalar, UnitInterval};

use crate::Result;

use super::assembly::{BotanicalAxis, BotanicalElementId, isqrt};
use super::operator::{BotanicalElement, PruneRule, TropismKind, field};

/// One straight axis to build: where it starts, which way it goes, and how it tapers.
pub(super) struct AxisSeed<'a> {
    pub id: BotanicalElementId,
    pub parent: Option<BotanicalElementId>,
    pub element: BotanicalElement,
    pub base: [DecisionScalar; 3],
    pub direction: [i32; 3],
    pub length: DecisionScalar,
    pub base_radius: DecisionScalar,
    pub taper: &'a DecisionCurve,
    pub segments: u32,
}

/// Builds a straight axis of `segments` segments from the seed's base along its direction.
pub(super) fn straight_axis(seed: AxisSeed<'_>) -> Result<BotanicalAxis> {
    let magnitude = isqrt(
        (0..3)
            .map(|axis| i64::from(seed.direction[axis]) * i64::from(seed.direction[axis]))
            .sum(),
    )
    .max(1);
    let mut points = Vec::with_capacity(seed.segments as usize + 1);
    let mut radii = Vec::with_capacity(seed.segments as usize + 1);
    for step in 0..=seed.segments {
        let along = interpolate(UnitInterval::ZERO, UnitInterval::ONE, step, seed.segments);
        let travelled = i64::from(seed.length.bits()) * i64::from(along.bits())
            / i64::from(UnitInterval::ONE.bits());
        points.push(std::array::from_fn(|axis| {
            DecisionScalar::from_bits(
                seed.base[axis].bits().saturating_add(
                    i32::try_from(travelled * i64::from(seed.direction[axis]) / magnitude)
                        .unwrap_or(i32::MAX),
                ),
            )
        }));
        let factor = seed.taper.sample(along)?;
        radii.push(DecisionScalar::from_bits(
            ((i64::from(seed.base_radius.bits()) * i64::from(factor.bits())) >> 16) as i32,
        ));
    }
    Ok(BotanicalAxis {
        id: seed.id,
        parent: seed.parent,
        frame: None,
        element: seed.element,
        points,
        radii,
    })
}

/// The default taper: full radius at the base falling to a tenth at the tip.
pub(super) fn linear_taper() -> DecisionCurve {
    DecisionCurve::new(vec![
        (UnitInterval::ZERO, DecisionScalar::from_bits(65_536)),
        (UnitInterval::ONE, DecisionScalar::from_bits(6_553)),
    ])
    .expect("the default taper is two ordered points")
}

/// Where `step` of `steps` falls between `start` and `end`.
pub(super) fn interpolate(
    start: UnitInterval,
    end: UnitInterval,
    step: u32,
    steps: u32,
) -> UnitInterval {
    if steps == 0 {
        return start;
    }
    let span = i64::from(end.bits()) - i64::from(start.bits());
    UnitInterval::from_bits(
        (i64::from(start.bits()) + span * i64::from(step) / i64::from(steps)).clamp(0, 65_535)
            as u16,
    )
}

/// Sine and cosine of a signed normalized turn, in `UnitInterval` bits, from an integer table.
/// A table rather than `f64`: an authored plant must grow the same on every target, and a libm
/// difference of one bit would move a branch.
pub(crate) fn turn_sin_cos(turn: i64) -> (i64, i64) {
    const QUARTER: i64 = 16_384;
    const TABLE: [i64; 17] = [
        0, 6_393, 12_539, 18_204, 23_170, 27_245, 30_273, 32_137, 32_767, 32_137, 30_273, 27_245,
        23_170, 18_204, 12_539, 6_393, 0,
    ];
    let sample = |phase: i64| -> i64 {
        let phase = phase.rem_euclid(4 * QUARTER);
        let (index, sign) = if phase < 2 * QUARTER {
            (phase, 1)
        } else {
            (phase - 2 * QUARTER, -1)
        };
        let slot = (index * 16 / (2 * QUARTER)).clamp(0, 16) as usize;
        sign * TABLE[slot] * 2
    };
    (sample(turn), sample(turn + QUARTER))
}

/// A direction `declination` off `axis`, rotated `azimuth` about it.
pub(super) fn cone_direction(
    axis: [DecisionScalar; 3],
    declination: i64,
    azimuth: i64,
) -> [i32; 3] {
    let (sin_dec, cos_dec) = turn_sin_cos(declination / 2);
    let (sin_az, cos_az) = turn_sin_cos(azimuth * 4);
    let one = i64::from(UnitInterval::ONE.bits());
    // The axis contributes the cosine share; the perpendicular plane contributes the sine share.
    let lateral = sin_dec.abs().min(one);
    [
        ((i64::from(axis[0].bits()) * cos_dec + lateral * cos_az * 4) / one) as i32,
        ((i64::from(axis[1].bits()) * cos_dec) / one) as i32,
        ((i64::from(axis[2].bits()) * cos_dec + lateral * sin_az * 4) / one) as i32,
    ]
}

/// Position, outward direction, and radius at `along` on `axis`, rotated `turn` about it.
pub(super) fn sample_axis(
    axis: &BotanicalAxis,
    along: UnitInterval,
    turn: i64,
) -> Result<([DecisionScalar; 3], [DecisionScalar; 3], DecisionScalar)> {
    if axis.points.is_empty() {
        return Err(field("axis.points"));
    }
    let last = axis.points.len() - 1;
    let scaled = usize::from(along.bits()) * last;
    let index = (scaled / usize::from(UnitInterval::ONE.bits())).min(last);
    let position = axis.points[index];
    let radius = axis.radii[index.min(axis.radii.len() - 1)];
    let (sin, cos) = turn_sin_cos(turn);
    let one = i64::from(UnitInterval::ONE.bits());
    let direction = [
        DecisionScalar::from_bits((cos * i64::from(UnitInterval::ONE.bits()) / one) as i32),
        DecisionScalar::from_bits(i32::from(UnitInterval::ONE.bits()) / 4),
        DecisionScalar::from_bits((sin * i64::from(UnitInterval::ONE.bits()) / one) as i32),
    ];
    Ok((position, direction, radius))
}

/// Bends an axis, accumulating along its length so the tip moves most and the base not at all.
/// The displacement is measured from how far along the axis a point sits, never from its current
/// height — otherwise an already-drooping branch would be *lifted* by gravity.
pub(super) fn bend(axis: &mut BotanicalAxis, kind: TropismKind, strength: UnitInterval) {
    if strength.bits() == 0 || axis.points.len() < 2 {
        return;
    }
    let sign: i64 = match kind {
        TropismKind::Phototropism => 1,
        TropismKind::Gravitropism | TropismKind::Thigmotropism => -1,
    };
    let one = i64::from(UnitInterval::ONE.bits());
    let base = axis.points[0];
    let count = (axis.points.len() - 1) as i64;
    for (step, point) in axis.points.iter_mut().enumerate().skip(1) {
        let travelled = isqrt(
            (0..3)
                .map(|lane| {
                    let delta = i64::from(point[lane].bits() - base[lane].bits());
                    delta * delta
                })
                .sum(),
        );
        // Quadratic in distance travelled: a smooth arc rather than a kink at the base.
        let step = step as i64;
        let share = step * step * one / (count * count).max(1);
        let displacement = i64::from(strength.bits()) * travelled / one * share / one;
        point[1] = DecisionScalar::from_bits(
            point[1]
                .bits()
                .saturating_add(i32::try_from(sign * displacement).unwrap_or(0)),
        );
    }
}

/// Applies a prune rule, keeping canonical order.
pub(super) fn prune(
    axes: Vec<BotanicalAxis>,
    rule: PruneRule,
    threshold: DecisionScalar,
    count: u32,
) -> Vec<BotanicalAxis> {
    match rule {
        PruneRule::BelowHeight => axes
            .into_iter()
            .filter(|axis| axis.base_height().bits() >= threshold.bits())
            .collect(),
        PruneRule::ShorterThan => axes
            .into_iter()
            .filter(|axis| axis.length().bits() >= threshold.bits())
            .collect(),
        PruneRule::KeepStrongest => {
            // Strength is base radius; ties break on identity so the survivor never depends on
            // input order.
            let mut per_parent: BTreeMap<Option<BotanicalElementId>, Vec<BotanicalAxis>> =
                BTreeMap::new();
            for axis in axes {
                per_parent.entry(axis.parent).or_default().push(axis);
            }
            let mut kept = Vec::new();
            for (_, mut siblings) in per_parent {
                siblings.sort_by_key(|axis| {
                    (
                        std::cmp::Reverse(axis.radii.first().map_or(0, |radius| radius.bits())),
                        axis.id,
                    )
                });
                siblings.truncate(count as usize);
                kept.extend(siblings);
            }
            kept.sort_by_key(|axis| axis.id);
            kept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::grow::{BotanicalBudget, NoBotanicalModules, grow};
    use super::super::tests_support::birch;
    use super::*;

    /// Pruning removes axes by rule and keeps canonical order, so what survives cannot depend on
    /// the order the axes arrived in.
    #[test]
    fn pruning_keeps_the_strongest_in_canonical_order() {
        let grown = grow(&birch(), 0, &NoBotanicalModules, &BotanicalBudget::COOK)
            .unwrap()
            .assembly;
        let branches: Vec<BotanicalAxis> = grown
            .axes
            .iter()
            .filter(|axis| axis.element == BotanicalElement::Branch)
            .cloned()
            .collect();
        assert!(branches.len() > 2);

        let kept = prune(
            branches.clone(),
            PruneRule::KeepStrongest,
            DecisionScalar::from_bits(0),
            2,
        );
        let mut reversed = branches.clone();
        reversed.reverse();
        let kept_reversed = prune(
            reversed,
            PruneRule::KeepStrongest,
            DecisionScalar::from_bits(0),
            2,
        );
        assert_eq!(
            kept, kept_reversed,
            "input order cannot change the survivors"
        );
        assert_eq!(kept.len(), 2);

        let high = prune(
            branches,
            PruneRule::BelowHeight,
            DecisionScalar::from_integer(4).unwrap(),
            0,
        );
        assert!(high.iter().all(
            |axis| axis.base_height().bits() >= DecisionScalar::from_integer(4).unwrap().bits()
        ));
    }
}
