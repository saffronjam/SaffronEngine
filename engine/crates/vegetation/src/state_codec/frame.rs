//! The outer frame every persistent container shares: magic, version, schema hash, a
//! length-prefixed payload, the payload digest, and a trailing commit marker that makes an
//! interrupted write detectable.

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{ContentHash, Error, Result};

pub(super) fn encode_frame(
    format: &'static str,
    magic: &[u8; 8],
    version: u32,
    schema: ContentHash,
    payload: &[u8],
    commit: &[u8; 8],
) -> Result<Vec<u8>> {
    if payload.is_empty() {
        return Err(Error::ArtifactFormat {
            format,
            field: "payload".to_owned(),
        });
    }
    let mut writer = BinaryWriter::with_capacity(
        8_usize
            .checked_add(4 + 32 + 8 + payload.len() + 32 + 8)
            .ok_or(Error::NumericOverflow)?,
    );
    writer.bytes(magic);
    writer.u32(version);
    writer.bytes(&schema.bytes());
    writer.length(payload.len())?;
    writer.bytes(payload);
    writer.bytes(&ContentHash::of(payload).bytes());
    writer.bytes(commit);
    Ok(writer.finish())
}

pub(super) fn decode_frame<'a>(
    bytes: &'a [u8],
    format: &'static str,
    magic: &[u8; 8],
    version: u32,
    schema: ContentHash,
    commit: &[u8; 8],
) -> Result<&'a [u8]> {
    let mut reader = BinaryReader::new(bytes, format);
    reader.expect(magic, "magic")?;
    let found = reader.u32()?;
    if found != version {
        return Err(Error::FormatVersion {
            format,
            found,
            expected: version,
        });
    }
    if ContentHash::new(reader.array()?) != schema {
        return Err(Error::ArtifactSchema { format });
    }
    let payload_length = reader.length()?;
    let payload = reader.take(payload_length)?;
    let payload_hash = ContentHash::new(reader.array()?);
    reader.expect(commit, "commitMarker")?;
    reader.complete()?;
    if payload_hash != ContentHash::of(payload) {
        return Err(Error::ArtifactHashMismatch {
            format,
            subject: "payload".to_owned(),
        });
    }
    Ok(payload)
}
