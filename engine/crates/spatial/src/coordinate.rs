//! Exact hierarchical cells and render-relative world positions.

use std::fmt;
use std::str::FromStr;

use glam::{DVec3, Vec3};

use crate::{Error, Result};

/// The level-zero cell edge in metres.
pub const BASE_CELL_EDGE_METERS: f64 = 64.0;
/// Fractional bits in a metre of cell-local position.
pub const LOCAL_FRACTION_BITS: u32 = 12;
/// Cell-local ticks per metre (`1 / 4096 m`, about `0.244 mm`).
pub const LOCAL_TICKS_PER_METER: u32 = 1 << LOCAL_FRACTION_BITS;
/// Cell-local ticks along one level-zero cell edge.
pub const BASE_CELL_TICKS: u32 = 64 * LOCAL_TICKS_PER_METER;
/// The highest canonical hierarchy level.
pub const MAX_HIERARCHY_LEVEL: u8 = 62;

/// A logical world cell at an exact power-of-two hierarchy level.
///
/// Level zero spans one [`BASE_CELL_EDGE_METERS`] cube. Level `n` spans `2^n` base cells per
/// axis. Coordinates name cells at their own level and are restricted so their complete base-cell
/// extent remains representable by signed 64-bit base coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorldCellKey {
    coordinates: [i64; 3],
    level: u8,
}

impl WorldCellKey {
    /// Constructs a checked key.
    pub fn new(x: i64, y: i64, z: i64, level: u8) -> Result<Self> {
        if level > MAX_HIERARCHY_LEVEL {
            return Err(Error::HierarchyLevel(level));
        }
        let scale = 1_i128 << level;
        for coordinate in [x, y, z] {
            let first = i128::from(coordinate) * scale;
            let last = first + scale - 1;
            if first < i128::from(i64::MIN) || last > i128::from(i64::MAX) {
                return Err(Error::CellCoordinateRange);
            }
        }
        Ok(Self {
            coordinates: [x, y, z],
            level,
        })
    }

    /// Constructs a level-zero key.
    pub fn base(x: i64, y: i64, z: i64) -> Self {
        Self {
            coordinates: [x, y, z],
            level: 0,
        }
    }

    /// The signed cell coordinates at this key's level.
    #[must_use]
    pub fn coordinates(self) -> [i64; 3] {
        self.coordinates
    }

    /// The hierarchy level (`0` is the finest/base level).
    #[must_use]
    pub fn level(self) -> u8 {
        self.level
    }

    /// The parent cell, using Euclidean floor division for negative coordinates.
    pub fn parent(self) -> Result<Self> {
        if self.level == MAX_HIERARCHY_LEVEL {
            return Err(Error::CellOverflow);
        }
        Self::new(
            self.coordinates[0].div_euclid(2),
            self.coordinates[1].div_euclid(2),
            self.coordinates[2].div_euclid(2),
            self.level + 1,
        )
    }

    /// The child selected by three binary axis bits.
    pub fn child(self, child_bits: [u8; 3]) -> Result<Self> {
        if self.level == 0 || child_bits.iter().any(|&bit| bit > 1) {
            return Err(Error::CellOverflow);
        }
        let child_axis = |axis: usize| {
            self.coordinates[axis]
                .checked_mul(2)
                .and_then(|value| value.checked_add(i64::from(child_bits[axis])))
                .ok_or(Error::CellOverflow)
        };
        Self::new(
            child_axis(0)?,
            child_axis(1)?,
            child_axis(2)?,
            self.level - 1,
        )
    }

    /// The lexicographically minimum descendant at `target_level`.
    pub fn minimum_descendant(self, target_level: u8) -> Result<Self> {
        if target_level > self.level {
            return Err(Error::HierarchyLevel(target_level));
        }
        let shift = self.level - target_level;
        let scale = 1_i128 << shift;
        let coordinate = |axis: usize| {
            i64::try_from(
                i128::from(self.coordinates[axis])
                    .checked_mul(scale)
                    .ok_or(Error::CellOverflow)?,
            )
            .map_err(|_| Error::CellOverflow)
        };
        Self::new(
            coordinate(0)?,
            coordinate(1)?,
            coordinate(2)?,
            target_level,
        )
    }

    /// The ancestor at `target_level`.
    pub fn ancestor(self, target_level: u8) -> Result<Self> {
        if target_level < self.level || target_level > MAX_HIERARCHY_LEVEL {
            return Err(Error::HierarchyLevel(target_level));
        }
        let shift = target_level - self.level;
        let divisor = 1_i64 << shift;
        Self::new(
            self.coordinates[0].div_euclid(divisor),
            self.coordinates[1].div_euclid(divisor),
            self.coordinates[2].div_euclid(divisor),
            target_level,
        )
    }

    /// A same-level neighbour at the signed cell offset.
    pub fn neighbour(self, offset: [i64; 3]) -> Result<Self> {
        let axis = |index: usize| {
            self.coordinates[index]
                .checked_add(offset[index])
                .ok_or(Error::CellOverflow)
        };
        Self::new(axis(0)?, axis(1)?, axis(2)?, self.level)
    }

    /// The complete level-zero coordinate bounds, half-open on the maximum face.
    #[must_use]
    pub fn base_cell_bounds(self) -> ([i128; 3], [i128; 3]) {
        let scale = 1_i128 << self.level;
        let first = self.coordinates.map(|value| i128::from(value) * scale);
        let end = first.map(|value| value + scale);
        (first, end)
    }

    /// The complete world-tick bounds, half-open on the maximum face.
    #[must_use]
    pub fn bounds(self) -> WorldBounds {
        let (first, end) = self.base_cell_bounds();
        WorldBounds {
            min_ticks: first.map(|value| value * i128::from(BASE_CELL_TICKS)),
            max_ticks_exclusive: end.map(|value| value * i128::from(BASE_CELL_TICKS)),
        }
    }

    /// The canonical 25-byte key: level followed by 192-bit Morton-interleaved ZigZag axes.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 25] {
        let zigzag = self.coordinates.map(zigzag_encode);
        let mut bytes = [0_u8; 25];
        bytes[0] = self.level;
        let mut output_bit = 0_usize;
        for source_bit in (0..64).rev() {
            for axis in zigzag {
                let bit = ((axis >> source_bit) & 1) as u8;
                let byte = 1 + output_bit / 8;
                let shift = 7 - output_bit % 8;
                bytes[byte] |= bit << shift;
                output_bit += 1;
            }
        }
        bytes
    }

    /// Decodes and validates the canonical key bytes.
    pub fn from_canonical_bytes(bytes: [u8; 25]) -> Result<Self> {
        let level = bytes[0];
        if level > MAX_HIERARCHY_LEVEL {
            return Err(Error::InvalidCellEncoding);
        }
        let mut zigzag = [0_u64; 3];
        let mut input_bit = 0_usize;
        for target_bit in (0..64).rev() {
            for axis in &mut zigzag {
                let byte = 1 + input_bit / 8;
                let shift = 7 - input_bit % 8;
                *axis |= u64::from((bytes[byte] >> shift) & 1) << target_bit;
                input_bit += 1;
            }
        }
        Self::new(
            zigzag_decode(zigzag[0]),
            zigzag_decode(zigzag[1]),
            zigzag_decode(zigzag[2]),
            level,
        )
        .map_err(|_| Error::InvalidCellEncoding)
    }
}

/// Enumerates exact hierarchy cells intersecting a half-open bound under a hard count cap.
pub fn world_cells_covering_bounds(
    bounds: WorldBounds,
    level: u8,
    limit: u64,
) -> Result<Vec<WorldCellKey>> {
    if level > MAX_HIERARCHY_LEVEL {
        return Err(Error::HierarchyLevel(level));
    }
    let span = i128::from(BASE_CELL_TICKS)
        .checked_shl(u32::from(level))
        .ok_or(Error::NumericOverflow)?;
    let minimum_ticks = bounds.min_ticks();
    let maximum_ticks = bounds.max_ticks_exclusive();
    let mut minimum = [0_i64; 3];
    let mut maximum = [0_i64; 3];
    for axis in 0..3 {
        minimum[axis] = i64::try_from(minimum_ticks[axis].div_euclid(span))
            .map_err(|_| Error::NumericOverflow)?;
        maximum[axis] = i64::try_from(
            maximum_ticks[axis]
                .checked_sub(1)
                .ok_or(Error::NumericOverflow)?
                .div_euclid(span),
        )
        .map_err(|_| Error::NumericOverflow)?;
    }
    let count = (0..3).try_fold(1_u64, |count, axis| {
        let axis_count = maximum[axis]
            .checked_sub(minimum[axis])
            .and_then(|value| value.checked_add(1))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(Error::NumericOverflow)?;
        count.checked_mul(axis_count).ok_or(Error::NumericOverflow)
    })?;
    if count > limit {
        return Err(Error::CellEnumerationLimit {
            requested: count,
            limit,
        });
    }
    let mut cells = Vec::new();
    cells
        .try_reserve_exact(usize::try_from(count).map_err(|_| Error::NumericOverflow)?)
        .map_err(|_| Error::NumericOverflow)?;
    for x in minimum[0]..=maximum[0] {
        for y in minimum[1]..=maximum[1] {
            for z in minimum[2]..=maximum[2] {
                cells.push(WorldCellKey::new(x, y, z, level)?);
            }
        }
    }
    Ok(cells)
}

impl fmt::Display for WorldCellKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{},{},{}",
            self.level, self.coordinates[0], self.coordinates[1], self.coordinates[2]
        )
    }
}

impl FromStr for WorldCellKey {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let (level, coordinates) = value.split_once(':').ok_or(Error::InvalidCellEncoding)?;
        let level = level
            .parse::<u8>()
            .map_err(|_| Error::InvalidCellEncoding)?;
        let mut parts = coordinates.split(',');
        let parse_axis = |part: Option<&str>| {
            part.ok_or(Error::InvalidCellEncoding)?
                .parse::<i64>()
                .map_err(|_| Error::InvalidCellEncoding)
        };
        let x = parse_axis(parts.next())?;
        let y = parse_axis(parts.next())?;
        let z = parse_axis(parts.next())?;
        if parts.next().is_some() {
            return Err(Error::InvalidCellEncoding);
        }
        Self::new(x, y, z, level)
    }
}

/// A quantized position inside the half-open level-zero cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuantizedLocalPosition([u32; 3]);

impl QuantizedLocalPosition {
    /// Constructs checked half-open cell-local ticks.
    pub fn new(ticks: [u32; 3]) -> Result<Self> {
        if ticks.iter().any(|&value| value >= BASE_CELL_TICKS) {
            return Err(Error::LocalPositionRange);
        }
        Ok(Self(ticks))
    }

    /// The three cell-local tick values.
    #[must_use]
    pub fn ticks(self) -> [u32; 3] {
        self.0
    }
}

/// A serialized world position with exact global ownership and quantized local precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorldPosition {
    cell: WorldCellKey,
    local: QuantizedLocalPosition,
}

impl WorldPosition {
    /// Constructs a position from a level-zero owner and half-open local ticks.
    pub fn new(cell: WorldCellKey, local: QuantizedLocalPosition) -> Result<Self> {
        if cell.level != 0 {
            return Err(Error::PositionCellLevel);
        }
        Ok(Self { cell, local })
    }

    /// The exact origin.
    #[must_use]
    pub fn origin() -> Self {
        Self {
            cell: WorldCellKey::base(0, 0, 0),
            local: QuantizedLocalPosition([0, 0, 0]),
        }
    }

    /// Converts exact global ticks into the canonical half-open owner cell and local position.
    pub fn from_global_ticks(ticks: [i128; 3]) -> Result<Self> {
        let mut cell = [0_i64; 3];
        let mut local = [0_u32; 3];
        for axis in 0..3 {
            let owner = ticks[axis].div_euclid(i128::from(BASE_CELL_TICKS));
            cell[axis] = i64::try_from(owner).map_err(|_| Error::NumericOverflow)?;
            local[axis] = u32::try_from(ticks[axis].rem_euclid(i128::from(BASE_CELL_TICKS)))
                .map_err(|_| Error::NumericOverflow)?;
        }
        Self::new(
            WorldCellKey::base(cell[0], cell[1], cell[2]),
            QuantizedLocalPosition::new(local)?,
        )
    }

    /// Quantizes global metre coordinates with round-to-nearest, ties-to-even.
    pub fn from_world_meters(position: DVec3) -> Result<Self> {
        if !position.is_finite() {
            return Err(Error::NonFinite);
        }
        let mut ticks = [0_i128; 3];
        for (axis, value) in position.to_array().into_iter().enumerate() {
            let quantized = (value * f64::from(LOCAL_TICKS_PER_METER)).round_ties_even();
            if quantized < i128::MIN as f64 || quantized > i128::MAX as f64 {
                return Err(Error::NumericOverflow);
            }
            ticks[axis] = quantized as i128;
        }
        Self::from_global_ticks(ticks)
    }

    /// The exact global tick coordinates.
    #[must_use]
    pub fn global_ticks(self) -> [i128; 3] {
        let cell = self.cell.coordinates;
        let local = self.local.0;
        std::array::from_fn(|axis| {
            i128::from(cell[axis]) * i128::from(BASE_CELL_TICKS) + i128::from(local[axis])
        })
    }

    /// Converts to global metre coordinates. Serialized identity remains the integer form.
    #[must_use]
    pub fn world_meters(self) -> DVec3 {
        let ticks = self.global_ticks();
        DVec3::new(ticks[0] as f64, ticks[1] as f64, ticks[2] as f64)
            / f64::from(LOCAL_TICKS_PER_METER)
    }

    /// Converts to an `f32` render-relative position around an exact origin.
    pub fn to_render_relative(self, origin: Self) -> Result<Vec3> {
        let position = self.global_ticks();
        let origin = origin.global_ticks();
        let value = DVec3::new(
            (position[0] - origin[0]) as f64,
            (position[1] - origin[1]) as f64,
            (position[2] - origin[2]) as f64,
        ) / f64::from(LOCAL_TICKS_PER_METER);
        let result = value.as_vec3();
        if !result.is_finite() {
            return Err(Error::NumericOverflow);
        }
        Ok(result)
    }

    /// Quantizes an `f32` render-relative point around an exact origin.
    pub fn from_render_relative(position: Vec3, origin: Self) -> Result<Self> {
        if !position.is_finite() {
            return Err(Error::NonFinite);
        }
        origin.offset_meters(position.as_dvec3())
    }

    /// Adds a finite world-space metre offset and quantizes it with ties-to-even.
    pub fn offset_meters(self, offset: DVec3) -> Result<Self> {
        if !offset.is_finite() {
            return Err(Error::NonFinite);
        }
        let mut ticks = self.global_ticks();
        for (axis, value) in offset.to_array().into_iter().enumerate() {
            let delta = (value * f64::from(LOCAL_TICKS_PER_METER)).round_ties_even();
            if delta < i128::MIN as f64 || delta > i128::MAX as f64 {
                return Err(Error::NumericOverflow);
            }
            ticks[axis] = ticks[axis]
                .checked_add(delta as i128)
                .ok_or(Error::NumericOverflow)?;
        }
        Self::from_global_ticks(ticks)
    }

    /// The canonical level-zero owner cell.
    #[must_use]
    pub fn cell(self) -> WorldCellKey {
        self.cell
    }

    /// The canonical half-open local ticks.
    #[must_use]
    pub fn local(self) -> QuantizedLocalPosition {
        self.local
    }
}

impl Default for WorldPosition {
    fn default() -> Self {
        Self::origin()
    }
}

/// An exact half-open axis-aligned world box in global position ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldBounds {
    min_ticks: [i128; 3],
    max_ticks_exclusive: [i128; 3],
}

impl WorldBounds {
    /// Constructs a non-empty half-open box.
    pub fn new(min_ticks: [i128; 3], max_ticks_exclusive: [i128; 3]) -> Result<Self> {
        if (0..3).any(|axis| min_ticks[axis] >= max_ticks_exclusive[axis]) {
            return Err(Error::LocalPositionRange);
        }
        Ok(Self {
            min_ticks,
            max_ticks_exclusive,
        })
    }

    /// Quantizes metre bounds outward so every covered point remains inside the half-open box.
    pub fn from_world_meters(minimum: DVec3, maximum: DVec3) -> Result<Self> {
        if !minimum.is_finite() || !maximum.is_finite() {
            return Err(Error::NonFinite);
        }
        let mut min_ticks = [0_i128; 3];
        let mut max_ticks = [0_i128; 3];
        for axis in 0..3 {
            let min = (minimum[axis] * f64::from(LOCAL_TICKS_PER_METER)).floor();
            let max = (maximum[axis] * f64::from(LOCAL_TICKS_PER_METER)).ceil();
            if min < i128::MIN as f64
                || min > i128::MAX as f64
                || max < i128::MIN as f64
                || max > i128::MAX as f64
            {
                return Err(Error::NumericOverflow);
            }
            min_ticks[axis] = min as i128;
            max_ticks[axis] = max as i128;
            if min_ticks[axis] == max_ticks[axis] {
                max_ticks[axis] = max_ticks[axis]
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
        Self::new(min_ticks, max_ticks)
    }

    /// Quantizes render-relative metre bounds outward around an exact world origin.
    pub fn from_render_relative(
        minimum: DVec3,
        maximum: DVec3,
        render_origin: WorldPosition,
    ) -> Result<Self> {
        if !minimum.is_finite() || !maximum.is_finite() {
            return Err(Error::NonFinite);
        }
        let origin = render_origin.global_ticks();
        let mut min_ticks = [0_i128; 3];
        let mut max_ticks = [0_i128; 3];
        for axis in 0..3 {
            let minimum_delta = (minimum[axis] * f64::from(LOCAL_TICKS_PER_METER)).floor();
            let maximum_delta = (maximum[axis] * f64::from(LOCAL_TICKS_PER_METER)).ceil();
            if minimum_delta < i128::MIN as f64
                || minimum_delta > i128::MAX as f64
                || maximum_delta < i128::MIN as f64
                || maximum_delta > i128::MAX as f64
            {
                return Err(Error::NumericOverflow);
            }
            min_ticks[axis] = origin[axis]
                .checked_add(minimum_delta as i128)
                .ok_or(Error::NumericOverflow)?;
            max_ticks[axis] = origin[axis]
                .checked_add(maximum_delta as i128)
                .ok_or(Error::NumericOverflow)?;
            if min_ticks[axis] == max_ticks[axis] {
                max_ticks[axis] = max_ticks[axis]
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
        Self::new(min_ticks, max_ticks)
    }

    /// Minimum included tick coordinates.
    #[must_use]
    pub fn min_ticks(self) -> [i128; 3] {
        self.min_ticks
    }

    /// Maximum excluded tick coordinates.
    #[must_use]
    pub fn max_ticks_exclusive(self) -> [i128; 3] {
        self.max_ticks_exclusive
    }

    /// Whether the box contains the quantized position under half-open ownership.
    #[must_use]
    pub fn contains(self, position: WorldPosition) -> bool {
        let ticks = position.global_ticks();
        (0..3).all(|axis| {
            ticks[axis] >= self.min_ticks[axis] && ticks[axis] < self.max_ticks_exclusive[axis]
        })
    }

    /// The smallest box containing both inputs.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            min_ticks: std::array::from_fn(|axis| self.min_ticks[axis].min(other.min_ticks[axis])),
            max_ticks_exclusive: std::array::from_fn(|axis| {
                self.max_ticks_exclusive[axis].max(other.max_ticks_exclusive[axis])
            }),
        }
    }
}

const fn zigzag_encode(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}

const fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bytes_round_trip_boundaries() {
        let keys = [
            WorldCellKey::base(i64::MIN, -1, 0),
            WorldCellKey::base(1, i64::MAX, -42),
            WorldCellKey::new(-2, 0, 1, 62).unwrap(),
        ];
        for key in keys {
            assert_eq!(
                WorldCellKey::from_canonical_bytes(key.canonical_bytes()).unwrap(),
                key
            );
            assert_eq!(key.to_string().parse::<WorldCellKey>().unwrap(), key);
        }
    }

    #[test]
    fn every_level_boundary_key_round_trips_and_orders_big_endian() {
        for level in 0..=MAX_HIERARCHY_LEVEL {
            let extent = 1_i128 << (63 - level);
            let minimum = i64::try_from(-extent).unwrap();
            let maximum = i64::try_from(extent - 1).unwrap();
            let coordinates = [
                minimum,
                minimum.saturating_add(1),
                -2_i64.max(minimum),
                -1_i64.max(minimum),
                0_i64.min(maximum),
                1_i64.min(maximum),
                maximum.saturating_sub(1),
                maximum,
            ];
            for x in coordinates {
                for y in coordinates {
                    let key = WorldCellKey::new(x, y, maximum, level).unwrap();
                    assert_eq!(
                        WorldCellKey::from_canonical_bytes(key.canonical_bytes()).unwrap(),
                        key
                    );
                }
            }
            if level > 0 {
                assert!(WorldCellKey::new(minimum - 1, 0, 0, level).is_err());
                assert!(WorldCellKey::new(maximum + 1, 0, 0, level).is_err());
            }
        }
        assert_eq!(WorldCellKey::base(0, 0, 0).canonical_bytes(), [0; 25]);
        let mut negative_one = [0_u8; 25];
        negative_one[24] = 0b0000_0111;
        assert_eq!(
            WorldCellKey::base(-1, -1, -1).canonical_bytes(),
            negative_one
        );
    }

    #[test]
    fn global_position_domain_has_exact_half_open_owners() {
        let edge = i128::from(BASE_CELL_TICKS);
        let minimum = i128::from(i64::MIN) * edge;
        let maximum = i128::from(i64::MAX) * edge + edge - 1;
        for tick in [
            minimum,
            minimum + 1,
            -edge - 1,
            -edge,
            -1,
            0,
            1,
            edge - 1,
            edge,
            edge + 1,
            maximum - 1,
            maximum,
        ] {
            let position = WorldPosition::from_global_ticks([tick, tick, tick]).unwrap();
            assert_eq!(position.global_ticks(), [tick, tick, tick]);
            assert!(
                position
                    .local()
                    .ticks()
                    .iter()
                    .all(|value| *value < BASE_CELL_TICKS)
            );
        }
        assert!(WorldPosition::from_global_ticks([minimum - 1, 0, 0]).is_err());
        assert!(WorldPosition::from_global_ticks([maximum + 1, 0, 0]).is_err());
    }

    #[test]
    fn covering_cells_are_exact_for_negative_half_open_bounds_and_limits() {
        let edge = i128::from(BASE_CELL_TICKS);
        let bounds = WorldBounds::new([-edge, 0, -edge], [edge, edge, edge]).unwrap();
        assert_eq!(
            world_cells_covering_bounds(bounds, 0, 4).unwrap(),
            vec![
                WorldCellKey::base(-1, 0, -1),
                WorldCellKey::base(-1, 0, 0),
                WorldCellKey::base(0, 0, -1),
                WorldCellKey::base(0, 0, 0),
            ]
        );
        assert_eq!(
            world_cells_covering_bounds(bounds, 0, 3),
            Err(Error::CellEnumerationLimit {
                requested: 4,
                limit: 3,
            })
        );
    }

    #[test]
    fn minimum_descendant_preserves_the_ancestor_domain() {
        let ancestor = WorldCellKey::new(-2, 3, -1, 4).unwrap();
        let descendant = ancestor.minimum_descendant(1).unwrap();
        assert_eq!(descendant, WorldCellKey::new(-16, 24, -8, 1).unwrap());
        assert_eq!(descendant.ancestor(4).unwrap(), ancestor);
        assert_eq!(ancestor.minimum_descendant(4).unwrap(), ancestor);
        assert!(WorldCellKey::base(0, 0, 0).minimum_descendant(1).is_err());
    }

    #[test]
    fn negative_parent_uses_floor_division() {
        let key = WorldCellKey::base(-1, -2, -3);
        assert_eq!(key.parent().unwrap().coordinates(), [-1, -1, -2]);
        assert_eq!(key.ancestor(2).unwrap().coordinates(), [-1, -1, -1]);
    }

    #[test]
    fn every_child_returns_to_its_parent() {
        for level in 1..=12 {
            let parent = WorldCellKey::new(-7, 3, 11, level).unwrap();
            for x in 0..=1 {
                for y in 0..=1 {
                    for z in 0..=1 {
                        assert_eq!(parent.child([x, y, z]).unwrap().parent().unwrap(), parent);
                    }
                }
            }
        }
    }

    #[test]
    fn cell_faces_have_one_half_open_owner() {
        let face = WorldPosition::from_global_ticks([i128::from(BASE_CELL_TICKS), 0, 0]).unwrap();
        assert_eq!(face.cell().coordinates(), [1, 0, 0]);
        assert_eq!(face.local().ticks(), [0, 0, 0]);
        let negative = WorldPosition::from_global_ticks([-1, 0, 0]).unwrap();
        assert_eq!(negative.cell().coordinates(), [-1, 0, 0]);
        assert_eq!(negative.local().ticks()[0], BASE_CELL_TICKS - 1);
    }

    #[test]
    fn origin_rebasing_preserves_identity() {
        let world = WorldPosition::from_world_meters(DVec3::new(-64.25, 17.5, 999.0)).unwrap();
        let origin = WorldPosition::from_world_meters(DVec3::new(-128.0, 16.0, 960.0)).unwrap();
        let relative = world.to_render_relative(origin).unwrap();
        assert_eq!(
            WorldPosition::from_render_relative(relative, origin).unwrap(),
            world
        );
    }

    #[test]
    fn render_relative_bounds_preserve_sub_tick_origin() {
        let origin = WorldPosition::from_global_ticks([1_000_000_001, -17, 42]).unwrap();
        let bounds = WorldBounds::from_render_relative(
            DVec3::new(-1.0, -0.5, 0.0),
            DVec3::new(1.0, 0.5, 2.0),
            origin,
        )
        .unwrap();
        assert_eq!(bounds.min_ticks()[0], origin.global_ticks()[0] - 4096);
        assert_eq!(
            bounds.max_ticks_exclusive()[0],
            origin.global_ticks()[0] + 4096
        );
        assert!(bounds.contains(origin));
    }

    #[test]
    fn ties_round_to_even_ticks() {
        let half_tick = 0.5 / f64::from(LOCAL_TICKS_PER_METER);
        assert_eq!(
            WorldPosition::from_world_meters(DVec3::splat(half_tick))
                .unwrap()
                .global_ticks(),
            [0, 0, 0]
        );
        let one_and_half = 1.5 / f64::from(LOCAL_TICKS_PER_METER);
        assert_eq!(
            WorldPosition::from_world_meters(DVec3::splat(one_and_half))
                .unwrap()
                .global_ticks(),
            [2, 2, 2]
        );
    }
}
