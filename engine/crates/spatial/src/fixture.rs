//! Shared seam/origin fixtures and the later vegetation stress-scene specification.

use crate::{WorldCellKey, WorldPosition};

/// A named exact-coordinate fixture used by spatial and vegetation tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpatialFixture {
    /// Stable fixture name.
    pub name: &'static str,
    /// Exact query or object position.
    pub position: WorldPosition,
    /// Expected canonical owner.
    pub owner: WorldCellKey,
}

impl SpatialFixture {
    /// Seam, negative-coordinate, and render-origin fixtures.
    #[must_use]
    pub fn canonical() -> Vec<Self> {
        let ticks = i128::from(crate::BASE_CELL_TICKS);
        [
            ("origin", [0, 0, 0]),
            ("positive-face", [ticks, 0, 0]),
            ("negative-neighbour", [-1, -ticks, -ticks - 1]),
            ("far-origin", [ticks * 1_000_000, 17, -ticks * 1_000_000]),
        ]
        .into_iter()
        .map(|(name, point)| {
            let position =
                WorldPosition::from_global_ticks(point).expect("fixture is representable");
            Self {
                name,
                owner: position.cell(),
                position,
            }
        })
        .collect()
    }
}

/// Scale and content mix for the representative vegetation production gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForestBaselineSpec {
    /// Covered square kilometres.
    pub area_square_km: u32,
    /// Persistent macro plants.
    pub macro_plants: u64,
    /// Reconstructed micro instances at peak density.
    pub micro_instances: u64,
    /// Distinct plant species.
    pub species: u32,
    /// Authored/manual plant fraction in basis points.
    pub authored_basis_points: u16,
    /// Dynamic/promoted plant fraction in basis points.
    pub dynamic_basis_points: u16,
}

/// The fixed heterogeneous forest scale used by later phase gates.
pub const FOREST_BASELINE_SPEC: ForestBaselineSpec = ForestBaselineSpec {
    area_square_km: 64,
    macro_plants: 10_000_000,
    micro_instances: 2_000_000_000,
    species: 128,
    authored_basis_points: 500,
    dynamic_basis_points: 25,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_owners_are_derived_by_the_canonical_rule() {
        for fixture in SpatialFixture::canonical() {
            assert_eq!(fixture.position.cell(), fixture.owner, "{}", fixture.name);
        }
    }

    #[test]
    fn forest_spec_is_the_production_scale() {
        const {
            assert!(FOREST_BASELINE_SPEC.macro_plants == 10_000_000);
            assert!(FOREST_BASELINE_SPEC.micro_instances > FOREST_BASELINE_SPEC.macro_plants);
        }
    }
}
