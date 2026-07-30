//! World-bounds arithmetic the evaluation scopes are built from: intersection, halo expansion,
//! and the canonical hierarchical cell enumerations a graph reads through.

use std::collections::BTreeSet;

use saffron_spatial::{
    DecisionScalar, LOCAL_TICKS_PER_METER, WorldBounds, WorldCellKey, div_round_ties_even,
    world_cells_covering_bounds,
};
use saffron_vegetation::{EvaluationRegion, EvaluationRegionKind, vegetation_content_hash};

use crate::{Error, Result};

pub(super) fn intersect_bounds(left: WorldBounds, right: WorldBounds) -> Option<WorldBounds> {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    let minimum = std::array::from_fn(|axis| left_minimum[axis].max(right_minimum[axis]));
    let maximum = std::array::from_fn(|axis| left_maximum[axis].min(right_maximum[axis]));
    WorldBounds::new(minimum, maximum).ok()
}

pub(super) fn expand_bounds(bounds: WorldBounds, halo: DecisionScalar) -> Result<WorldBounds> {
    let halo_ticks = i128::try_from(
        div_round_ties_even(
            i128::from(halo.bits())
                .checked_mul(i128::from(LOCAL_TICKS_PER_METER))
                .ok_or(Error::Vegetation(
                    saffron_vegetation::Error::NumericOverflow,
                ))?,
            65_536,
        )?
        .unsigned_abs(),
    )
    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut expanded_minimum = [0_i128; 3];
    let mut expanded_maximum = [0_i128; 3];
    for axis in 0..3 {
        expanded_minimum[axis] = minimum[axis]
            .checked_sub(halo_ticks)
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
        expanded_maximum[axis] = maximum[axis]
            .checked_add(halo_ticks)
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
    }
    Ok(WorldBounds::new(expanded_minimum, expanded_maximum)?)
}

pub(super) fn canonical_hierarchical_regions(
    bounds: WorldBounds,
    level: u8,
    namespace: u128,
    limit: u64,
) -> Result<Vec<EvaluationRegion>> {
    world_cells_covering_bounds(bounds, level, limit)
        .map_err(|error| match error {
            saffron_spatial::Error::CellEnumerationLimit { requested, limit } => {
                Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                    resource: "input region cells",
                    requested,
                    limit,
                })
            }
            error => Error::Spatial(error),
        })?
        .into_iter()
        .map(|cell| {
            let cell_bounds = intersect_bounds(bounds, cell.bounds()).ok_or_else(|| {
                Error::Io(
                    "enumerated hierarchical region does not intersect its source bounds"
                        .to_owned(),
                )
            })?;
            let hash = vegetation_content_hash(
                &[
                    b"saffron-anima/hierarchical-region/v1\0".as_slice(),
                    namespace.to_be_bytes().as_slice(),
                    cell.canonical_bytes().as_slice(),
                ]
                .concat(),
            );
            Ok(EvaluationRegion {
                id: u128::from_be_bytes(hash[..16].try_into().unwrap()),
                kind: EvaluationRegionKind::Biome,
                layer: namespace,
                hierarchy_namespace: Some(namespace),
                seed_cell: cell,
                bounds: cell_bounds,
            })
        })
        .collect()
}

pub(super) fn insert_global_owners(
    read_bounds: WorldBounds,
    owner_level: u8,
    stage_index: usize,
    owners_by_stage: &mut [BTreeSet<WorldCellKey>],
    global_tile_count: &mut u64,
    limit: u64,
) -> Result<()> {
    let owners =
        world_cells_covering_bounds(read_bounds, owner_level, limit).map_err(
            |error| match error {
                saffron_spatial::Error::CellEnumerationLimit { requested, limit } => {
                    Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                        resource: "global stage tiles",
                        requested,
                        limit,
                    })
                }
                error => Error::Spatial(error),
            },
        )?;
    let stage_owners = owners_by_stage
        .get_mut(stage_index)
        .ok_or_else(|| Error::Io("compiled global-stage index is invalid".to_owned()))?;
    for owner in owners {
        if !stage_owners.insert(owner) {
            continue;
        }
        *global_tile_count = global_tile_count.checked_add(1).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
        if *global_tile_count > limit {
            return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                resource: "global stage tiles",
                requested: *global_tile_count,
                limit,
            }));
        }
    }
    Ok(())
}
