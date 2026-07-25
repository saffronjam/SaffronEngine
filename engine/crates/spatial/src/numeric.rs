//! Canonical fixed-point, normalized, curve, and sort-key numerics.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use crate::{Error, Result};

/// Divides signed integers with round-to-nearest, ties-to-even.
pub fn div_round_ties_even(numerator: i128, denominator: i128) -> Result<i128> {
    if denominator == 0 {
        return Err(Error::DivisionByZero);
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder == 0 {
        return Ok(quotient);
    }
    let twice_remainder = remainder
        .abs()
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    let denominator_abs = denominator.abs();
    let direction = if (numerator < 0) ^ (denominator < 0) {
        -1
    } else {
        1
    };
    if twice_remainder > denominator_abs
        || (twice_remainder == denominator_abs && quotient & 1 != 0)
    {
        quotient
            .checked_add(direction)
            .ok_or(Error::NumericOverflow)
    } else {
        Ok(quotient)
    }
}

/// A checked signed fixed-point value with a compile-time fractional-bit count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FixedI32<const FRACTION_BITS: u32>(i32);

impl<const FRACTION_BITS: u32> FixedI32<FRACTION_BITS> {
    const fn scale_i128() -> i128 {
        assert!(FRACTION_BITS <= 30);
        1_i128 << FRACTION_BITS
    }

    /// Constructs a value from its canonical signed bits.
    #[must_use]
    pub const fn from_bits(bits: i32) -> Self {
        Self(bits)
    }

    /// The canonical signed bits.
    #[must_use]
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// Constructs an exact integer value.
    pub fn from_integer(value: i32) -> Result<Self> {
        let scaled = i128::from(value)
            .checked_mul(Self::scale_i128())
            .ok_or(Error::NumericOverflow)?;
        i32::try_from(scaled)
            .map(Self)
            .map_err(|_| Error::NumericOverflow)
    }

    /// Constructs an exact rational with round-to-nearest, ties-to-even.
    pub fn from_ratio(numerator: i64, denominator: i64) -> Result<Self> {
        let scaled = i128::from(numerator)
            .checked_mul(Self::scale_i128())
            .ok_or(Error::NumericOverflow)?;
        let rounded = div_round_ties_even(scaled, i128::from(denominator))?;
        i32::try_from(rounded)
            .map(Self)
            .map_err(|_| Error::NumericOverflow)
    }

    /// Quantizes a finite float with round-to-nearest, ties-to-even.
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::NonFinite);
        }
        let scaled = (value * Self::scale_i128() as f64).round_ties_even();
        if scaled < f64::from(i32::MIN) || scaled > f64::from(i32::MAX) {
            return Err(Error::NumericOverflow);
        }
        Ok(Self(scaled as i32))
    }

    /// Converts to `f64` for presentation or non-authoritative calculations.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / Self::scale_i128() as f64
    }

    /// Checked addition.
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(Error::NumericOverflow)
    }

    /// Checked subtraction.
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(Error::NumericOverflow)
    }

    /// Checked fixed-point multiplication with round-to-nearest, ties-to-even.
    pub fn checked_mul(self, other: Self) -> Result<Self> {
        let product = i128::from(self.0) * i128::from(other.0);
        let rounded = div_round_ties_even(product, Self::scale_i128())?;
        i32::try_from(rounded)
            .map(Self)
            .map_err(|_| Error::NumericOverflow)
    }

    /// Checked fixed-point division with round-to-nearest, ties-to-even.
    pub fn checked_div(self, other: Self) -> Result<Self> {
        let numerator = i128::from(self.0)
            .checked_mul(Self::scale_i128())
            .ok_or(Error::NumericOverflow)?;
        let rounded = div_round_ties_even(numerator, i128::from(other.0))?;
        i32::try_from(rounded)
            .map(Self)
            .map_err(|_| Error::NumericOverflow)
    }

    /// Linear interpolation with a canonical normalized weight.
    pub fn lerp(self, end: Self, weight: UnitInterval) -> Result<Self> {
        let delta = i128::from(end.0) - i128::from(self.0);
        let weighted = div_round_ties_even(delta * i128::from(weight.0), u16::MAX.into())?;
        let result = i128::from(self.0)
            .checked_add(weighted)
            .ok_or(Error::NumericOverflow)?;
        i32::try_from(result)
            .map(Self)
            .map_err(|_| Error::NumericOverflow)
    }

    /// Big-endian canonical bytes.
    #[must_use]
    pub const fn canonical_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// The Q15.16 decision scalar used by authoritative placement and lifecycle rules.
pub type DecisionScalar = FixedI32<16>;

/// A three-axis decision vector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DecisionVec3 {
    /// X component.
    pub x: DecisionScalar,
    /// Y component.
    pub y: DecisionScalar,
    /// Z component.
    pub z: DecisionScalar,
}

/// A symmetric three-dimensional Hessian in canonical component order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DecisionHessian3 {
    /// Second derivative along X.
    pub xx: DecisionScalar,
    /// Mixed X/Y derivative.
    pub xy: DecisionScalar,
    /// Mixed X/Z derivative.
    pub xz: DecisionScalar,
    /// Second derivative along Y.
    pub yy: DecisionScalar,
    /// Mixed Y/Z derivative.
    pub yz: DecisionScalar,
    /// Second derivative along Z.
    pub zz: DecisionScalar,
}

/// A canonical closed `[0, 1]` value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitInterval(u16);

impl UnitInterval {
    /// Zero.
    pub const ZERO: Self = Self(0);
    /// One.
    pub const ONE: Self = Self(u16::MAX);

    /// Constructs from canonical bits.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Canonical bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Quantizes a finite float in `[0, 1]` using ties-to-even.
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::NonFinite);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::NormalizedRange);
        }
        Ok(Self((value * f64::from(u16::MAX)).round_ties_even() as u16))
    }

    /// Converts to presentation `f64`.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / f64::from(u16::MAX)
    }

    /// Big-endian canonical bytes.
    #[must_use]
    pub const fn canonical_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

/// A canonical closed `[-1, 1]` value using symmetric `±32767` endpoints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignedUnit(i16);

impl SignedUnit {
    /// Constructs from canonical bits. `i16::MIN` is rejected to keep symmetric endpoints.
    pub fn from_bits(bits: i16) -> Result<Self> {
        if bits == i16::MIN {
            return Err(Error::NormalizedRange);
        }
        Ok(Self(bits))
    }

    /// Canonical bits.
    #[must_use]
    pub const fn bits(self) -> i16 {
        self.0
    }

    /// Quantizes a finite float in `[-1, 1]` using ties-to-even.
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::NonFinite);
        }
        if !(-1.0..=1.0).contains(&value) {
            return Err(Error::NormalizedRange);
        }
        Ok(Self((value * f64::from(i16::MAX)).round_ties_even() as i16))
    }

    /// Converts to presentation `f64`.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / f64::from(i16::MAX)
    }
}

/// A quantized unit quaternion in canonical XYZW order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuantizedOrientation([SignedUnit; 4]);

impl QuantizedOrientation {
    /// Identity orientation.
    #[must_use]
    pub fn identity() -> Self {
        Self([
            SignedUnit::from_bits(0).expect("zero is a signed unit"),
            SignedUnit::from_bits(0).expect("zero is a signed unit"),
            SignedUnit::from_bits(0).expect("zero is a signed unit"),
            SignedUnit::from_bits(i16::MAX).expect("positive one is a signed unit"),
        ])
    }

    /// Constructs a non-zero normalized quaternion within quantization tolerance.
    pub fn new(bits: [i16; 4]) -> Result<Self> {
        let lanes = bits
            .map(SignedUnit::from_bits)
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let length_squared: i64 = bits
            .into_iter()
            .map(|value| i64::from(value) * i64::from(value))
            .sum();
        let unit = i64::from(i16::MAX) * i64::from(i16::MAX);
        let tolerance = unit / 512;
        if length_squared.abs_diff(unit) > tolerance as u64 {
            return Err(Error::InvalidOrientation);
        }
        Ok(Self(
            lanes
                .try_into()
                .expect("four orientation lanes were collected"),
        ))
    }

    /// Canonical signed normalized lane bits.
    #[must_use]
    pub fn bits(self) -> [i16; 4] {
        self.0.map(SignedUnit::bits)
    }
}

impl Default for QuantizedOrientation {
    fn default() -> Self {
        Self::identity()
    }
}

/// A finite float with canonical zero and total ordering, for non-authoritative sort keys.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalF32(f32);

impl CanonicalF32 {
    /// Constructs a finite key and normalizes negative zero to positive zero.
    pub fn new(value: f32) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::NonFinite);
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    /// The finite value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// Big-endian IEEE bytes after zero canonicalization.
    #[must_use]
    pub const fn canonical_bytes(self) -> [u8; 4] {
        self.0.to_bits().to_be_bytes()
    }
}

impl PartialEq for CanonicalF32 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for CanonicalF32 {}

impl PartialOrd for CanonicalF32 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CanonicalF32 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for CanonicalF32 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// A canonical piecewise-linear decision curve with unique ascending abscissae.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionCurve {
    points: Vec<(UnitInterval, DecisionScalar)>,
}

impl DecisionCurve {
    /// Validates and stores a canonical curve. The caller must supply the stable order.
    pub fn new(points: Vec<(UnitInterval, DecisionScalar)>) -> Result<Self> {
        Self::validate_points(&points)?;
        Ok(Self { points })
    }

    /// Validates canonical borrowed curve points without taking ownership.
    pub fn validate_points(points: &[(UnitInterval, DecisionScalar)]) -> Result<()> {
        if points.is_empty() || points.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(Error::CurveOrder);
        }
        Ok(())
    }

    /// The canonical points.
    #[must_use]
    pub fn points(&self) -> &[(UnitInterval, DecisionScalar)] {
        &self.points
    }

    /// Samples with endpoint clamping and ties-to-even linear interpolation.
    pub fn sample(&self, x: UnitInterval) -> Result<DecisionScalar> {
        Self::sample_points(&self.points, x)
    }

    /// Samples validated borrowed curve points without allocating an owned curve.
    pub fn sample_points(
        points: &[(UnitInterval, DecisionScalar)],
        x: UnitInterval,
    ) -> Result<DecisionScalar> {
        Self::validate_points(points)?;
        if x <= points[0].0 {
            return Ok(points[0].1);
        }
        if x >= points[points.len() - 1].0 {
            return Ok(points[points.len() - 1].1);
        }
        let index = points.partition_point(|(point_x, _)| *point_x < x);
        let (x0, y0) = points[index - 1];
        let (x1, y1) = points[index];
        let numerator = u32::from(x.bits() - x0.bits());
        let denominator = u32::from(x1.bits() - x0.bits());
        let weight_bits = div_round_ties_even(
            i128::from(numerator) * i128::from(u16::MAX),
            i128::from(denominator),
        )?;
        y0.lerp(y1, UnitInterval::from_bits(weight_bits as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_round_to_even_for_both_signs() {
        assert_eq!(div_round_ties_even(5, 2).unwrap(), 2);
        assert_eq!(div_round_ties_even(7, 2).unwrap(), 4);
        assert_eq!(div_round_ties_even(-5, 2).unwrap(), -2);
        assert_eq!(div_round_ties_even(-7, 2).unwrap(), -4);
    }

    #[test]
    fn fixed_operations_are_checked_and_canonical() {
        let a = DecisionScalar::from_ratio(3, 2).unwrap();
        let b = DecisionScalar::from_ratio(2, 3).unwrap();
        assert_eq!(
            a.checked_mul(b).unwrap(),
            DecisionScalar::from_integer(1).unwrap()
        );
        assert_eq!(a.canonical_bytes(), a.bits().to_be_bytes());
        assert!(DecisionScalar::from_f64(f64::NAN).is_err());
        assert!(
            DecisionScalar::from_bits(i32::MAX)
                .checked_add(DecisionScalar::from_bits(1))
                .is_err()
        );
    }

    #[test]
    fn quantized_orientation_requires_a_unit_quaternion() {
        let identity = QuantizedOrientation::identity();
        assert_eq!(identity.bits(), [0, 0, 0, i16::MAX]);
        assert_eq!(
            QuantizedOrientation::new([0, 0, 0, i16::MAX]).unwrap(),
            identity
        );
        assert_eq!(
            QuantizedOrientation::new([0, 0, 0, 0]).unwrap_err(),
            Error::InvalidOrientation
        );
        assert_eq!(
            QuantizedOrientation::new([i16::MIN, 0, 0, 0]).unwrap_err(),
            Error::NormalizedRange
        );
    }

    #[test]
    fn curve_clamps_and_interpolates() {
        let curve = DecisionCurve::new(vec![
            (
                UnitInterval::ZERO,
                DecisionScalar::from_integer(-2).unwrap(),
            ),
            (UnitInterval::ONE, DecisionScalar::from_integer(2).unwrap()),
        ])
        .unwrap();
        assert_eq!(curve.sample(UnitInterval::ZERO).unwrap().to_f64(), -2.0);
        let middle = UnitInterval::from_bits(32768);
        assert!((curve.sample(middle).unwrap().to_f64()).abs() < 0.001);
    }

    #[test]
    fn canonical_float_rejects_non_finite_and_normalizes_zero() {
        assert!(CanonicalF32::new(f32::INFINITY).is_err());
        assert_eq!(
            CanonicalF32::new(-0.0).unwrap().canonical_bytes(),
            CanonicalF32::new(0.0).unwrap().canonical_bytes()
        );
    }
}
