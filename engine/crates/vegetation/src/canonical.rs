//! Allocation-aware sinks for one canonical encoding implementation.

use crate::{Error, Result};

pub(crate) trait CanonicalSink {
    fn write(&mut self, bytes: &[u8]) -> Result<()>;

    fn write_byte(&mut self, byte: u8) -> Result<()> {
        self.write(&[byte])
    }
}

pub(crate) struct ByteSink {
    bytes: Vec<u8>,
}

impl ByteSink {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl CanonicalSink for ByteSink {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

pub(crate) struct CountSink {
    bytes: usize,
}

impl CountSink {
    pub(crate) const fn new() -> Self {
        Self { bytes: 0 }
    }

    pub(crate) const fn finish(self) -> usize {
        self.bytes
    }
}

impl CanonicalSink for CountSink {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(Error::NumericOverflow)?;
        Ok(())
    }
}
