//! The deterministic phenology signal: a per-mille phase of the year derived from the
//! calendar date and hemisphere, the intrinsic expression curves a plant family authors
//! per phenotype, and the resolution that reads them against persistent plant state.

use saffron_spatial::UnitInterval;

use crate::asset::{PhenotypeResponse, PhenotypeRole};
use crate::point::PlantLifecycle;

/// Days in each Gregorian month of a non-leap year, cumulative before the month.
const CUMULATIVE_DAYS: [u16; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// The per-mille domain every curve in this module is evaluated over.
const MILLE: u32 = 1000;

/// The weight a phenotype must reach before it displaces the cooked appearance.
const ACTIVATION_MILLE: u16 = 500;

/// The edge ramp a role's defaults use when the author leaves `ramp_mille` at zero.
const DEFAULT_RAMP_MILLE: u16 = 60;

fn leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The per-mille seasonal phase (`0..1000`) of a calendar date at a latitude:
/// the day-of-year fraction, shifted half a year in the southern hemisphere so a
/// curve authored against northern seasons reads the same everywhere.
pub fn season_phase_mille(year: i32, month: i32, day: i32, latitude_deg: f32) -> u16 {
    let month_index = (month - 1).clamp(0, 11) as usize;
    let leap = u16::from(leap_year(year) && month_index >= 2);
    let day_of_year =
        CUMULATIVE_DAYS[month_index] + leap + (day.clamp(1, 31) as u16).saturating_sub(1);
    let days_in_year = if leap_year(year) { 366 } else { 365 };
    let mut mille = u32::from(day_of_year) * MILLE / days_in_year;
    if latitude_deg < 0.0 {
        mille = (mille + 500) % MILLE;
    }
    mille.min(999) as u16
}

/// The persistent per-plant state a phenotype resolves against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhenologyState {
    /// Typed lifecycle, which outranks every derived curve.
    pub lifecycle: PlantLifecycle,
    /// The seasonal phase of the calendar date, per-mille of the year.
    pub season_mille: u16,
    /// Persistent plant health.
    pub health: UnitInterval,
    /// Persistent plant moisture.
    pub moisture: UnitInterval,
}

/// The response a role carries when the phenotype authors none: the season windows of
/// the calendar roles, the low-health band damage expresses in, and the high-moisture
/// band wetness expresses in. A role with no default is never derived from state.
pub fn role_default_response(role: PhenotypeRole) -> PhenotypeResponse {
    let unit = |mille: u32| UnitInterval::from_bits((mille * 65_535 / MILLE) as u16);
    let seasonal = |window: (u16, u16)| PhenotypeResponse {
        season_window: Some(window),
        ramp_mille: DEFAULT_RAMP_MILLE,
        ..PhenotypeResponse::default()
    };
    match role {
        PhenotypeRole::Flowering => seasonal((200, 450)),
        PhenotypeRole::Fruiting => seasonal((450, 700)),
        PhenotypeRole::Senescent => seasonal((700, 950)),
        PhenotypeRole::Damaged => PhenotypeResponse {
            health_band: Some((UnitInterval::ZERO, unit(400))),
            ramp_mille: DEFAULT_RAMP_MILLE,
            ..PhenotypeResponse::default()
        },
        PhenotypeRole::Wet => PhenotypeResponse {
            moisture_band: Some((unit(700), UnitInterval::ONE)),
            ramp_mille: DEFAULT_RAMP_MILLE,
            ..PhenotypeResponse::default()
        },
        PhenotypeRole::Healthy
        | PhenotypeRole::Harvested
        | PhenotypeRole::Burned
        | PhenotypeRole::Dead => PhenotypeResponse::default(),
    }
}

/// The role's defaults with every band the phenotype authored substituted in.
fn effective_response(role: PhenotypeRole, authored: PhenotypeResponse) -> PhenotypeResponse {
    let default = role_default_response(role);
    PhenotypeResponse {
        season_window: authored.season_window.or(default.season_window),
        health_band: authored.health_band.or(default.health_band),
        moisture_band: authored.moisture_band.or(default.moisture_band),
        ramp_mille: if authored.ramp_mille == 0 {
            default.ramp_mille
        } else {
            authored.ramp_mille
        },
    }
}

/// The trapezoid weight of `offset` inside a span of `span`, ramping over `ramp` at the
/// edges `ramp_low`/`ramp_high` select. An edge sitting at the end of its domain has
/// nothing to soften — full health is not a partial threshold — so it stays square.
fn trapezoid_mille(offset: u32, span: u32, ramp: u32, ramp_low: bool, ramp_high: bool) -> u16 {
    if span == 0 || offset >= span {
        return 0;
    }
    let ramp = ramp.min(span.div_ceil(2));
    if ramp == 0 {
        return MILLE as u16;
    }
    let low = if ramp_low { offset } else { u32::MAX };
    let high = if ramp_high {
        span - 1 - offset
    } else {
        u32::MAX
    };
    let distance = low.min(high);
    if distance >= ramp {
        MILLE as u16
    } else {
        ((distance + 1) * MILLE / (ramp + 1)) as u16
    }
}

/// The weight of a wrapping seasonal window at a per-mille phase. Both edges ramp: the
/// year has no end, so neither edge sits at the end of its domain.
pub fn season_weight_mille(phase_mille: u16, window: (u16, u16), ramp_mille: u16) -> u16 {
    let (start, end) = (u32::from(window.0) % MILLE, u32::from(window.1) % MILLE);
    let span = (end + MILLE - start) % MILLE;
    let offset = (u32::from(phase_mille) % MILLE + MILLE - start) % MILLE;
    trapezoid_mille(offset, span, u32::from(ramp_mille), true, true)
}

/// The weight of a unit band at a unit value.
pub fn band_weight_mille(
    value: UnitInterval,
    band: (UnitInterval, UnitInterval),
    ramp_mille: u16,
) -> u16 {
    let scale = |unit: UnitInterval| u32::from(unit.bits()) * MILLE / u32::from(u16::MAX);
    let (low, high) = (scale(band.0), scale(band.1));
    if high <= low {
        return 0;
    }
    let value = scale(value);
    if value < low {
        return 0;
    }
    trapezoid_mille(
        value - low,
        high - low + 1,
        u32::from(ramp_mille),
        low > 0,
        high < MILLE,
    )
}

/// The per-mille weight with which a phenotype expresses under `state`, or `None` when
/// neither the phenotype nor its role declares anything to derive it from — those
/// appearances are reached through the cooked default or the typed lifecycle instead.
pub fn phenotype_weight_mille(
    role: PhenotypeRole,
    response: PhenotypeResponse,
    state: PhenologyState,
) -> Option<u16> {
    let effective = effective_response(role, response);
    let mut weight = MILLE;
    let mut derived = false;
    if let Some(window) = effective.season_window {
        weight = weight
            * u32::from(season_weight_mille(
                state.season_mille,
                window,
                effective.ramp_mille,
            ))
            / MILLE;
        derived = true;
    }
    if let Some(band) = effective.health_band {
        weight =
            weight * u32::from(band_weight_mille(state.health, band, effective.ramp_mille)) / MILLE;
        derived = true;
    }
    if let Some(band) = effective.moisture_band {
        weight = weight
            * u32::from(band_weight_mille(
                state.moisture,
                band,
                effective.ramp_mille,
            ))
            / MILLE;
        derived = true;
    }
    derived.then_some(weight as u16)
}

/// The phenotype an instance renders, from typed lifecycle state and the intrinsic
/// curves each phenotype expresses through.
///
/// Dead and stump lifecycles take the Dead role and senescent the Senescent role — typed
/// state is not a weight and outranks every curve. Otherwise the phenotype whose weight
/// is highest, and above the activation threshold, wins; equal weights resolve to the
/// earlier phenotype, so authoring order is the tiebreak. The cooked phenotype is the
/// fallback throughout. `phenotypes` yields `(id, role, authored response)`.
pub fn resolve_rendered_phenotype(
    phenotypes: impl Iterator<Item = (u32, PhenotypeRole, PhenotypeResponse)> + Clone,
    cooked: u32,
    state: PhenologyState,
) -> u32 {
    let by_role = |role: PhenotypeRole| {
        phenotypes
            .clone()
            .find(|(_, candidate, _)| *candidate == role)
            .map(|(id, _, _)| id)
    };
    match state.lifecycle {
        PlantLifecycle::Dead | PlantLifecycle::Stump => {
            return by_role(PhenotypeRole::Dead).unwrap_or(cooked);
        }
        PlantLifecycle::Senescent => {
            return by_role(PhenotypeRole::Senescent).unwrap_or(cooked);
        }
        _ => {}
    }
    let mut best: Option<(u16, u32)> = None;
    for (id, role, response) in phenotypes {
        let Some(weight) = phenotype_weight_mille(role, response, state) else {
            continue;
        };
        if weight < ACTIVATION_MILLE {
            continue;
        }
        if best.is_none_or(|(current, _)| weight > current) {
            best = Some((weight, id));
        }
    }
    best.map_or(cooked, |(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(mille: u32) -> UnitInterval {
        UnitInterval::from_bits((mille * 65_535 / MILLE) as u16)
    }

    fn healthy_state(season_mille: u16) -> PhenologyState {
        PhenologyState {
            lifecycle: PlantLifecycle::Mature,
            season_mille,
            health: UnitInterval::ONE,
            moisture: unit(300),
        }
    }

    #[test]
    fn the_phase_tracks_the_calendar_and_flips_hemispheres() {
        let north_jan = season_phase_mille(2026, 1, 1, 55.0);
        let north_jul = season_phase_mille(2026, 7, 2, 55.0);
        assert_eq!(north_jan, 0);
        assert!((495..=505).contains(&north_jul), "{north_jul}");
        let south_jan = season_phase_mille(2026, 1, 1, -30.0);
        assert_eq!(south_jan, 500);
        assert!(season_phase_mille(2024, 12, 31, 0.0) < 1000);
    }

    #[test]
    fn the_seasonal_curve_ramps_in_and_out_of_its_window() {
        let window = (700, 950);
        assert_eq!(season_weight_mille(690, window, 50), 0, "before the window");
        assert_eq!(season_weight_mille(960, window, 50), 0, "after the window");
        let entering = season_weight_mille(710, window, 50);
        let plateau = season_weight_mille(820, window, 50);
        let leaving = season_weight_mille(940, window, 50);
        assert!(
            entering > 0 && entering < plateau,
            "entering {entering} plateau {plateau}"
        );
        assert_eq!(plateau, 1000, "the middle is full expression");
        assert!(leaving > 0 && leaving < plateau, "leaving {leaving}");
        assert!(
            season_weight_mille(710, window, 50) < season_weight_mille(730, window, 50),
            "the ramp is monotonic"
        );
        assert_eq!(
            season_weight_mille(710, window, 0),
            1000,
            "a zero ramp is a hard window"
        );
    }

    #[test]
    fn the_season_curve_wraps_through_the_new_year() {
        let winter = (950, 100);
        assert_eq!(season_weight_mille(980, winter, 0), 1000);
        assert_eq!(season_weight_mille(50, winter, 0), 1000);
        assert_eq!(season_weight_mille(500, winter, 0), 0);
    }

    #[test]
    fn health_drives_the_damaged_phenotype_and_moisture_the_wet_one() {
        let phenotypes = [
            (0, PhenotypeRole::Healthy, PhenotypeResponse::default()),
            (1, PhenotypeRole::Damaged, PhenotypeResponse::default()),
            (2, PhenotypeRole::Wet, PhenotypeResponse::default()),
        ];
        let resolve = |state| resolve_rendered_phenotype(phenotypes.iter().copied(), 0, state);
        let dry_and_well = PhenologyState {
            lifecycle: PlantLifecycle::Mature,
            season_mille: 100,
            health: UnitInterval::ONE,
            moisture: unit(200),
        };
        assert_eq!(resolve(dry_and_well), 0, "nothing derived: the cooked one");
        assert_eq!(
            resolve(PhenologyState {
                health: unit(100),
                ..dry_and_well
            }),
            1,
            "low health selects the damaged phenotype"
        );
        assert_eq!(
            resolve(PhenologyState {
                moisture: unit(950),
                ..dry_and_well
            }),
            2,
            "high moisture selects the wet phenotype"
        );
        // Both bands fully satisfied: the earlier phenotype wins the tie.
        assert_eq!(
            resolve(PhenologyState {
                health: UnitInterval::ZERO,
                moisture: UnitInterval::ONE,
                ..dry_and_well
            }),
            1
        );
    }

    #[test]
    fn an_authored_band_overrides_the_role_default() {
        let stoic = PhenotypeResponse {
            health_band: Some((UnitInterval::ZERO, unit(100))),
            ..PhenotypeResponse::default()
        };
        let phenotypes = [
            (0, PhenotypeRole::Healthy, PhenotypeResponse::default()),
            (1, PhenotypeRole::Damaged, stoic),
        ];
        let state = |health| PhenologyState {
            lifecycle: PlantLifecycle::Mature,
            season_mille: 100,
            health,
            moisture: unit(200),
        };
        assert_eq!(
            resolve_rendered_phenotype(phenotypes.iter().copied(), 0, state(unit(300))),
            0,
            "the default band would have fired at 0.3; the authored one does not"
        );
        assert_eq!(
            resolve_rendered_phenotype(phenotypes.iter().copied(), 0, state(unit(20))),
            1
        );
    }

    #[test]
    fn the_rendered_phenotype_follows_lifecycle_then_the_curves() {
        let phenotypes = [
            (0, PhenotypeRole::Healthy, PhenotypeResponse::default()),
            (1, PhenotypeRole::Senescent, PhenotypeResponse::default()),
            (2, PhenotypeRole::Dead, PhenotypeResponse::default()),
        ];
        let resolve = |lifecycle, season| {
            resolve_rendered_phenotype(
                phenotypes.iter().copied(),
                0,
                PhenologyState {
                    lifecycle,
                    ..healthy_state(season)
                },
            )
        };
        assert_eq!(resolve(PlantLifecycle::Mature, 100), 0);
        assert_eq!(resolve(PlantLifecycle::Mature, 800), 1, "autumn window");
        assert_eq!(
            resolve(PlantLifecycle::Senescent, 100),
            1,
            "typed lifecycle wins"
        );
        assert_eq!(resolve(PlantLifecycle::Dead, 100), 2);
        assert_eq!(resolve(PlantLifecycle::Stump, 100), 2);
        let healthy_only = [(7, PhenotypeRole::Healthy, PhenotypeResponse::default())];
        assert_eq!(
            resolve_rendered_phenotype(
                healthy_only.iter().copied(),
                7,
                PhenologyState {
                    lifecycle: PlantLifecycle::Dead,
                    ..healthy_state(0)
                }
            ),
            7
        );
    }

    #[test]
    fn a_role_without_a_derived_curve_carries_no_weight() {
        assert_eq!(
            phenotype_weight_mille(
                PhenotypeRole::Healthy,
                PhenotypeResponse::default(),
                healthy_state(560)
            ),
            None
        );
        assert_eq!(
            phenotype_weight_mille(
                PhenotypeRole::Fruiting,
                PhenotypeResponse::default(),
                healthy_state(560)
            ),
            Some(1000),
            "mid-window fruiting expresses fully"
        );
        assert_eq!(
            phenotype_weight_mille(
                PhenotypeRole::Fruiting,
                PhenotypeResponse::default(),
                healthy_state(100)
            ),
            Some(0),
            "outside its window it expresses not at all"
        );
    }

    #[test]
    fn declared_curves_multiply() {
        // Fruit only on a well-watered plant: the seasonal and moisture terms compose.
        let watered_fruit = PhenotypeResponse {
            moisture_band: Some((unit(600), UnitInterval::ONE)),
            ramp_mille: 0,
            ..PhenotypeResponse::default()
        };
        let mid_season = PhenologyState {
            moisture: unit(900),
            ..healthy_state(560)
        };
        assert_eq!(
            phenotype_weight_mille(PhenotypeRole::Fruiting, watered_fruit, mid_season),
            Some(1000)
        );
        assert_eq!(
            phenotype_weight_mille(
                PhenotypeRole::Fruiting,
                watered_fruit,
                PhenologyState {
                    moisture: unit(100),
                    ..mid_season
                }
            ),
            Some(0),
            "the dry plant fruits at zero even mid-season"
        );
    }
}
