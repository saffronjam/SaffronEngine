//! Integer geometry, quaternion, and tile-addressing helpers.

use super::*;

use saffron_spatial::{
    DecisionScalar, FieldChannel, LOCAL_TICKS_PER_METER, SignedUnit, UnitInterval, WorldBounds,
    WorldPosition, div_round_ties_even,
};

use crate::canonical::CanonicalSink;
use crate::hash::sha256;
use crate::{CompiledGraphNode, Error, QuantizedOrientation, Result};

pub(super) fn encode_world_bounds<S: CanonicalSink>(
    sink: &mut S,
    bounds: WorldBounds,
) -> Result<()> {
    for tick in bounds.min_ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    for tick in bounds.max_ticks_exclusive() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

pub(super) fn encode_world_position<S: CanonicalSink>(
    sink: &mut S,
    position: WorldPosition,
) -> Result<()> {
    sink.write(&position.cell().canonical_bytes())?;
    for tick in position.local().ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

pub(super) fn tile_index(
    position: WorldPosition,
    bounds: WorldBounds,
    dimensions: [u32; 3],
) -> Result<usize> {
    if dimensions.contains(&0) || !bounds.contains(position) {
        return Err(Error::GraphDocument {
            path: "micro-output".to_owned(),
            reason: "invalid dimensions or foreign point".to_owned(),
        });
    }
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let point = position.global_ticks();
    let mut coordinate = [0_u32; 3];
    for axis in 0..3 {
        let span = maximum[axis] - minimum[axis];
        let offset = point[axis] - minimum[axis];
        let scaled = offset
            .checked_mul(i128::from(dimensions[axis]))
            .ok_or(Error::NumericOverflow)?
            / span;
        coordinate[axis] = u32::try_from(scaled)
            .map_err(|_| Error::NumericOverflow)?
            .min(dimensions[axis] - 1);
    }
    let index = u64::from(coordinate[0])
        .checked_mul(u64::from(dimensions[1]))
        .and_then(|value| value.checked_add(u64::from(coordinate[1])))
        .and_then(|value| value.checked_mul(u64::from(dimensions[2])))
        .and_then(|value| value.checked_add(u64::from(coordinate[2])))
        .ok_or(Error::NumericOverflow)?;
    usize::try_from(index).map_err(|_| Error::NumericOverflow)
}

pub(super) fn packed_sample_count(dimensions: [u32; 3]) -> Result<u64> {
    let count = dimensions.into_iter().try_fold(1_u64, |product, value| {
        product
            .checked_mul(u64::from(value))
            .ok_or(Error::NumericOverflow)
    })?;
    if count == 0 {
        return Err(Error::GraphDocument {
            path: "evaluation.tile.dimensions".to_owned(),
            reason: "tile dimensions must be non-zero".to_owned(),
        });
    }
    Ok(count)
}

pub(super) fn tile_sample_position(
    bounds: WorldBounds,
    dimensions: [u32; 3],
    index: u64,
) -> Result<WorldPosition> {
    let count = packed_sample_count(dimensions)?;
    if index >= count {
        return Err(Error::NumericOverflow);
    }
    let yz = u64::from(dimensions[1])
        .checked_mul(u64::from(dimensions[2]))
        .ok_or(Error::NumericOverflow)?;
    let coordinates = [
        index / yz,
        (index % yz) / u64::from(dimensions[2]),
        index % u64::from(dimensions[2]),
    ];
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut ticks = [0_i128; 3];
    for axis in 0..3 {
        let span = maximum[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let numerator = i128::from(coordinates[axis])
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| value.checked_mul(span))
            .ok_or(Error::NumericOverflow)?;
        let denominator = i128::from(dimensions[axis])
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?;
        let offset = div_round_ties_even(numerator, denominator)?.min(span - 1);
        ticks[axis] = minimum[axis]
            .checked_add(offset)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

pub(super) fn field_channel_name(channel: FieldChannel) -> &'static str {
    match channel {
        FieldChannel::Altitude => "altitude",
        FieldChannel::Slope => "slope",
        FieldChannel::Curvature => "curvature",
        FieldChannel::Concavity => "concavity",
        FieldChannel::Drainage => "drainage",
        FieldChannel::Moisture => "moisture",
        FieldChannel::Temperature => "temperature",
        FieldChannel::Precipitation => "precipitation",
        FieldChannel::Sunlight => "sunlight",
        FieldChannel::Exposure => "exposure",
        FieldChannel::WaterDistance => "water-distance",
        FieldChannel::WaterDepth => "water-depth",
        FieldChannel::SignedBlocker => "signed-blocker",
        FieldChannel::SplineDistance => "spline-distance",
        FieldChannel::User(_) => "user",
    }
}

pub(super) fn orientation_from_normal_and_yaw(
    normal: Option<[SignedUnit; 3]>,
    yaw: UnitInterval,
) -> Result<QuantizedOrientation> {
    let Some(normal) = normal else {
        return yaw_orientation(yaw);
    };
    let n = normal.map(|value| i64::from(value.bits()));
    let unit = i64::from(i16::MAX);
    let align = if n[1] <= -unit + 1 {
        [unit, 0, 0, 0]
    } else {
        normalize_quaternion_q15([n[2], 0, -n[0], unit + n[1]])?
    };
    let yaw = yaw_quaternion_q15(yaw)?;
    let product = multiply_quaternion_q15(align, yaw)?;
    Ok(QuantizedOrientation::new(
        product
            .map(i16::try_from)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::NumericOverflow)?
            .try_into()
            .map_err(|_| Error::NumericOverflow)?,
    )?)
}

pub(super) fn yaw_orientation(yaw: UnitInterval) -> Result<QuantizedOrientation> {
    let quaternion = yaw_quaternion_q15(yaw)?;
    Ok(QuantizedOrientation::new(
        quaternion
            .map(i16::try_from)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::NumericOverflow)?
            .try_into()
            .map_err(|_| Error::NumericOverflow)?,
    )?)
}

fn yaw_quaternion_q15(yaw: UnitInterval) -> Result<[i64; 4]> {
    let half_angle = div_round_ties_even(
        i128::from(yaw.bits()) * (1_i128 << 32),
        i128::from(u16::MAX) * 2,
    )?;
    let (cosine, sine) =
        cordic_sin_cos(u32::try_from(half_angle).map_err(|_| Error::NumericOverflow)?);
    normalize_quaternion_q15([0, q30_to_q15(sine)?, 0, q30_to_q15(cosine)?])
}

pub(super) fn cordic_sin_cos(mut angle: u32) -> (i64, i64) {
    const ATAN: [i64; 16] = [
        0x2000_0000,
        0x12E4_051D,
        0x09FB_385B,
        0x0511_11D4,
        0x028B_0D43,
        0x0145_D7E1,
        0x00A2_F61E,
        0x0051_7C55,
        0x0028_BE53,
        0x0014_5F2F,
        0x000A_2F98,
        0x0005_17CC,
        0x0002_8BE6,
        0x0001_45F3,
        0x0000_A2FA,
        0x0000_517D,
    ];
    let mut negate = false;
    if angle > 0x4000_0000 && angle < 0xC000_0000 {
        angle = angle.wrapping_sub(0x8000_0000);
        negate = true;
    }
    let mut x = 652_032_874_i64;
    let mut y = 0_i64;
    let mut z = i64::from(angle as i32);
    for (index, atan) in ATAN.into_iter().enumerate() {
        let direction = if z >= 0 { 1 } else { -1 };
        let next_x = x - direction * (y >> index);
        let next_y = y + direction * (x >> index);
        x = next_x;
        y = next_y;
        z -= direction * atan;
    }
    if negate { (-x, -y) } else { (x, y) }
}

fn q30_to_q15(value: i64) -> Result<i64> {
    let rounded = div_round_ties_even(i128::from(value) * i128::from(i16::MAX), 1_i128 << 30)?;
    i64::try_from(rounded).map_err(|_| Error::NumericOverflow)
}

fn normalize_quaternion_q15(value: [i64; 4]) -> Result<[i64; 4]> {
    let length_squared = value.iter().try_fold(0_i128, |sum, lane| {
        sum.checked_add(i128::from(*lane) * i128::from(*lane))
            .ok_or(Error::NumericOverflow)
    })?;
    if length_squared == 0 {
        return Err(Error::NumericOverflow);
    }
    let length = integer_sqrt(length_squared);
    let mut result = [0_i64; 4];
    for index in 0..4 {
        let lane = div_round_ties_even(i128::from(value[index]) * i128::from(i16::MAX), length)?;
        result[index] = i64::try_from(lane)
            .map_err(|_| Error::NumericOverflow)?
            .clamp(-i64::from(i16::MAX), i64::from(i16::MAX));
    }
    Ok(result)
}

fn multiply_quaternion_q15(left: [i64; 4], right: [i64; 4]) -> Result<[i64; 4]> {
    let [lx, ly, lz, lw] = left;
    let [rx, ry, rz, rw] = right;
    let raw = [
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ];
    let scaled = raw.map(|value| {
        div_round_ties_even(i128::from(value), i128::from(i16::MAX)).and_then(|value| {
            i64::try_from(value).map_err(|_| saffron_spatial::Error::NumericOverflow)
        })
    });
    let scaled = scaled
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .try_into()
        .unwrap();
    normalize_quaternion_q15(scaled)
}

pub(super) fn interpolate_unit(
    start: UnitInterval,
    end: UnitInterval,
    weight: UnitInterval,
) -> Result<UnitInterval> {
    let delta = i128::from(end.bits()) - i128::from(start.bits());
    let value = i128::from(start.bits())
        + div_round_ties_even(delta * i128::from(weight.bits()), i128::from(u16::MAX))?;
    Ok(UnitInterval::from_bits(
        u16::try_from(value).map_err(|_| Error::NumericOverflow)?,
    ))
}

pub(super) fn offset_fixed(
    position: WorldPosition,
    offset: [DecisionScalar; 3],
) -> Result<WorldPosition> {
    offset_ticks(
        position,
        [
            fixed_meters_to_ticks(offset[0])?,
            fixed_meters_to_ticks(offset[1])?,
            fixed_meters_to_ticks(offset[2])?,
        ],
    )
}

pub(super) fn offset_ticks(position: WorldPosition, offset: [i128; 3]) -> Result<WorldPosition> {
    let ticks = position.global_ticks();
    WorldPosition::from_global_ticks([
        ticks[0]
            .checked_add(offset[0])
            .ok_or(Error::NumericOverflow)?,
        ticks[1]
            .checked_add(offset[1])
            .ok_or(Error::NumericOverflow)?,
        ticks[2]
            .checked_add(offset[2])
            .ok_or(Error::NumericOverflow)?,
    ])
    .map_err(Into::into)
}

pub(super) fn fixed_meters_to_ticks(value: DecisionScalar) -> Result<i128> {
    div_round_ties_even(
        i128::from(value.bits()) * i128::from(LOCAL_TICKS_PER_METER),
        65_536,
    )
    .map_err(Into::into)
}

pub(super) fn ensure_support_ticks(node: &CompiledGraphNode, requested: i128) -> Result<()> {
    let crate::NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.definition.spatial
    else {
        return Ok(());
    };
    let limit = fixed_meters_to_ticks(influence_radius)?
        .checked_abs()
        .ok_or(Error::NumericOverflow)?;
    let requested = requested.checked_abs().ok_or(Error::NumericOverflow)?;
    if requested <= limit {
        return Ok(());
    }
    Err(Error::GraphLimit {
        resource: "node influence radius ticks",
        requested: u64::try_from(requested).unwrap_or(u64::MAX),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
    })
}

pub(super) fn ticks_to_fixed_meters(value: i128) -> Result<i32> {
    let bits = div_round_ties_even(
        value.checked_mul(65_536).ok_or(Error::NumericOverflow)?,
        i128::from(LOCAL_TICKS_PER_METER),
    )?;
    i32::try_from(bits).map_err(|_| Error::NumericOverflow)
}

pub(super) fn distance_squared_xz(left: WorldPosition, right: WorldPosition) -> Result<i128> {
    let left = left.global_ticks();
    let right = right.global_ticks();
    let dx = left[0]
        .checked_sub(right[0])
        .ok_or(Error::NumericOverflow)?;
    let dz = left[2]
        .checked_sub(right[2])
        .ok_or(Error::NumericOverflow)?;
    dx.checked_mul(dx)
        .and_then(|value| value.checked_add(dz.checked_mul(dz)?))
        .ok_or(Error::NumericOverflow)
}

pub(super) fn xz_bucket(position: WorldPosition, edge: i128) -> (i128, i128) {
    let ticks = position.global_ticks();
    (ticks[0].div_euclid(edge), ticks[2].div_euclid(edge))
}

pub(super) fn xyz_bucket(position: WorldPosition, edge: i128) -> (i128, i128, i128) {
    let ticks = position.global_ticks();
    (
        ticks[0].div_euclid(edge),
        ticks[1].div_euclid(edge),
        ticks[2].div_euclid(edge),
    )
}

pub(super) fn offset_xz_bucket(bucket: (i128, i128), x: i8, z: i8) -> Result<(i128, i128)> {
    Ok((
        bucket
            .0
            .checked_add(i128::from(x))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .1
            .checked_add(i128::from(z))
            .ok_or(Error::NumericOverflow)?,
    ))
}

pub(super) fn offset_xyz_bucket(
    bucket: (i128, i128, i128),
    x: i8,
    y: i8,
    z: i8,
) -> Result<(i128, i128, i128)> {
    Ok((
        bucket
            .0
            .checked_add(i128::from(x))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .1
            .checked_add(i128::from(y))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .2
            .checked_add(i128::from(z))
            .ok_or(Error::NumericOverflow)?,
    ))
}

pub(super) fn distance_squared(left: [i128; 3], right: [i128; 3]) -> Result<i128> {
    (0..3).try_fold(0_i128, |sum, axis| {
        let delta = left[axis]
            .checked_sub(right[axis])
            .ok_or(Error::NumericOverflow)?;
        sum.checked_add(delta.checked_mul(delta).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })
}

pub(super) fn point_segment_distance_ticks(
    point: WorldPosition,
    start: WorldPosition,
    end: WorldPosition,
) -> Result<i128> {
    let point = point.global_ticks();
    let start = start.global_ticks();
    let end = end.global_ticks();
    let direction = [
        end[0].checked_sub(start[0]).ok_or(Error::NumericOverflow)?,
        end[1].checked_sub(start[1]).ok_or(Error::NumericOverflow)?,
        end[2].checked_sub(start[2]).ok_or(Error::NumericOverflow)?,
    ];
    let length_squared = direction.iter().try_fold(0_i128, |sum, value| {
        sum.checked_add(value.checked_mul(*value).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })?;
    if length_squared == 0 {
        return Ok(integer_sqrt(distance_squared(point, start)?));
    }
    let offset = [
        point[0]
            .checked_sub(start[0])
            .ok_or(Error::NumericOverflow)?,
        point[1]
            .checked_sub(start[1])
            .ok_or(Error::NumericOverflow)?,
        point[2]
            .checked_sub(start[2])
            .ok_or(Error::NumericOverflow)?,
    ];
    let dot = (0..3).try_fold(0_i128, |sum, axis| {
        sum.checked_add(
            offset[axis]
                .checked_mul(direction[axis])
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)
    })?;
    let numerator = dot.clamp(0, length_squared);
    let mut closest = [0_i128; 3];
    for axis in 0..3 {
        let displacement = direction[axis]
            .checked_mul(numerator)
            .ok_or(Error::NumericOverflow)?
            / length_squared;
        closest[axis] = start[axis]
            .checked_add(displacement)
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(integer_sqrt(distance_squared(point, closest)?))
}

pub(super) fn point_bounds_distance_ticks(
    point: WorldPosition,
    bounds: WorldBounds,
) -> Result<i128> {
    let point = point.global_ticks();
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let squared = (0..3).try_fold(0_i128, |sum, axis| {
        let delta = if point[axis] < minimum[axis] {
            minimum[axis]
                .checked_sub(point[axis])
                .ok_or(Error::NumericOverflow)?
        } else if point[axis] >= maximum[axis] {
            point[axis]
                .checked_sub(maximum[axis])
                .and_then(|value| value.checked_add(1))
                .ok_or(Error::NumericOverflow)?
        } else {
            0
        };
        sum.checked_add(delta.checked_mul(delta).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })?;
    Ok(integer_sqrt(squared))
}

pub(super) fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }
    let mut low = 1_i128;
    let mut high = value.min(1_i128 << 64);
    while low <= high {
        let middle = low + (high - low) / 2;
        if middle <= value / middle {
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    high
}

pub(super) fn integer_sqrt_ceil(value: u64) -> u64 {
    let floor = integer_sqrt(i128::from(value)) as u64;
    if floor * floor == value {
        floor
    } else {
        floor + 1
    }
}

pub(super) fn candidate_key_u128(identity: CandidateIdentity) -> u128 {
    let hash = sha256(
        &[
            identity.node_address.to_be_bytes().as_slice(),
            identity.node.to_be_bytes().as_slice(),
            identity.node_semantic_revision.to_be_bytes().as_slice(),
            identity.ordinal.to_be_bytes().as_slice(),
            identity.ancestor.to_be_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

pub(super) fn push_len<S: CanonicalSink>(sink: &mut S, value: usize) -> Result<()> {
    sink.write(
        &u64::try_from(value)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    )
}
