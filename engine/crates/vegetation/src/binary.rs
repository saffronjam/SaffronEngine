//! Shared canonical big-endian primitives for vegetation-derived formats.

use saffron_core::Uuid;
use saffron_spatial::{FieldChannel, WorldBounds, WorldCellKey};

use crate::{Error, Result};

/// The canonical `(tag, user)` pair every vegetation format writes for a field channel.
pub(crate) fn field_channel_tag(channel: FieldChannel) -> (u8, u64) {
    match channel {
        FieldChannel::Altitude => (0, 0),
        FieldChannel::Slope => (1, 0),
        FieldChannel::Curvature => (2, 0),
        FieldChannel::Concavity => (3, 0),
        FieldChannel::Drainage => (4, 0),
        FieldChannel::Moisture => (5, 0),
        FieldChannel::Temperature => (6, 0),
        FieldChannel::Precipitation => (7, 0),
        FieldChannel::Sunlight => (8, 0),
        FieldChannel::Exposure => (9, 0),
        FieldChannel::WaterDistance => (10, 0),
        FieldChannel::WaterDepth => (11, 0),
        FieldChannel::SignedBlocker => (12, 0),
        FieldChannel::SplineDistance => (13, 0),
        FieldChannel::User(value) => (14, value),
    }
}

pub(crate) fn field_channel_from_tag(tag: u8, user: u64) -> Option<FieldChannel> {
    Some(match (tag, user) {
        (0, 0) => FieldChannel::Altitude,
        (1, 0) => FieldChannel::Slope,
        (2, 0) => FieldChannel::Curvature,
        (3, 0) => FieldChannel::Concavity,
        (4, 0) => FieldChannel::Drainage,
        (5, 0) => FieldChannel::Moisture,
        (6, 0) => FieldChannel::Temperature,
        (7, 0) => FieldChannel::Precipitation,
        (8, 0) => FieldChannel::Sunlight,
        (9, 0) => FieldChannel::Exposure,
        (10, 0) => FieldChannel::WaterDistance,
        (11, 0) => FieldChannel::WaterDepth,
        (12, 0) => FieldChannel::SignedBlocker,
        (13, 0) => FieldChannel::SplineDistance,
        (14, value) => FieldChannel::User(value),
        _ => return None,
    })
}

pub(crate) struct BinaryWriter {
    bytes: Vec<u8>,
}

impl BinaryWriter {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn zeroes(&mut self, count: usize) -> Result<()> {
        let end = self
            .bytes
            .len()
            .checked_add(count)
            .ok_or(Error::NumericOverflow)?;
        self.bytes.resize(end, 0);
        Ok(())
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn i32(&mut self, value: i32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn u128(&mut self, value: u128) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn i128(&mut self, value: i128) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn uuid(&mut self, value: Uuid) {
        self.u64(value.value());
    }

    pub(crate) fn cell(&mut self, value: WorldCellKey) {
        self.bytes(&value.canonical_bytes());
    }

    pub(crate) fn bounds(&mut self, value: WorldBounds) {
        for tick in value.min_ticks() {
            self.i128(tick);
        }
        for tick in value.max_ticks_exclusive() {
            self.i128(tick);
        }
    }

    pub(crate) fn length(&mut self, value: usize) -> Result<()> {
        self.u64(u64::try_from(value).map_err(|_| Error::NumericOverflow)?);
        Ok(())
    }

    pub(crate) fn string(&mut self, value: &str) -> Result<()> {
        self.length(value.len())?;
        self.bytes(value.as_bytes());
        Ok(())
    }
}

pub(crate) struct BinaryReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    format: &'static str,
}

impl<'a> BinaryReader<'a> {
    pub(crate) fn new(bytes: &'a [u8], format: &'static str) -> Self {
        Self {
            bytes,
            cursor: 0,
            format,
        }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.cursor)
    }

    pub(crate) fn complete(&self) -> Result<()> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: self.format,
                field: "trailingBytes".to_owned(),
            })
        }
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(Error::NumericOverflow)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(Error::ArtifactTruncated {
                format: self.format,
            })?;
        self.cursor = end;
        Ok(bytes)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| Error::ArtifactTruncated {
                format: self.format,
            })
    }

    pub(crate) fn expect(&mut self, expected: &[u8], field: &str) -> Result<()> {
        if self.take(expected.len())? == expected {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: self.format,
                field: field.to_owned(),
            })
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::ArtifactFormat {
                format: self.format,
                field: "boolean".to_owned(),
            }),
        }
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub(crate) fn i128(&mut self) -> Result<i128> {
        Ok(i128::from_be_bytes(self.array()?))
    }

    pub(crate) fn uuid(&mut self) -> Result<Uuid> {
        Ok(Uuid(self.u64()?))
    }

    pub(crate) fn cell(&mut self) -> Result<WorldCellKey> {
        Ok(WorldCellKey::from_canonical_bytes(self.array()?)?)
    }

    pub(crate) fn bounds(&mut self) -> Result<WorldBounds> {
        Ok(WorldBounds::new(
            [self.i128()?, self.i128()?, self.i128()?],
            [self.i128()?, self.i128()?, self.i128()?],
        )?)
    }

    pub(crate) fn length(&mut self) -> Result<usize> {
        let value = usize::try_from(self.u64()?).map_err(|_| Error::NumericOverflow)?;
        if value > self.remaining() {
            return Err(Error::ArtifactTruncated {
                format: self.format,
            });
        }
        Ok(value)
    }

    pub(crate) fn count(&mut self, minimum_item_bytes: usize) -> Result<usize> {
        let count = usize::try_from(self.u64()?).map_err(|_| Error::NumericOverflow)?;
        if minimum_item_bytes != 0 && count > self.remaining() / minimum_item_bytes {
            return Err(Error::ArtifactTruncated {
                format: self.format,
            });
        }
        Ok(count)
    }

    pub(crate) fn string(&mut self) -> Result<String> {
        let length = self.length()?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| Error::ArtifactFormat {
            format: self.format,
            field: "utf8".to_owned(),
        })
    }
}
