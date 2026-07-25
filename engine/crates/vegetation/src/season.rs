//! The deterministic seasonal signal: a per-mille phase of the year derived from
//! the calendar date and hemisphere, and the window test seasonal phenotypes use.

use crate::asset::PhenotypeRole;
use crate::point::PlantLifecycle;

/// Days in each Gregorian month of a non-leap year, cumulative before the month.
const CUMULATIVE_DAYS: [u16; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

fn leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The per-mille seasonal phase (`0..1000`) of a calendar date at a latitude:
/// the day-of-year fraction, shifted half a year in the southern hemisphere so a
/// window authored against northern seasons reads the same everywhere.
pub fn season_phase_mille(year: i32, month: i32, day: i32, latitude_deg: f32) -> u16 {
    let month_index = (month - 1).clamp(0, 11) as usize;
    let leap = u16::from(leap_year(year) && month_index >= 2);
    let day_of_year =
        CUMULATIVE_DAYS[month_index] + leap + (day.clamp(1, 31) as u16).saturating_sub(1);
    let days_in_year = if leap_year(year) { 366 } else { 365 };
    let mut mille = u32::from(day_of_year) * 1000 / days_in_year;
    if latitude_deg < 0.0 {
        mille = (mille + 500) % 1000;
    }
    mille.min(999) as u16
}

/// The effective seasonal window of a phenotype role in per-mille of the year,
/// wrapping through 1000: the authored window when present, else the role's
/// default. Non-seasonal roles have no window and never activate from season alone.
pub fn role_season_window(role: PhenotypeRole, authored: Option<(u16, u16)>) -> Option<(u16, u16)> {
    if let Some(window) = authored {
        return Some(window);
    }
    match role {
        PhenotypeRole::Flowering => Some((200, 450)),
        PhenotypeRole::Fruiting => Some((450, 700)),
        PhenotypeRole::Senescent => Some((700, 950)),
        PhenotypeRole::Healthy
        | PhenotypeRole::Harvested
        | PhenotypeRole::Damaged
        | PhenotypeRole::Burned
        | PhenotypeRole::Dead
        | PhenotypeRole::Wet => None,
    }
}

/// Whether a per-mille season phase falls inside a wrapping window.
pub fn season_in_window(phase_mille: u16, window: (u16, u16)) -> bool {
    let (start, end) = window;
    if start <= end {
        (start..end).contains(&phase_mille)
    } else {
        phase_mille >= start || phase_mille < end
    }
}

/// The phenotype an instance renders, from typed lifecycle state and the seasonal
/// signal — never inferred from an active mesh. Dead/stump lifecycles take the Dead
/// role, senescent takes the Senescent role, and a healthy plant takes the first
/// phenotype whose seasonal window contains the current phase; the cooked phenotype
/// is the fallback throughout. `phenotypes` yields `(id, role, authored window)`.
pub fn resolve_rendered_phenotype(
    phenotypes: impl Iterator<Item = (u32, PhenotypeRole, Option<(u16, u16)>)> + Clone,
    cooked: u32,
    lifecycle: PlantLifecycle,
    season_mille: u16,
) -> u32 {
    let by_role = |role: PhenotypeRole| {
        phenotypes
            .clone()
            .find(|(_, candidate, _)| *candidate == role)
            .map(|(id, _, _)| id)
    };
    match lifecycle {
        PlantLifecycle::Dead | PlantLifecycle::Stump => {
            return by_role(PhenotypeRole::Dead).unwrap_or(cooked);
        }
        PlantLifecycle::Senescent => {
            return by_role(PhenotypeRole::Senescent).unwrap_or(cooked);
        }
        _ => {}
    }
    phenotypes
        .clone()
        .find(|(_, role, authored)| {
            role_season_window(*role, *authored)
                .is_some_and(|window| season_in_window(season_mille, window))
        })
        .map(|(id, _, _)| id)
        .unwrap_or(cooked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_tracks_the_calendar_and_flips_hemispheres() {
        let north_jan = season_phase_mille(2026, 1, 1, 55.0);
        let north_jul = season_phase_mille(2026, 7, 2, 55.0);
        assert_eq!(north_jan, 0);
        assert!((495..=505).contains(&north_jul), "{north_jul}");
        let south_jan = season_phase_mille(2026, 1, 1, -30.0);
        assert_eq!(south_jan, 500);
        // A leap year keeps December inside the year.
        assert!(season_phase_mille(2024, 12, 31, 0.0) < 1000);
    }

    #[test]
    fn the_rendered_phenotype_follows_lifecycle_then_season() {
        let phenotypes = [
            (0, PhenotypeRole::Healthy, None),
            (1, PhenotypeRole::Senescent, None),
            (2, PhenotypeRole::Dead, None),
        ];
        let resolve = |lifecycle, season| {
            resolve_rendered_phenotype(phenotypes.iter().copied(), 0, lifecycle, season)
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
        // No matching role falls back to the cooked phenotype.
        let healthy_only = [(7, PhenotypeRole::Healthy, None)];
        assert_eq!(
            resolve_rendered_phenotype(healthy_only.iter().copied(), 7, PlantLifecycle::Dead, 0),
            7
        );
    }

    #[test]
    fn windows_wrap_and_roles_default() {
        assert!(season_in_window(720, (700, 950)));
        assert!(!season_in_window(500, (700, 950)));
        assert!(season_in_window(980, (950, 100)));
        assert!(season_in_window(50, (950, 100)));
        assert!(!season_in_window(500, (950, 100)));
        assert_eq!(
            role_season_window(PhenotypeRole::Senescent, None),
            Some((700, 950))
        );
        assert_eq!(
            role_season_window(PhenotypeRole::Senescent, Some((100, 300))),
            Some((100, 300))
        );
        assert_eq!(role_season_window(PhenotypeRole::Healthy, None), None);
    }
}
