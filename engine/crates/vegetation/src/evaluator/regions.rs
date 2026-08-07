//! Cell, stage-region, and bounds algebra.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{
    BASE_CELL_TICKS, DecisionScalar, WorldBounds, WorldCellKey, WorldPosition, div_round_ties_even,
};

use crate::hash::sha256;
use crate::{CompiledGraphNode, Error, Result};

pub(super) fn canonical_owner(position: WorldPosition, level: u8) -> Result<WorldCellKey> {
    position.cell().ancestor(level).map_err(Into::into)
}

pub(super) fn cell_region_count(bounds: WorldBounds, level: u8) -> Result<usize> {
    let edge = i128::from(BASE_CELL_TICKS)
        .checked_mul(
            1_i128
                .checked_shl(u32::from(level))
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let minimum = bounds.min_ticks().map(|value| value.div_euclid(edge));
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| (value - 1).div_euclid(edge));
    minimum
        .into_iter()
        .zip(maximum)
        .try_fold(1_i128, |count, (minimum, maximum)| {
            count
                .checked_mul(
                    maximum
                        .checked_sub(minimum)
                        .and_then(|extent| extent.checked_add(1))
                        .ok_or(Error::NumericOverflow)?,
                )
                .ok_or(Error::NumericOverflow)
        })?
        .try_into()
        .map_err(|_| Error::NumericOverflow)
}

pub(super) fn canonical_cell_regions(
    bounds: WorldBounds,
    level: u8,
    hierarchy_namespace: u128,
) -> Result<Vec<EvaluationRegion>> {
    let edge = i128::from(BASE_CELL_TICKS)
        .checked_mul(
            1_i128
                .checked_shl(u32::from(level))
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let minimum = bounds.min_ticks().map(|value| value.div_euclid(edge));
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| (value - 1).div_euclid(edge));
    let mut regions = Vec::new();
    crate::memory::reserve_exact(
        &mut regions,
        cell_region_count(bounds, level)?,
        "canonical cell regions",
    )?;
    for x in minimum[0]..=maximum[0] {
        for y in minimum[1]..=maximum[1] {
            for z in minimum[2]..=maximum[2] {
                let cell = WorldCellKey::new(
                    i64::try_from(x).map_err(|_| Error::NumericOverflow)?,
                    i64::try_from(y).map_err(|_| Error::NumericOverflow)?,
                    i64::try_from(z).map_err(|_| Error::NumericOverflow)?,
                    level,
                )?;
                let cell_bounds = intersect_bounds(bounds, cell.bounds())?.ok_or_else(|| {
                    Error::GraphDocument {
                        path: "evaluation.regions".to_owned(),
                        reason: "enumerated region cell does not intersect its source bounds"
                            .to_owned(),
                    }
                })?;
                regions.push(EvaluationRegion {
                    id: hierarchical_region_identity(hierarchy_namespace, cell),
                    kind: EvaluationRegionKind::Biome,
                    layer: hierarchy_namespace,
                    hierarchy_namespace: Some(hierarchy_namespace),
                    seed_cell: cell,
                    bounds: cell_bounds,
                });
            }
        }
    }
    Ok(regions)
}

pub(super) fn stage_regions(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<Vec<EvaluationRegion>> {
    let level = node.definition.spatial.level();
    let minimum_input_level = state
        .scope
        .current_global_stage()
        .map_or(state.inputs.output_cell.level(), |stage| {
            stage.minimum_input_level
        });
    if level < minimum_input_level {
        return Ok(Vec::new());
    }
    let mut result = BTreeMap::<(u8, u128, WorldCellKey), EvaluationRegion>::new();
    for region in regions {
        if region.seed_cell.level() > level {
            return Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "region seed cell is coarser than the node stage".to_owned(),
            });
        }
        let stage_cell = region.seed_cell.ancestor(level)?;
        let (kind, source, id) = if let Some(namespace) = region.hierarchy_namespace {
            (
                0,
                namespace,
                hierarchical_region_identity(namespace, stage_cell),
            )
        } else {
            (
                1,
                region.id,
                independent_stage_region_identity(region.id, stage_cell),
            )
        };
        let bounds = intersect_bounds(region.bounds, stage_cell.bounds())?;
        if let Some(bounds) = bounds {
            let key = (kind, source, stage_cell);
            if let Some(existing) = result.get_mut(&key) {
                existing.bounds = union_bounds(existing.bounds, bounds)?;
            } else {
                result.insert(
                    key,
                    EvaluationRegion {
                        id,
                        kind: region.kind,
                        layer: region.layer,
                        hierarchy_namespace: region.hierarchy_namespace,
                        seed_cell: stage_cell,
                        bounds,
                    },
                );
            }
        }
    }
    let mut regions = Vec::new();
    crate::memory::reserve_exact(&mut regions, result.len(), "canonical stage regions")?;
    regions.extend(result.into_values());
    Ok(regions)
}

fn hierarchical_region_identity(namespace: u128, cell: WorldCellKey) -> u128 {
    let hash = sha256(
        &[
            b"saffron-anima/hierarchical-region/v1\0".as_slice(),
            namespace.to_be_bytes().as_slice(),
            cell.canonical_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

fn independent_stage_region_identity(region: u128, cell: WorldCellKey) -> u128 {
    let hash = sha256(
        &[
            b"saffron-anima/independent-stage-region/v1\0".as_slice(),
            region.to_be_bytes().as_slice(),
            cell.canonical_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

fn union_bounds(left: WorldBounds, right: WorldBounds) -> Result<WorldBounds> {
    let minimum = std::array::from_fn(|axis| left.min_ticks()[axis].min(right.min_ticks()[axis]));
    let maximum = std::array::from_fn(|axis| {
        left.max_ticks_exclusive()[axis].max(right.max_ticks_exclusive()[axis])
    });
    WorldBounds::new(minimum, maximum).map_err(Into::into)
}

pub(super) fn intersect_bounds(
    left: WorldBounds,
    right: WorldBounds,
) -> Result<Option<WorldBounds>> {
    let minimum = std::array::from_fn(|axis| left.min_ticks()[axis].max(right.min_ticks()[axis]));
    let maximum = std::array::from_fn(|axis| {
        left.max_ticks_exclusive()[axis].min(right.max_ticks_exclusive()[axis])
    });
    if (0..3).any(|axis| minimum[axis] >= maximum[axis]) {
        Ok(None)
    } else {
        WorldBounds::new(minimum, maximum)
            .map(Some)
            .map_err(Into::into)
    }
}

pub(super) fn expand_bounds_checked(bounds: WorldBounds, radius: i128) -> Result<WorldBounds> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut expanded_minimum = [0_i128; 3];
    let mut expanded_maximum = [0_i128; 3];
    for axis in 0..3 {
        expanded_minimum[axis] = minimum[axis]
            .checked_sub(radius)
            .ok_or(Error::NumericOverflow)?;
        expanded_maximum[axis] = maximum[axis]
            .checked_add(radius)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldBounds::new(expanded_minimum, expanded_maximum).map_err(Into::into)
}

pub(super) fn bounds_contains_bounds(container: WorldBounds, contained: WorldBounds) -> bool {
    (0..3).all(|axis| {
        container.min_ticks()[axis] <= contained.min_ticks()[axis]
            && container.max_ticks_exclusive()[axis] >= contained.max_ticks_exclusive()[axis]
    })
}

pub(super) fn prototype_bounds(
    prototype: &PlantPrototype,
    candidate: &GraphCandidate,
) -> Result<WorldBounds> {
    let [x, y, z, w] = candidate.orientation.bits().map(i128::from);
    let scale = i128::from(i16::MAX);
    let scale_squared = scale.checked_mul(scale).ok_or(Error::NumericOverflow)?;
    let twice = |value: i128| value.checked_mul(2).ok_or(Error::NumericOverflow);
    let matrix = [
        [
            scale_squared
                .checked_sub(twice(y * y + z * z)?)
                .ok_or(Error::NumericOverflow)?,
            twice(x * y - z * w)?,
            twice(x * z + y * w)?,
        ],
        [
            twice(x * y + z * w)?,
            scale_squared
                .checked_sub(twice(x * x + z * z)?)
                .ok_or(Error::NumericOverflow)?,
            twice(y * z - x * w)?,
        ],
        [
            twice(x * z - y * w)?,
            twice(y * z + x * w)?,
            scale_squared
                .checked_sub(twice(x * x + y * y)?)
                .ok_or(Error::NumericOverflow)?,
        ],
    ];
    let position = candidate.position.global_ticks();
    let mut minimum = [i128::MAX; 3];
    let mut maximum = [i128::MIN; 3];
    for corner in 0..8 {
        let local: [DecisionScalar; 3] = std::array::from_fn(|axis| {
            if corner & (1 << axis) == 0 {
                prototype.local_bounds_min[axis]
            } else {
                prototype.local_bounds_max[axis]
            }
        });
        let scaled = [
            local[0].checked_mul(candidate.scale[0])?.bits(),
            local[1].checked_mul(candidate.scale[1])?.bits(),
            local[2].checked_mul(candidate.scale[2])?.bits(),
        ];
        for axis in 0..3 {
            let rotated =
                matrix[axis]
                    .iter()
                    .zip(scaled)
                    .try_fold(0_i128, |sum, (coefficient, value)| {
                        sum.checked_add(
                            coefficient
                                .checked_mul(i128::from(value))
                                .ok_or(Error::NumericOverflow)?,
                        )
                        .ok_or(Error::NumericOverflow)
                    })?;
            let fixed = DecisionScalar::from_bits(
                i32::try_from(div_round_ties_even(rotated, scale_squared)?)
                    .map_err(|_| Error::NumericOverflow)?,
            );
            let world = position[axis]
                .checked_add(fixed_meters_to_ticks(fixed)?)
                .ok_or(Error::NumericOverflow)?;
            minimum[axis] = minimum[axis].min(world);
            maximum[axis] = maximum[axis].max(world);
        }
    }
    let [maximum_x, maximum_y, maximum_z] =
        maximum.map(|value| value.checked_add(1).ok_or(Error::NumericOverflow));
    let maximum = [maximum_x?, maximum_y?, maximum_z?];
    WorldBounds::new(minimum, maximum).map_err(Into::into)
}

pub(super) fn candidate_bounds(
    candidate: &GraphCandidate,
    state: &EvaluationState<'_>,
) -> Result<WorldBounds> {
    if let Some(point) = &candidate.authored_point {
        return Ok(point.bounds);
    }
    let family = candidate
        .family
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: candidate.identity.node,
            input: "plant family before bounds-overlap".to_owned(),
        })?;
    prototype_bounds(prototype_for_family(state, family)?, candidate)
}

pub(super) fn bounds_support_radius(position: WorldPosition, bounds: WorldBounds) -> Result<i128> {
    let point = position.global_ticks();
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    (0..3).try_fold(0_i128, |radius, axis| {
        let low = point[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let high = maximum[axis]
            .checked_sub(1)
            .and_then(|value| value.checked_sub(point[axis]))
            .ok_or(Error::NumericOverflow)?;
        Ok(radius
            .max(low.checked_abs().ok_or(Error::NumericOverflow)?)
            .max(high.checked_abs().ok_or(Error::NumericOverflow)?))
    })
}

pub(super) fn bounds_intersect(left: WorldBounds, right: WorldBounds) -> bool {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    (0..3).all(|axis| {
        left_minimum[axis] < right_maximum[axis] && right_minimum[axis] < left_maximum[axis]
    })
}

pub(super) fn touches_boundary(bounds: WorldBounds, cell: WorldBounds) -> bool {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let cell_minimum = cell.min_ticks();
    let cell_maximum = cell.max_ticks_exclusive();
    (0..3).any(|axis| minimum[axis] <= cell_minimum[axis] || maximum[axis] >= cell_maximum[axis])
}
