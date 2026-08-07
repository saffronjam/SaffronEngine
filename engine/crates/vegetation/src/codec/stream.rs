//! The authored-document byte stream: `u32` lengths, big-endian scalars, and a header
//! carrying the magic, the format version, and the schema hash.

use saffron_core::Uuid;
use saffron_json::{Value, dump_json_sorted, parse_json};
use saffron_spatial::{
    DecisionScalar, FieldChannel, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};

use crate::binary::{field_channel_from_tag, field_channel_tag};
use crate::{Error, PlantId, Result};

pub(super) fn invalid_enum(format: &'static str, field: &str) -> Error {
    Error::InvalidFormat {
        format,
        field: field.to_owned(),
    }
}

pub(super) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(super) fn with_header(magic: &[u8; 8], version: u32, schema: [u8; 32]) -> Self {
        let mut writer = Self { bytes: Vec::new() };
        writer.bytes(magic);
        writer.u32(version);
        writer.bytes(&schema);
        writer
    }

    pub(super) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub(super) fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(super) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    pub(super) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(super) fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn i16(&mut self, value: i16) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn i32(&mut self, value: i32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn u128(&mut self, value: u128) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn i128(&mut self, value: i128) {
        self.bytes(&value.to_be_bytes());
    }

    pub(super) fn uuid(&mut self, value: Uuid) {
        self.u64(value.value());
    }

    pub(super) fn plant_id(&mut self, value: PlantId) {
        self.bytes(&value.bytes());
    }

    pub(super) fn cell(&mut self, value: WorldCellKey) {
        self.bytes(&value.canonical_bytes());
    }

    pub(super) fn position(&mut self, value: WorldPosition) {
        for tick in value.global_ticks() {
            self.i128(tick);
        }
    }

    pub(super) fn bounds(&mut self, value: WorldBounds) {
        for tick in value.min_ticks() {
            self.i128(tick);
        }
        for tick in value.max_ticks_exclusive() {
            self.i128(tick);
        }
    }

    pub(super) fn fixed(&mut self, value: DecisionScalar) {
        self.bytes(&value.canonical_bytes());
    }

    pub(super) fn fixed2(&mut self, value: [DecisionScalar; 2]) {
        for scalar in value {
            self.fixed(scalar);
        }
    }

    pub(super) fn fixed3(&mut self, value: [DecisionScalar; 3]) {
        for scalar in value {
            self.fixed(scalar);
        }
    }

    pub(super) fn unit(&mut self, value: UnitInterval) {
        self.u16(value.bits());
    }

    pub(super) fn string(&mut self, value: &str) -> Result<()> {
        self.u32(u32::try_from(value.len()).map_err(|_| Error::NumericOverflow)?);
        self.bytes(value.as_bytes());
        Ok(())
    }

    pub(super) fn value(&mut self, value: &Value) -> Result<()> {
        self.string(&dump_json_sorted(value, -1))
    }

    pub(super) fn vec<T>(
        &mut self,
        values: &[T],
        mut write: impl FnMut(&mut Self, &T) -> Result<()>,
    ) -> Result<()> {
        self.u32(u32::try_from(values.len()).map_err(|_| Error::NumericOverflow)?);
        for value in values {
            write(self, value)?;
        }
        Ok(())
    }

    pub(super) fn option<T>(
        &mut self,
        value: Option<T>,
        write: impl FnOnce(&mut Self, T) -> Result<()>,
    ) -> Result<()> {
        match value {
            Some(value) => {
                self.bool(true);
                write(self, value)
            }
            None => {
                self.bool(false);
                Ok(())
            }
        }
    }

    pub(super) fn field_channel(&mut self, channel: FieldChannel) {
        let (tag, user) = field_channel_tag(channel);
        self.u8(tag);
        self.u64(user);
    }
}

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    format: &'static str,
}

impl<'a> Reader<'a> {
    pub(super) fn with_header(
        bytes: &'a [u8],
        magic: &[u8; 8],
        expected_version: u32,
        expected_schema: [u8; 32],
        format: &'static str,
    ) -> Result<Self> {
        let mut reader = Self {
            bytes,
            cursor: 0,
            format,
        };
        if reader.take(8)? != magic {
            return Err(reader.invalid("magic"));
        }
        let found = reader.u32()?;
        if found != expected_version {
            return Err(Error::FormatVersion {
                format,
                found,
                expected: expected_version,
            });
        }
        if reader.take(32)? != expected_schema {
            return Err(reader.invalid("schemaHash"));
        }
        Ok(reader)
    }

    pub(super) fn complete(&self) -> Result<()> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(self.invalid("trailingBytes"))
        }
    }

    pub(super) fn invalid(&self, field: &str) -> Error {
        Error::InvalidFormat {
            format: self.format,
            field: field.to_owned(),
        }
    }

    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(Error::NumericOverflow)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or_else(|| self.invalid("truncated"))?;
        self.cursor = end;
        Ok(value)
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| self.invalid("array"))
    }

    pub(super) fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.invalid("bool")),
        }
    }

    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(super) fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(super) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(super) fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub(super) fn i128(&mut self) -> Result<i128> {
        Ok(i128::from_be_bytes(self.array()?))
    }

    pub(super) fn uuid(&mut self) -> Result<Uuid> {
        Ok(Uuid(self.u64()?))
    }

    pub(super) fn plant_id(&mut self) -> Result<PlantId> {
        PlantId::from_canonical_bytes(self.array()?).map_err(|_| Error::InvalidPlantId)
    }

    pub(super) fn cell(&mut self) -> Result<WorldCellKey> {
        Ok(WorldCellKey::from_canonical_bytes(self.array()?)?)
    }

    pub(super) fn position(&mut self) -> Result<WorldPosition> {
        Ok(WorldPosition::from_global_ticks([
            self.i128()?,
            self.i128()?,
            self.i128()?,
        ])?)
    }

    pub(super) fn bounds(&mut self) -> Result<WorldBounds> {
        Ok(WorldBounds::new(
            [self.i128()?, self.i128()?, self.i128()?],
            [self.i128()?, self.i128()?, self.i128()?],
        )?)
    }

    pub(super) fn fixed(&mut self) -> Result<DecisionScalar> {
        Ok(DecisionScalar::from_bits(self.i32()?))
    }

    pub(super) fn fixed2(&mut self) -> Result<[DecisionScalar; 2]> {
        Ok([self.fixed()?, self.fixed()?])
    }

    pub(super) fn fixed3(&mut self) -> Result<[DecisionScalar; 3]> {
        Ok([self.fixed()?, self.fixed()?, self.fixed()?])
    }

    pub(super) fn unit(&mut self) -> Result<UnitInterval> {
        Ok(UnitInterval::from_bits(self.u16()?))
    }

    pub(super) fn string(&mut self) -> Result<String> {
        let length = usize::try_from(self.u32()?).map_err(|_| Error::NumericOverflow)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| self.invalid("utf8"))
    }

    pub(super) fn value(&mut self) -> Result<Value> {
        Ok(parse_json(&self.string()?)?)
    }

    pub(super) fn vec<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        let length = usize::try_from(self.u32()?).map_err(|_| Error::NumericOverflow)?;
        let mut values = Vec::with_capacity(length);
        for _ in 0..length {
            values.push(read(self)?);
        }
        Ok(values)
    }

    pub(super) fn option<T>(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<Option<T>> {
        if self.bool()? {
            Ok(Some(read(self)?))
        } else {
            Ok(None)
        }
    }

    pub(super) fn field_channel(&mut self) -> Result<FieldChannel> {
        let tag = self.u8()?;
        let user = self.u64()?;
        field_channel_from_tag(tag, user).ok_or_else(|| self.invalid("fieldChannel"))
    }
}
