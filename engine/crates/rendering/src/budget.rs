//! The frame-budget controller: auto-steps the render-quality tier to hold the frame budget.
//!
//! When `PerfConfig::auto_quality` is on, the controller watches frame work time against the budget
//! (`1000 / target_fps`) and steps the [`QualityTier`] down after a sustained over-budget run, back
//! up after a sustained run with headroom. Hysteresis (consecutive-frame thresholds plus a cooldown
//! after each switch) keeps it from oscillating; it never auto-selects `Ultra` (a stills tier) or
//! drops below `Low`.
//!
//! Below the `Low` floor it steps dynamic resolution instead: the render targets shrink and the
//! present blit upscales. Going down it spends tier steps first, since cheaper GI is less visible
//! than fewer pixels; coming back up it restores resolution before raising the tier.

use crate::quality::QualityTier;

/// Consecutive over-budget frames before stepping down.
const OVER_BUDGET_STEP: u32 = 12;
/// Consecutive comfortably-under-budget frames before stepping up.
const UNDER_BUDGET_STEP: u32 = 90;
/// A single frame this many times the budget forces an immediate downstep (a "panic" hitch).
const PANIC_MULTIPLE: f32 = 2.0;
/// Headroom required to consider stepping up: work below this fraction of budget.
const UPSTEP_HEADROOM_FRAC: f32 = 0.7;
/// Frames to wait after a switch before another, so a step's effect is measured before the next.
const COOLDOWN_FRAMES: u32 = 30;

/// The auto-quality tier ladder, cheapest first. `Ultra` is excluded — it is an explicit
/// stills/screenshot tier, never auto-selected.
const LADDER: [QualityTier; 3] = [QualityTier::Low, QualityTier::Medium, QualityTier::High];

/// The dynamic-resolution ladder, cheapest first; `1.0` is native. Stepped only below the tier
/// floor, so most scenes never leave native resolution.
const SCALE_LADDER: [f32; 5] = [0.5, 0.59, 0.71, 0.83, 1.0];

/// One budget adjustment: change the quality tier, or (below the tier floor) the render scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BudgetStep {
    /// Switch to this render-quality tier.
    Tier(QualityTier),
    /// Switch to this dynamic-resolution factor (`(0, 1]`).
    Scale(f32),
}

/// The `SCALE_LADDER` rung nearest `scale` (so a hand-set scale snaps onto the ladder).
fn scale_rung(scale: f32) -> usize {
    SCALE_LADDER
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - scale)
                .abs()
                .partial_cmp(&(*b - scale).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(SCALE_LADDER.len() - 1, |(i, _)| i)
}

/// The rolling state of the frame-budget controller.
#[derive(Clone, Copy, Debug, Default)]
pub struct BudgetController {
    over: u32,
    under: u32,
    cooldown: u32,
}

impl BudgetController {
    /// A fresh controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one frame's work time (ms) against the budget (ms) and returns the step to take, or
    /// `None` to hold. `tier`/`scale` are the active dials. Going down it steps the tier first and
    /// only drops resolution once at the `Low` floor; coming up it restores resolution before
    /// raising the tier. A non-positive `budget_ms` (uncapped) or a `tier` off the auto ladder
    /// (e.g. `Ultra`/`Custom`) holds.
    pub fn update(
        &mut self,
        frame_ms: f32,
        budget_ms: f32,
        tier: QualityTier,
        scale: f32,
    ) -> Option<BudgetStep> {
        if budget_ms <= 0.0 {
            return None;
        }
        let rung = LADDER.iter().position(|&t| t == tier)?;
        let srung = scale_rung(scale);

        if self.cooldown > 0 {
            self.cooldown -= 1;
        }

        // Panic: a single very-late frame steps down at once (bypassing the streak + cooldown) —
        // tier first, then resolution at the floor.
        if frame_ms > budget_ms * PANIC_MULTIPLE {
            if rung > 0 {
                return Some(self.commit(BudgetStep::Tier(LADDER[rung - 1])));
            }
            if srung > 0 {
                return Some(self.commit(BudgetStep::Scale(SCALE_LADDER[srung - 1])));
            }
        }

        if frame_ms > budget_ms {
            self.over += 1;
            self.under = 0;
        } else if frame_ms < budget_ms * UPSTEP_HEADROOM_FRAC {
            self.under += 1;
            self.over = 0;
        } else {
            // In the comfortable band around budget — neither streak advances.
            self.over = 0;
            self.under = 0;
        }

        if self.cooldown > 0 {
            return None;
        }
        if self.over >= OVER_BUDGET_STEP {
            if rung > 0 {
                return Some(self.commit(BudgetStep::Tier(LADDER[rung - 1])));
            }
            if srung > 0 {
                return Some(self.commit(BudgetStep::Scale(SCALE_LADDER[srung - 1])));
            }
        }
        if self.under >= UNDER_BUDGET_STEP {
            if srung + 1 < SCALE_LADDER.len() {
                return Some(self.commit(BudgetStep::Scale(SCALE_LADDER[srung + 1])));
            }
            if rung + 1 < LADDER.len() {
                return Some(self.commit(BudgetStep::Tier(LADDER[rung + 1])));
            }
        }
        None
    }

    /// Records a committed switch: reset the streaks and arm the cooldown.
    fn commit(&mut self, step: BudgetStep) -> BudgetStep {
        self.over = 0;
        self.under = 0;
        self.cooldown = COOLDOWN_FRAMES;
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Native render scale (1.0) for the tier-only cases.
    const NATIVE: f32 = 1.0;

    #[test]
    fn sustained_over_budget_steps_down_one_rung() {
        let mut c = BudgetController::new();
        // Just under the panic multiple, so it takes the streak path.
        for _ in 0..OVER_BUDGET_STEP - 1 {
            assert_eq!(c.update(20.0, 16.0, QualityTier::High, NATIVE), None);
        }
        assert_eq!(
            c.update(20.0, 16.0, QualityTier::High, NATIVE),
            Some(BudgetStep::Tier(QualityTier::Medium)),
            "steps High → Medium after the over-budget streak"
        );
    }

    #[test]
    fn a_panic_frame_steps_down_immediately() {
        let mut c = BudgetController::new();
        assert_eq!(
            c.update(40.0, 16.0, QualityTier::Medium, NATIVE),
            Some(BudgetStep::Tier(QualityTier::Low)),
            "a >2× budget frame steps down at once"
        );
    }

    #[test]
    fn sustained_headroom_steps_up_then_cooldown_holds() {
        let mut c = BudgetController::new();
        // Comfortable headroom for the up-step streak.
        for _ in 0..UNDER_BUDGET_STEP - 1 {
            assert_eq!(c.update(5.0, 16.0, QualityTier::Low, NATIVE), None);
        }
        assert_eq!(
            c.update(5.0, 16.0, QualityTier::Low, NATIVE),
            Some(BudgetStep::Tier(QualityTier::Medium)),
            "at native scale, steps Low → Medium after the headroom streak"
        );
        // The cooldown now holds even with continued headroom.
        assert_eq!(
            c.update(5.0, 16.0, QualityTier::Medium, NATIVE),
            None,
            "cooldown holds"
        );
    }

    #[test]
    fn below_low_it_drops_resolution_then_floors() {
        // At the Low tier floor + native scale, sustained over-budget drops resolution one rung.
        let mut c = BudgetController::new();
        for _ in 0..OVER_BUDGET_STEP - 1 {
            assert_eq!(c.update(20.0, 16.0, QualityTier::Low, NATIVE), None);
        }
        assert_eq!(
            c.update(20.0, 16.0, QualityTier::Low, NATIVE),
            Some(BudgetStep::Scale(0.83)),
            "Low + native + over-budget steps the render scale down"
        );
        // At the scale floor, a panic frame has nothing left to give.
        let mut c = BudgetController::new();
        assert_eq!(
            c.update(100.0, 16.0, QualityTier::Low, 0.5),
            None,
            "nothing below Low + minimum scale"
        );
    }

    #[test]
    fn coming_up_it_restores_resolution_before_raising_the_tier() {
        let mut c = BudgetController::new();
        for _ in 0..UNDER_BUDGET_STEP - 1 {
            assert_eq!(c.update(5.0, 16.0, QualityTier::Low, 0.71), None);
        }
        assert_eq!(
            c.update(5.0, 16.0, QualityTier::Low, 0.71),
            Some(BudgetStep::Scale(0.83)),
            "headroom restores resolution first, holding the tier at Low"
        );
    }

    #[test]
    fn never_climbs_above_high() {
        // At High + native with endless headroom: never auto-climbs to Ultra or past native scale.
        let mut c = BudgetController::new();
        for _ in 0..UNDER_BUDGET_STEP * 2 {
            assert_eq!(c.update(1.0, 16.0, QualityTier::High, NATIVE), None);
        }
    }

    #[test]
    fn uncapped_or_off_ladder_holds() {
        let mut c = BudgetController::new();
        assert_eq!(
            c.update(100.0, 0.0, QualityTier::High, NATIVE),
            None,
            "uncapped holds"
        );
        assert_eq!(
            c.update(100.0, 16.0, QualityTier::Ultra, NATIVE),
            None,
            "Ultra is off the auto ladder"
        );
        assert_eq!(
            c.update(100.0, 16.0, QualityTier::Custom, NATIVE),
            None,
            "Custom is off the auto ladder"
        );
    }
}
