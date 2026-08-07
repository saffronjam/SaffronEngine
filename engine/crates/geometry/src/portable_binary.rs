//! Bounded canonical big-endian primitives for the portable hierarchy codec.

use crate::{Error, Result};

pub(crate) struct BinaryWriter {
    bytes: Vec<u8>,
}

impl BinaryWriter {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
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

    pub(crate) fn length(&mut self, value: usize) -> Result<()> {
        self.u64(u64::try_from(value).map_err(|_| Error::NumericOverflow)?);
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
            Err(Error::HierarchyFormat {
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
        let bytes = self.bytes.get(self.cursor..end).ok_or(Error::Truncated)?;
        self.cursor = end;
        Ok(bytes)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| Error::Truncated)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::HierarchyFormat {
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

    pub(crate) fn length(&mut self) -> Result<usize> {
        let value = usize::try_from(self.u64()?).map_err(|_| Error::NumericOverflow)?;
        if value > self.remaining() {
            return Err(Error::Truncated);
        }
        Ok(value)
    }

    pub(crate) fn count(&mut self, minimum_item_bytes: usize) -> Result<usize> {
        let count = usize::try_from(self.u64()?).map_err(|_| Error::NumericOverflow)?;
        if minimum_item_bytes != 0 && count > self.remaining() / minimum_item_bytes {
            return Err(Error::Truncated);
        }
        Ok(count)
    }
}
